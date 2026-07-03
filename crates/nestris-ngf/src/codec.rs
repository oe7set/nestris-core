//! NGF frame codec.
//!
//! A frame is a fixed-size record whose size depends on the version stored
//! in the top 3 bits of the first byte: v1 = 71, v2 = 72, v3 = 73 bytes.
//! The decoder mirrors the reference Python parser
//! (`NestrisLTM/src/services/ngf_import_service.py`) expression by
//! expression; the v3 encoder is its exact inverse.
//!
//! Layout (v3, 73 bytes):
//!
//! ```text
//! byte 0        version(3) | game_type(2) | player_num(3)
//! bytes 1..3    gameid, u16 big-endian
//! bytes 3..7    ctime ms (28 bits) | lines high nibble (4 bits)
//! byte 7        lines low byte (lines = 12 bits total)
//! byte 8        level (u8)
//! bytes 9..12   score, 24-bit big-endian
//! byte 12       instant_das(5) | preview piece(3)
//! byte 13       cur_piece_das(5) | current piece(3)
//! bytes 14..23  piece counts T,J,Z,O,S,L,I; 10 bits each, MSB-first,
//!               2 trailing pad bits
//! bytes 23..73  playfield: 200 cells x 2 bits, MSB-first, row-major from
//!               the top-left; cell values 0=empty, 1=white, 2=accent A,
//!               3=accent B (the engine's stable cell ids verbatim)
//! ```
//!
//! Missing values use all-ones sentinels: score `0xFFFFFF`, lines `0xFFF`,
//! level `0xFF`, piece counts `0x3FF`, DAS `0x1F`, piece codes `0b111`.

use nestris_engine::enums::Piece;
use thiserror::Error;

/// NGF piece code order: T=0, J=1, Z=2, O=3, S=4, L=5, I=6, 7 = none.
pub const NGF_PIECES: [Piece; 7] = [
    Piece::T,
    Piece::J,
    Piece::Z,
    Piece::O,
    Piece::S,
    Piece::L,
    Piece::I,
];

pub const V1_FRAME_SIZE: usize = 71;
pub const V2_FRAME_SIZE: usize = 72;
pub const V3_FRAME_SIZE: usize = 73;

/// Frame size for a version byte's version bits, if the version is known.
pub fn frame_size(version: u8) -> Option<usize> {
    match version {
        1 => Some(V1_FRAME_SIZE),
        2 => Some(V2_FRAME_SIZE),
        3 => Some(V3_FRAME_SIZE),
        _ => None,
    }
}

const SENTINEL_SCORE_V2: u32 = 0xFF_FFFF;
const SENTINEL_SCORE_V1: u32 = 0x1F_FFFF;
const SENTINEL_LINES_V2: u16 = 0xFFF;
const SENTINEL_LINES_V1: u16 = 0x1FF;
const SENTINEL_LEVEL_V2: u8 = 0xFF;
const SENTINEL_LEVEL_V1: u8 = 0x3F;
const SENTINEL_COUNT_V3: u16 = 0x3FF;
const SENTINEL_COUNT_V2: u16 = 0x1FF;
const SENTINEL_COUNT_V1: u16 = 0xFF;
const SENTINEL_DAS: u8 = 0x1F;
const SENTINEL_PIECE: u8 = 0b111;

/// Maximum encodable values in v3 (larger inputs are clamped on encode so a
/// long game degrades gracefully instead of wrapping).
pub const MAX_CTIME_MS: u32 = (1 << 28) - 1;
pub const MAX_LINES: u16 = 0xFFE;
pub const MAX_SCORE: u32 = 0xFF_FFFE;
pub const MAX_COUNT: u16 = 0x3FE;

#[derive(Debug, Error, PartialEq)]
pub enum NgfError {
    #[error("unknown NGF frame version {0}")]
    UnknownVersion(u8),
    #[error("truncated NGF frame: need {expected} bytes, have {actual}")]
    Truncated { expected: usize, actual: usize },
}

/// One decoded (or to-be-encoded) NGF frame. Field cells are the engine's
/// stable ids (0 empty, 1 white, 2 accent A, 3 accent B).
#[derive(Clone, Debug, PartialEq)]
pub struct NgfFrame {
    pub version: u8,
    /// 0 = minimal, 1 = classic, 2 = DAS trainer (NestrisChamps game types).
    pub game_type: u8,
    pub player_num: u8,
    pub gameid: u16,
    /// Milliseconds since game start (28-bit).
    pub ctime_ms: u32,
    pub lines: Option<u16>,
    pub level: Option<u8>,
    pub score: Option<u32>,
    pub instant_das: Option<u8>,
    pub preview: Option<Piece>,
    pub cur_piece_das: Option<u8>,
    pub cur_piece: Option<Piece>,
    /// Piece counts in NGF order (T, J, Z, O, S, L, I).
    pub counts: [Option<u16>; 7],
    /// 200 cells, row-major from the top-left.
    pub field: [u8; 200],
}

impl Default for NgfFrame {
    fn default() -> Self {
        Self {
            version: 3,
            game_type: 1, // classic
            player_num: 0,
            gameid: 0,
            ctime_ms: 0,
            lines: None,
            level: None,
            score: None,
            instant_das: None,
            preview: None,
            cur_piece_das: None,
            cur_piece: None,
            counts: [None; 7],
            field: [0u8; 200],
        }
    }
}

fn piece_to_code(piece: Option<Piece>) -> u8 {
    piece
        .and_then(|p| NGF_PIECES.iter().position(|&n| n == p))
        .map(|i| i as u8)
        .unwrap_or(SENTINEL_PIECE)
}

fn code_to_piece(code: u8) -> Option<Piece> {
    NGF_PIECES.get(code as usize).copied()
}

/// Encode `frame` as a version-3 record, appending exactly
/// [`V3_FRAME_SIZE`] bytes to `out`. Out-of-range values are clamped.
pub fn encode_v3(frame: &NgfFrame, out: &mut Vec<u8>) {
    out.reserve(V3_FRAME_SIZE);
    let start = out.len();

    out.push(3 << 5 | (frame.game_type & 0b11) << 3 | (frame.player_num & 0b111));
    out.extend_from_slice(&frame.gameid.to_be_bytes());

    let ctime = frame.ctime_ms.min(MAX_CTIME_MS);
    let lines = frame
        .lines
        .map(|l| l.min(MAX_LINES))
        .unwrap_or(SENTINEL_LINES_V2);
    out.push((ctime >> 20) as u8);
    out.push((ctime >> 12) as u8);
    out.push((ctime >> 4) as u8);
    out.push((((ctime & 0x0F) as u8) << 4) | ((lines >> 8) as u8 & 0x0F));
    out.push((lines & 0xFF) as u8);

    out.push(frame.level.unwrap_or(SENTINEL_LEVEL_V2));

    let score = frame
        .score
        .map(|s| s.min(MAX_SCORE))
        .unwrap_or(SENTINEL_SCORE_V2);
    out.push((score >> 16) as u8);
    out.push((score >> 8) as u8);
    out.push(score as u8);

    let instant_das = frame.instant_das.map(|d| d.min(0x1E)).unwrap_or(SENTINEL_DAS);
    out.push(instant_das << 3 | piece_to_code(frame.preview));
    let cur_das = frame
        .cur_piece_das
        .map(|d| d.min(0x1E))
        .unwrap_or(SENTINEL_DAS);
    out.push(cur_das << 3 | piece_to_code(frame.cur_piece));

    // 7 counts x 10 bits, MSB-first, then 2 pad bits to the byte boundary.
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for &count in &frame.counts {
        let v = count.map(|c| c.min(MAX_COUNT)).unwrap_or(SENTINEL_COUNT_V3) as u32;
        acc = (acc << 10) | v;
        bits += 10;
        while bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    debug_assert_eq!(bits, 6); // 70 bits emitted as 8 bytes + 6 residual bits
    out.push((acc << (8 - bits)) as u8);

    // 200 cells x 2 bits, 4 per byte, MSB-first.
    for chunk in frame.field.chunks_exact(4) {
        out.push(
            (chunk[0] & 0b11) << 6 | (chunk[1] & 0b11) << 4 | (chunk[2] & 0b11) << 2
                | (chunk[3] & 0b11),
        );
    }

    debug_assert_eq!(out.len() - start, V3_FRAME_SIZE);
}

/// Decode one frame from the start of `bytes`. Returns the frame and the
/// number of bytes consumed.
pub fn decode_frame(bytes: &[u8]) -> Result<(NgfFrame, usize), NgfError> {
    let first = *bytes.first().ok_or(NgfError::Truncated {
        expected: 1,
        actual: 0,
    })?;
    let version = (first & 0b1110_0000) >> 5;
    let size = frame_size(version).ok_or(NgfError::UnknownVersion(version))?;
    if bytes.len() < size {
        return Err(NgfError::Truncated {
            expected: size,
            actual: bytes.len(),
        });
    }
    let f = &bytes[..size];

    let game_type = (first & 0b0001_1000) >> 3;
    let player_num = first & 0b111;
    let gameid = u16::from_be_bytes([f[1], f[2]]);
    let ctime_ms = (f[3] as u32) << 20 | (f[4] as u32) << 12 | (f[5] as u32) << 4
        | ((f[6] & 0xF0) as u32) >> 4;
    let lines_hi = (f[6] & 0x0F) as u16;

    let mut frame = NgfFrame {
        version,
        game_type,
        player_num,
        gameid,
        ctime_ms,
        ..NgfFrame::default()
    };

    let field_start;
    if version >= 2 {
        let lines = lines_hi << 8 | f[7] as u16;
        let level = f[8];
        let score = (f[9] as u32) << 16 | (f[10] as u32) << 8 | f[11] as u32;
        let instant_das = (f[12] & 0b1111_1000) >> 3;
        let preview = f[12] & 0b111;
        let cur_das = (f[13] & 0b1111_1000) >> 3;
        let cur_piece = f[13] & 0b111;

        let (counts, count_sentinel, after) = if version == 3 {
            let c = [
                (f[14] as u16) << 2 | ((f[15] & 0b1100_0000) as u16) >> 6,
                ((f[15] & 0b0011_1111) as u16) << 4 | ((f[16] & 0b1111_0000) as u16) >> 4,
                ((f[16] & 0b0000_1111) as u16) << 6 | ((f[17] & 0b1111_1100) as u16) >> 2,
                ((f[17] & 0b0000_0011) as u16) << 8 | f[18] as u16,
                (f[19] as u16) << 2 | ((f[20] & 0b1100_0000) as u16) >> 6,
                ((f[20] & 0b0011_1111) as u16) << 4 | ((f[21] & 0b1111_0000) as u16) >> 4,
                ((f[21] & 0b0000_1111) as u16) << 6 | ((f[22] & 0b1111_1100) as u16) >> 2,
            ];
            (c, SENTINEL_COUNT_V3, 23)
        } else {
            let c = [
                (f[14] as u16) << 1 | ((f[15] & 0b1000_0000) as u16) >> 7,
                ((f[15] & 0b0111_1111) as u16) << 2 | ((f[16] & 0b1100_0000) as u16) >> 6,
                ((f[16] & 0b0011_1111) as u16) << 3 | ((f[17] & 0b1110_0000) as u16) >> 5,
                ((f[17] & 0b0001_1111) as u16) << 4 | ((f[18] & 0b1111_0000) as u16) >> 4,
                ((f[18] & 0b0000_1111) as u16) << 5 | ((f[19] & 0b1111_1000) as u16) >> 3,
                ((f[19] & 0b0000_0111) as u16) << 6 | ((f[20] & 0b1111_1100) as u16) >> 2,
                ((f[20] & 0b0000_0011) as u16) << 7 | ((f[21] & 0b1111_1110) as u16) >> 1,
            ];
            (c, SENTINEL_COUNT_V2, 22)
        };

        frame.lines = (lines != SENTINEL_LINES_V2).then_some(lines);
        frame.level = (level != SENTINEL_LEVEL_V2).then_some(level);
        frame.score = (score != SENTINEL_SCORE_V2).then_some(score);
        frame.instant_das = (instant_das != SENTINEL_DAS).then_some(instant_das);
        frame.cur_piece_das = (cur_das != SENTINEL_DAS).then_some(cur_das);
        frame.preview = code_to_piece(preview);
        frame.cur_piece = code_to_piece(cur_piece);
        for (i, &c) in counts.iter().enumerate() {
            frame.counts[i] = (c != count_sentinel).then_some(c);
        }
        field_start = after;
    } else {
        // Version 1.
        let score = ((f[7] & 0x0F) as u32) << 17 | (f[8] as u32) << 9 | (f[9] as u32) << 1
            | ((f[10] & 0x80) as u32) >> 7;
        let lines = ((f[10] & 0x7F) as u16) << 2 | ((f[11] & 0xC0) as u16) >> 6;
        let level = f[11] & 0x3F;
        let instant_das = (f[12] & 0b1111_1000) >> 3;
        let preview = f[12] & 0b111;
        let cur_das = (f[13] & 0b1111_1000) >> 3;
        let cur_piece = f[13] & 0b111;

        frame.score = (score != SENTINEL_SCORE_V1).then_some(score);
        frame.lines = (lines != SENTINEL_LINES_V1).then_some(lines);
        frame.level = (level != SENTINEL_LEVEL_V1).then_some(level);
        frame.instant_das = (instant_das != SENTINEL_DAS).then_some(instant_das);
        frame.cur_piece_das = (cur_das != SENTINEL_DAS).then_some(cur_das);
        frame.preview = code_to_piece(preview);
        frame.cur_piece = code_to_piece(cur_piece);
        for i in 0..7 {
            let c = f[14 + i] as u16;
            frame.counts[i] = (c != SENTINEL_COUNT_V1).then_some(c);
        }
        field_start = 21;
    }

    for (i, &b) in f[field_start..field_start + 50].iter().enumerate() {
        frame.field[i * 4] = (b & 0b1100_0000) >> 6;
        frame.field[i * 4 + 1] = (b & 0b0011_0000) >> 4;
        frame.field[i * 4 + 2] = (b & 0b0000_1100) >> 2;
        frame.field[i * 4 + 3] = b & 0b11;
    }

    Ok((frame, size))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_frame() -> NgfFrame {
        let mut field = [0u8; 200];
        // A recognizable pattern: bottom row alternating ids, one cell at
        // the top-left corner.
        field[0] = 2;
        for c in 0..10 {
            field[190 + c] = ((c % 3) + 1) as u8;
        }
        NgfFrame {
            version: 3,
            game_type: 1,
            player_num: 0,
            gameid: 42,
            ctime_ms: 123_456,
            lines: Some(65),
            level: Some(10),
            score: Some(114_535),
            instant_das: None,
            preview: Some(Piece::S),
            cur_piece_das: None,
            cur_piece: Some(Piece::I),
            counts: [
                Some(24),
                Some(30),
                Some(18),
                Some(26),
                Some(22),
                Some(21),
                Some(24),
            ],
            field,
        }
    }

    #[test]
    fn v3_round_trip() {
        let frame = sample_frame();
        let mut buf = Vec::new();
        encode_v3(&frame, &mut buf);
        assert_eq!(buf.len(), V3_FRAME_SIZE);
        let (decoded, consumed) = decode_frame(&buf).unwrap();
        assert_eq!(consumed, V3_FRAME_SIZE);
        assert_eq!(decoded, frame);
    }

    #[test]
    fn v3_round_trip_all_sentinels() {
        let frame = NgfFrame {
            gameid: 1,
            ctime_ms: 0,
            ..NgfFrame::default()
        };
        let mut buf = Vec::new();
        encode_v3(&frame, &mut buf);
        let (decoded, _) = decode_frame(&buf).unwrap();
        assert_eq!(decoded, frame);
        assert_eq!(decoded.score, None);
        assert_eq!(decoded.preview, None);
        assert_eq!(decoded.counts, [None; 7]);
    }

    #[test]
    fn v3_round_trip_extremes() {
        let mut frame = sample_frame();
        frame.ctime_ms = MAX_CTIME_MS;
        frame.lines = Some(MAX_LINES);
        frame.score = Some(MAX_SCORE);
        frame.level = Some(254);
        frame.counts = [Some(MAX_COUNT); 7];
        frame.instant_das = Some(0x1E);
        frame.cur_piece_das = Some(0);
        let mut buf = Vec::new();
        encode_v3(&frame, &mut buf);
        let (decoded, _) = decode_frame(&buf).unwrap();
        assert_eq!(decoded, frame);
    }

    #[test]
    fn v3_clamps_out_of_range() {
        let mut frame = sample_frame();
        frame.score = Some(99_999_999);
        frame.counts[0] = Some(5000);
        let mut buf = Vec::new();
        encode_v3(&frame, &mut buf);
        let (decoded, _) = decode_frame(&buf).unwrap();
        assert_eq!(decoded.score, Some(MAX_SCORE));
        assert_eq!(decoded.counts[0], Some(MAX_COUNT));
    }

    /// Hand-computed byte fixture: independently derived from the reference
    /// parser's expressions, guarding both encoder and decoder against
    /// simultaneous, self-consistent bugs.
    #[test]
    fn v3_known_bytes() {
        let mut frame = NgfFrame {
            gameid: 0x0102,
            ctime_ms: 0x0ABCDEF,
            lines: Some(0x123),
            level: Some(0x14),
            score: Some(0x0567_89),
            instant_das: Some(0x0A),
            preview: Some(Piece::T),    // code 0
            cur_piece_das: Some(0x1E),
            cur_piece: Some(Piece::L), // code 5
            counts: [Some(1), Some(2), Some(3), Some(4), Some(5), Some(6), Some(7)],
            ..NgfFrame::default()
        };
        frame.field[0] = 1;
        frame.field[1] = 2;
        frame.field[2] = 3;
        frame.field[3] = 0;

        let mut buf = Vec::new();
        encode_v3(&frame, &mut buf);

        assert_eq!(buf[0], 0b011_01_000); // v3, classic, player 0
        assert_eq!(&buf[1..3], &[0x01, 0x02]);
        // ctime 0x0ABCDEF: 28 bits -> bytes 0xAB 0xCD 0xEF? No: value<<? —
        // ctime>>20=0x0A ... wait, 0x0ABCDEF >> 20 = 0x0A? 0x0ABCDEF is 28
        // bits: 0000 1010 1011 1100 1101 1110 1111.
        assert_eq!(buf[3], 0x0A);
        assert_eq!(buf[4], 0xBC);
        assert_eq!(buf[5], 0xDE);
        assert_eq!(buf[6], 0xF1); // low nibble of ctime (0xF) | lines hi (0x1)
        assert_eq!(buf[7], 0x23); // lines low byte
        assert_eq!(buf[8], 0x14); // level
        assert_eq!(&buf[9..12], &[0x05, 0x67, 0x89]); // score 24-bit BE
        assert_eq!(buf[12], 0x0A << 3); // das 0x0A | preview T=0
        assert_eq!(buf[13], 0x1E << 3 | 5); // das 0x1E | L=5
        // counts 1..7 at 10 bits each, MSB-first:
        // 0000000001 0000000010 0000000011 0000000100 0000000101 0000000110
        // 0000000111 + 00 pad
        let bits: String = frame
            .counts
            .iter()
            .map(|c| format!("{:010b}", c.unwrap()))
            .collect::<String>()
            + "00";
        let expect: Vec<u8> = (0..9)
            .map(|i| u8::from_str_radix(&bits[i * 8..i * 8 + 8], 2).unwrap())
            .collect();
        assert_eq!(&buf[14..23], expect.as_slice());
        // field: cells 1,2,3,0 -> 0b01_10_11_00
        assert_eq!(buf[23], 0b0110_1100);
        assert_eq!(buf.len(), V3_FRAME_SIZE);

        let (decoded, _) = decode_frame(&buf).unwrap();
        assert_eq!(decoded, frame);
    }

    #[test]
    fn v1_decodes() {
        // Build a v1 frame by hand: score 0x012345 (21 bits), lines 0x1F5
        // (9 bits)... keep it simple: score=1000, lines=10, level=5.
        let mut f = vec![0u8; V1_FRAME_SIZE];
        f[0] = 1 << 5;
        f[1] = 0;
        f[2] = 7; // gameid 7
        // ctime 5000 ms
        let ct: u32 = 5000;
        f[3] = (ct >> 20) as u8;
        f[4] = (ct >> 12) as u8;
        f[5] = (ct >> 4) as u8;
        f[6] = ((ct & 0xF) as u8) << 4;
        // score 1000 = 0b1111101000 (21-bit container)
        let score: u32 = 1000;
        f[7] = ((score >> 17) & 0x0F) as u8;
        f[8] = (score >> 9) as u8;
        f[9] = (score >> 1) as u8;
        f[10] = ((score & 1) as u8) << 7;
        // lines 10 (9 bits): top 7 into f[10] low bits, bottom 2 into f[11]
        let lines: u16 = 10;
        f[10] |= ((lines >> 2) & 0x7F) as u8;
        f[11] = (((lines & 0b11) as u8) << 6) | 5; // level 5
        f[12] = SENTINEL_DAS << 3 | SENTINEL_PIECE;
        f[13] = SENTINEL_DAS << 3 | 6; // cur piece I
        for i in 0..7 {
            f[14 + i] = (i + 1) as u8;
        }
        f[21] = 0b1000_0000; // field cell 0 = 2

        let (frame, consumed) = decode_frame(&f).unwrap();
        assert_eq!(consumed, V1_FRAME_SIZE);
        assert_eq!(frame.version, 1);
        assert_eq!(frame.gameid, 7);
        assert_eq!(frame.ctime_ms, 5000);
        assert_eq!(frame.score, Some(1000));
        assert_eq!(frame.lines, Some(10));
        assert_eq!(frame.level, Some(5));
        assert_eq!(frame.instant_das, None);
        assert_eq!(frame.preview, None);
        assert_eq!(frame.cur_piece, Some(Piece::I));
        assert_eq!(frame.counts[2], Some(3));
        assert_eq!(frame.field[0], 2);
    }

    #[test]
    fn v2_decodes_counts() {
        let mut f = vec![0u8; V2_FRAME_SIZE];
        f[0] = 2 << 5;
        // lines/level/score sentinels
        f[6] |= 0x0F;
        f[7] = 0xFF;
        f[8] = 0xFF;
        f[9] = 0xFF;
        f[10] = 0xFF;
        f[11] = 0xFF;
        f[12] = 0xFF;
        f[13] = 0xFF;
        // counts: 9 bits each, all = 3 -> pack MSB-first
        let bits: String = (0..7).map(|_| "000000011").collect::<String>() + "0";
        for i in 0..8 {
            f[14 + i] = u8::from_str_radix(&bits[i * 8..i * 8 + 8], 2).unwrap();
        }
        let (frame, consumed) = decode_frame(&f).unwrap();
        assert_eq!(consumed, V2_FRAME_SIZE);
        assert_eq!(frame.counts, [Some(3); 7]);
        assert_eq!(frame.score, None);
        assert_eq!(frame.lines, None);
        assert_eq!(frame.level, None);
    }

    #[test]
    fn unknown_version_rejected() {
        let buf = [7u8 << 5; 80];
        assert_eq!(decode_frame(&buf), Err(NgfError::UnknownVersion(7)));
    }

    #[test]
    fn truncated_rejected() {
        let mut buf = Vec::new();
        encode_v3(&NgfFrame::default(), &mut buf);
        buf.truncate(40);
        assert!(matches!(
            decode_frame(&buf),
            Err(NgfError::Truncated { expected: 73, .. })
        ));
    }
}

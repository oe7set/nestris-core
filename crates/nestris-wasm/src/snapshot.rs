//! Compact binary frame snapshot for the JS boundary (little-endian).
//!
//! Replaces the per-frame JSON string: the engine serializes one
//! `FrameSnapshot` into a persistent buffer, JS reads it via
//! `snapshot_ptr()/len` and decodes with `web/src/snapshot.ts` (which must
//! mirror this layout exactly — bump [`SNAPSHOT_VERSION`] on any change).
//!
//! Layout (all little-endian, in order):
//!
//! ```text
//! u8   version
//! u8   lock_state    0 UNLOCKED 1 ACQUIRING 2 LOCKED 3 DRIFT 4 LOST
//! u8   game_state    0 no_signal 1 unknown 2 title 3 type_select
//!                    4 level_select 5 in_game 6 paused 7 game_over
//!                    8 highscore_entry
//! u8   flags         bit0 playfield present, bit1 extended stats present,
//!                    bit2 recording active, bit3 lock quad present,
//!                    bit4 statistics map present
//! i64  seq           f64 ts
//! i32  score/lines/level (i32::MIN = unknown)
//! u8   next_piece    piece code 0 '-' 1 I 2 O 3 T 4 S 5 Z 6 J 7 L,
//!                    255 = unknown
//! u8   current_piece
//! u8   has_pos (+ u16 row, u16 col when 1)
//! u8   n_cells, then n x (u8 row, u8 col)
//! 9 x f32 confidences (score, lines, level, next, playfield, statistics,
//!                      current, geometry, overall)
//! [200]u8 playfield              when flag bit0
//! stats block (fixed, f64 NaN = unknown):
//!   f64 pps, f64 tetris_rate, i64 burn, u32 drought, u32 max_drought,
//!   u32 single, u32 double, u32 triple, u32 tetris, f64 score_per_min,
//!   i64 pieces, f64 active_seconds
//! statistics map                 when flag bit4:
//!   u8 n, then n x (u8 piece_code, i32 value (i32::MIN = unknown))
//! extended stats                 when flag bit1:
//!   5 x i64 points (drops, singles, doubles, triples, tetrises)
//!   f64 efficiency (NaN), i64 pace (i64::MIN)
//!   4 x u32 i_drought (current, last, max, count)
//!   u8 board_max_height, f32 avg_height, u16 holes, u8 board_flags
//!   u16 n_trt, n x (u32 lines, f32 rate)
//!   u16 n_height, n x (f32 ts, u8 height, u8 flags)
//!   7 x u32 piece counts, 7 x u32 piece droughts, f64 deviation
//! lock quad                      when flag bit3: 8 x f32 (source px)
//! u16  events JSON length, then that many UTF-8 bytes (0 = no events)
//! ```

use nestris_engine::enums::{GameState, Piece};
use nestris_engine::geometry_cal::lock::LockState;
use nestris_engine::output::OutputFrame;

pub const SNAPSHOT_VERSION: u8 = 1;

pub fn lock_state_code(state: LockState) -> u8 {
    match state {
        LockState::Unlocked => 0,
        LockState::Acquiring => 1,
        LockState::Locked => 2,
        LockState::Drift => 3,
        LockState::Lost => 4,
    }
}

fn game_state_code(state: GameState) -> u8 {
    match state {
        GameState::NoSignal => 0,
        GameState::Unknown => 1,
        GameState::Title => 2,
        GameState::TypeSelect => 3,
        GameState::LevelSelect => 4,
        GameState::InGame => 5,
        GameState::Paused => 6,
        GameState::GameOver => 7,
        GameState::HighscoreEntry => 8,
    }
}

fn piece_code(piece: Option<Piece>) -> u8 {
    match piece {
        None => 255,
        Some(Piece::None) => 0,
        Some(Piece::I) => 1,
        Some(Piece::O) => 2,
        Some(Piece::T) => 3,
        Some(Piece::S) => 4,
        Some(Piece::Z) => 5,
        Some(Piece::J) => 6,
        Some(Piece::L) => 7,
    }
}

struct W<'a>(&'a mut Vec<u8>);

impl W<'_> {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i64(&mut self, v: i64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f64(&mut self, v: f64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn opt_i32(&mut self, v: Option<i64>) {
        self.i32(v.map_or(i32::MIN, |x| x.clamp(i64::from(i32::MIN + 1), i64::from(i32::MAX)) as i32));
    }
    fn opt_f64(&mut self, v: Option<f64>) {
        self.f64(v.unwrap_or(f64::NAN));
    }
}

/// Serialize one output frame (plus host-visible lock info) into `out`.
pub fn encode_snapshot(
    frame: &OutputFrame,
    lock_state: u8,
    lock_quad: Option<[f64; 8]>,
    recording: bool,
    out: &mut Vec<u8>,
) {
    out.clear();
    let mut w = W(out);
    let f = &frame.fields;

    let mut flags = 0u8;
    if f.playfield.is_some() {
        flags |= 1;
    }
    if frame.stats_ext.is_some() {
        flags |= 1 << 1;
    }
    if recording {
        flags |= 1 << 2;
    }
    if lock_quad.is_some() {
        flags |= 1 << 3;
    }
    if f.statistics.is_some() {
        flags |= 1 << 4;
    }

    w.u8(SNAPSHOT_VERSION);
    w.u8(lock_state);
    w.u8(game_state_code(frame.game_state));
    w.u8(flags);
    w.i64(frame.seq);
    w.f64(frame.ts);
    w.opt_i32(f.score);
    w.opt_i32(f.lines);
    w.opt_i32(f.level);
    w.u8(piece_code(f.next_piece));
    w.u8(piece_code(f.current_piece));
    match f.current_piece_pos {
        Some((r, c)) => {
            w.u8(1);
            w.u16(r as u16);
            w.u16(c as u16);
        }
        None => w.u8(0),
    }
    let cells = f.current_piece_cells.as_deref().unwrap_or(&[]);
    w.u8(cells.len().min(255) as u8);
    for &(r, c) in cells.iter().take(255) {
        w.u8(r as u8);
        w.u8(c as u8);
    }

    let c = &frame.confidence;
    for v in [
        c.score,
        c.lines,
        c.level,
        c.next_piece,
        c.playfield,
        c.statistics,
        c.current_piece,
        c.geometry,
        c.overall,
    ] {
        w.f32(v as f32);
    }

    if let Some(grid) = &f.playfield {
        for row in grid.iter().take(20) {
            for &cell in row.iter().take(10) {
                w.u8(cell);
            }
        }
    }

    let s = &frame.stats;
    w.opt_f64(s.pps);
    w.opt_f64(s.tetris_rate);
    w.i64(s.burn);
    w.u32(s.drought);
    w.u32(s.max_drought);
    w.u32(s.clears.single);
    w.u32(s.clears.double);
    w.u32(s.clears.triple);
    w.u32(s.clears.tetris);
    w.opt_f64(s.score_per_min);
    w.i64(s.pieces);
    w.opt_f64(s.active_seconds);

    if let Some(stats_map) = &f.statistics {
        w.u8(stats_map.0.len().min(255) as u8);
        for &(piece, value) in stats_map.0.iter().take(255) {
            w.u8(piece_code(Some(piece)));
            w.opt_i32(value);
        }
    }

    if let Some(ext) = &frame.stats_ext {
        w.i64(ext.points.drops);
        w.i64(ext.points.singles);
        w.i64(ext.points.doubles);
        w.i64(ext.points.triples);
        w.i64(ext.points.tetrises);
        w.opt_f64(ext.efficiency);
        w.i64(ext.pace_score.unwrap_or(i64::MIN));
        w.u32(ext.i_drought.current);
        w.u32(ext.i_drought.last);
        w.u32(ext.i_drought.max);
        w.u32(ext.i_drought.count);
        w.u8(ext.board.max_height);
        w.f32(ext.board.avg_height);
        w.u16(ext.board.holes);
        let mut bf = 0u8;
        if ext.board.tetris_ready {
            bf |= 1;
        }
        if ext.board.double_well {
            bf |= 2;
        }
        if ext.board.clean_slope {
            bf |= 4;
        }
        w.u8(bf);
        w.u16(ext.trt_trend.len().min(u16::MAX as usize) as u16);
        for &(lines, rate) in &ext.trt_trend {
            w.u32(lines.clamp(0, i64::from(u32::MAX)) as u32);
            w.f32(rate as f32);
        }
        w.u16(ext.height_timeline.len().min(u16::MAX as usize) as u16);
        for &(ts, height, hf) in &ext.height_timeline {
            w.f32(ts as f32);
            w.u8(height);
            w.u8(hf);
        }
        for &count in &ext.piece_dist.counts {
            w.u32(count.clamp(0, i64::from(u32::MAX)) as u32);
        }
        for &d in &ext.piece_dist.drought {
            w.u32(d);
        }
        w.f64(ext.piece_dist.deviation);
    }

    if let Some(quad) = lock_quad {
        for v in quad {
            w.f32(v as f32);
        }
    }

    if frame.events.is_empty() {
        w.u16(0);
    } else {
        let json = serde_json::to_string(&frame.events).unwrap_or_default();
        let bytes = json.as_bytes();
        let len = bytes.len().min(u16::MAX as usize);
        w.u16(len as u16);
        w.0.extend_from_slice(&bytes[..len]);
    }
}

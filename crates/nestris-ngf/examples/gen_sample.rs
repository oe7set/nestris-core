//! Generate a small sample .ngf file for cross-checking against the
//! reference Python parser: `cargo run -p nestris-ngf --example gen_sample -- <out>`

use nestris_engine::enums::Piece;
use nestris_ngf::codec::{NgfFrame, encode_v3};

fn main() {
    let out = std::env::args()
        .nth(1)
        .expect("usage: gen_sample <out.ngf>");
    let mut buf = Vec::new();
    for i in 0u32..10 {
        let mut field = [0u8; 200];
        for c in 0..10 {
            field[190 + c] = ((c + i as usize) % 4) as u8;
        }
        let frame = NgfFrame {
            gameid: 3,
            ctime_ms: i * 16,
            lines: Some(60 + i as u16),
            level: Some(18),
            score: Some(100_000 + i * 1200),
            instant_das: None,
            preview: Some(Piece::T),
            cur_piece_das: None,
            cur_piece: Some(Piece::I),
            counts: [
                Some(10 + i as u16),
                Some(11),
                Some(12),
                Some(13),
                Some(14),
                Some(15),
                Some(16),
            ],
            field,
            ..NgfFrame::default()
        };
        encode_v3(&frame, &mut buf);
    }
    std::fs::write(&out, &buf).expect("write sample");
    println!("wrote {} bytes to {out}", buf.len());
}

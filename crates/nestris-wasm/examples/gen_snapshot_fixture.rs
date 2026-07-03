//! Generate the cross-language snapshot-codec fixture consumed by
//! `web/src/snapshot.test.ts`:
//!
//! ```text
//! cargo run -p nestris-wasm --example gen_snapshot_fixture -- web/src/snapshot.fixture.json
//! ```
//!
//! The fixture pairs the binary snapshot (hex) of a fully populated
//! synthetic frame with its JSON serialization, so the TypeScript decoder
//! is tested against the actual Rust encoder output.

use nestris_engine::enums::{GameState, Piece, Region};
use nestris_engine::output::{
    Confidence, Event, Fields, GameStats, LineClears, OutputFrame, SCHEMA_VERSION, StatisticsMap,
};
use nestris_engine::stats_ext::{
    BoardMetrics, DroughtStats, ExtendedStats, PieceDistribution, PointsBreakdown,
};
use nestris_wasm::snapshot::encode_snapshot;

fn synthetic_frame() -> OutputFrame {
    let mut playfield = vec![vec![0u8; 10]; 20];
    playfield[19] = vec![1, 2, 3, 0, 1, 2, 3, 0, 1, 2];
    playfield[0][0] = 2;

    OutputFrame {
        schema_version: SCHEMA_VERSION,
        seq: 1234,
        ts: 45.675,
        region: Region::Ntsc,
        game_state: GameState::InGame,
        fields: Fields {
            score: Some(114_535),
            lines: Some(65),
            level: Some(10),
            next_piece: Some(Piece::S),
            current_piece: Some(Piece::I),
            current_piece_pos: Some((3, 5)),
            current_piece_cells: Some(vec![(2, 5), (3, 5), (4, 5), (5, 5)]),
            playfield: Some(playfield),
            statistics: Some(StatisticsMap(vec![
                (Piece::T, Some(24)),
                (Piece::J, Some(30)),
                (Piece::Z, Some(18)),
                (Piece::O, Some(26)),
                (Piece::S, Some(22)),
                (Piece::L, Some(21)),
                (Piece::I, None),
            ])),
        },
        stats: GameStats {
            pps: Some(0.87),
            tetris_rate: Some(0.43),
            burn: 37,
            drought: 14,
            max_drought: 21,
            clears: LineClears {
                single: 21,
                double: 5,
                triple: 2,
                tetris: 7,
            },
            score_per_min: None,
            pieces: 181,
            active_seconds: Some(208.4),
        },
        stats_ext: Some(ExtendedStats {
            points: PointsBreakdown {
                drops: 795,
                singles: 9240,
                doubles: 5500,
                triples: 6600,
                tetrises: 92_400,
            },
            efficiency: Some(159.0),
            pace_score: Some(1_253_335),
            i_drought: DroughtStats {
                current: 14,
                last: 0,
                max: 21,
                count: 2,
            },
            board: BoardMetrics {
                max_height: 9,
                avg_height: 4.5,
                holes: 3,
                tetris_ready: true,
                double_well: false,
                clean_slope: false,
            },
            trt_trend: vec![(4, 1.0), (5, 0.8), (9, 0.888)],
            height_timeline: vec![(1.0, 3, 0), (1.25, 5, 1), (1.5, 4, 9)],
            piece_dist: PieceDistribution {
                counts: [24, 30, 18, 26, 22, 21, 24],
                drought: [1, 3, 0, 5, 2, 7, 14],
                deviation: 0.081,
            },
        }),
        confidence: Confidence {
            score: 1.0,
            lines: 0.9,
            level: 0.8,
            next_piece: 0.7,
            playfield: 0.6,
            statistics: 0.5,
            current_piece: 0.4,
            geometry: 0.973,
            overall: 0.85,
        },
        events: vec![Event {
            ts: 45.6,
            field: "lines".into(),
            reason: "clear_tetris".into(),
            severity: "info".into(),
            old: None,
            new: Some(serde_json::Value::from(4)),
            confidence: None,
        }],
    }
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .expect("usage: gen_snapshot_fixture <out.json>");
    let frame = synthetic_frame();
    let mut buf = Vec::new();
    let quad = [10.5, 20.5, 600.25, 22.0, 610.0, 500.0, 12.0, 498.75];
    encode_snapshot(&frame, 2 /* LOCKED */, Some(quad), true, &mut buf);
    let hex: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    let fixture = serde_json::json!({
        "snapshot_hex": hex,
        "lock_state": "LOCKED",
        "recording": true,
        "lock_quad": quad,
        "frame_json": serde_json::to_value(&frame).unwrap(),
    });
    std::fs::write(&out, serde_json::to_string_pretty(&fixture).unwrap()).unwrap();
    println!("wrote {} ({} snapshot bytes)", out, buf.len());
}

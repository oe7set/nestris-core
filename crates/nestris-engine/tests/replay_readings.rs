//! Phase-4 gate: replay Python-dumped pre-fusion readings through the Rust
//! fusion -> plausibility -> stats -> output chain and diff the produced
//! OutputFrames against the Python oracle's output.jsonl — EXACTLY (these
//! layers are deterministic integer/f64 machines; no CV is involved).
//!
//! Mirrors the processor's post-recognition glue: the new-game reset order,
//! the plausibility write-back, and the event merge order (plausibility,
//! stats-drain, new-game).

use std::path::{Path, PathBuf};

use nestris_engine::config::{FusionConfig, PlausibilityConfig};
use nestris_engine::enums::{GameState, Piece, Region};
use nestris_engine::output::{
    Confidence, Event, Fields, OutputFrame, SCHEMA_VERSION, StatisticsMap,
};
use nestris_engine::state::fusion::{FusionEngine, RawReading};
use nestris_engine::state::plausibility::{PlausibilityFilter, PlausibilityInput};
use nestris_engine::stats::StatsEngine;
use serde_json::Value;

fn stages_dir() -> PathBuf {
    std::env::var("NESTRIS_STAGES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/stages"))
}

fn read_jsonl(path: &Path) -> Vec<Value> {
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("jsonl line"))
        .collect()
}

fn game_state(v: &Value) -> GameState {
    serde_json::from_value(v.clone()).expect("game state")
}

fn reading_from_row(row: &Value) -> RawReading {
    let opt_pair = |v: &Value| -> Option<(u32, u32)> {
        v.as_array()
            .map(|a| (a[0].as_u64().unwrap() as u32, a[1].as_u64().unwrap() as u32))
    };
    RawReading {
        seq: row["seq"].as_i64().unwrap(),
        ts: row["ts"].as_f64().unwrap(),
        state: game_state(&row["state"]),
        state_confidence: row["state_confidence"].as_f64().unwrap_or(0.0),
        score: row["score"].as_i64(),
        score_confidence: row["score_confidence"].as_f64().unwrap_or(0.0),
        lines: row["lines"].as_i64(),
        lines_confidence: row["lines_confidence"].as_f64().unwrap_or(0.0),
        level: row["level"].as_i64(),
        level_confidence: row["level_confidence"].as_f64().unwrap_or(0.0),
        next_piece: row["next_piece"].as_str().and_then(Piece::from_letter),
        next_confidence: row["next_confidence"].as_f64().unwrap_or(0.0),
        playfield: row["playfield"].as_array().map(|rows| {
            rows.iter()
                .map(|r| {
                    r.as_array()
                        .unwrap()
                        .iter()
                        .map(|c| c.as_i64().unwrap() as u8)
                        .collect()
                })
                .collect()
        }),
        playfield_confidence: row["playfield_confidence"].as_f64().unwrap_or(0.0),
        statistics: row["statistics"].as_object().map(|obj| {
            obj.iter()
                .map(|(k, v)| (Piece::from_letter(k).expect("piece letter"), v.as_i64()))
                .collect()
        }),
        statistics_confidence: row["statistics_confidence"].as_f64().unwrap_or(0.0),
        current_piece: row["current_piece"].as_str().and_then(Piece::from_letter),
        current_piece_pos: row["current_piece_pos"]
            .as_array()
            .map(|_| opt_pair(&row["current_piece_pos"]).unwrap()),
        current_piece_cells: row["current_piece_cells"]
            .as_array()
            .map(|cells| cells.iter().map(|c| opt_pair(c).unwrap()).collect()),
        current_piece_confidence: row["current_piece_confidence"].as_f64().unwrap_or(0.0),
        geometry_confidence: row["geometry_confidence"].as_f64().unwrap_or(1.0),
    }
}

/// Structural JSON equality with exact ints and 1e-9-relative floats.
fn json_close(a: &Value, b: &Value, path: &str) -> Result<(), String> {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            if x == y {
                return Ok(());
            }
            let (xf, yf) = (x.as_f64().unwrap(), y.as_f64().unwrap());
            if (xf - yf).abs() <= 1e-9 * yf.abs().max(1.0) {
                Ok(())
            } else {
                Err(format!("{path}: {x} != {y}"))
            }
        }
        (Value::Array(xs), Value::Array(ys)) => {
            if xs.len() != ys.len() {
                return Err(format!("{path}: array len {} != {}", xs.len(), ys.len()));
            }
            for (i, (x, y)) in xs.iter().zip(ys.iter()).enumerate() {
                json_close(x, y, &format!("{path}[{i}]"))?;
            }
            Ok(())
        }
        (Value::Object(xs), Value::Object(ys)) => {
            if xs.len() != ys.len() {
                let xk: Vec<_> = xs.keys().collect();
                let yk: Vec<_> = ys.keys().collect();
                return Err(format!("{path}: keys {xk:?} != {yk:?}"));
            }
            for ((kx, x), (ky, y)) in xs.iter().zip(ys.iter()) {
                if kx != ky {
                    return Err(format!("{path}: key order {kx} != {ky}"));
                }
                json_close(x, y, &format!("{path}.{kx}"))?;
            }
            Ok(())
        }
        _ => {
            if a == b {
                Ok(())
            } else {
                Err(format!("{path}: {a} != {b}"))
            }
        }
    }
}

#[test]
fn replay_readings_reproduces_python_output_exactly() {
    let stages = stages_dir();
    let Ok(entries) = std::fs::read_dir(&stages) else {
        eprintln!("stage dumps not found at {} — skipped", stages.display());
        return;
    };
    let mut fixtures = 0usize;
    let mut total_frames = 0usize;

    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.join("meta.json").exists() {
            continue;
        }
        let readings = read_jsonl(&dir.join("raw_readings.jsonl"));
        let outputs = read_jsonl(&dir.join("output.jsonl"));
        if readings.is_empty() || outputs.len() != readings.len() {
            continue;
        }
        let name = dir.file_name().unwrap().to_string_lossy().to_string();

        // Oracle behavior: a new game fires on its first in-game frame.
        let mut fusion = FusionEngine::new(FusionConfig {
            new_game_confirm_frames: 0,
            ..FusionConfig::default()
        });
        let mut plausibility = PlausibilityFilter::new(PlausibilityConfig::default());
        let mut stats = StatsEngine::new();

        for (row, expected) in readings.iter().zip(outputs.iter()) {
            let reading = reading_from_row(row);
            let mut fused = fusion.update(&reading);
            let mut frame_events: Vec<Event> = Vec::new();
            if fused.is_new_game {
                plausibility.reset();
                stats.reset();
                frame_events.push(Event {
                    ts: reading.ts,
                    field: "game".into(),
                    reason: "new_game".into(),
                    severity: "info".into(),
                    old: None,
                    new: None,
                    confidence: None,
                });
            }
            let plausible = plausibility.filter(&PlausibilityInput {
                seq: reading.seq,
                ts: reading.ts,
                state: fused.state,
                score: fused.score,
                lines: fused.lines,
                level: fused.level,
                score_conf: Some(fused.confidence.score),
                lines_conf: Some(fused.confidence.lines),
                level_conf: Some(fused.confidence.level),
            });
            fused.score = plausible.score;
            fused.lines = plausible.lines;
            fused.level = plausible.level;
            let game_stats = stats.update(&fused);

            let mut events: Vec<Event> = plausible
                .events
                .iter()
                .map(|ev| Event {
                    ts: ev.ts,
                    field: ev.field.into(),
                    reason: ev.reason.into(),
                    severity: ev.severity.into(),
                    old: ev.old.map(Value::from),
                    new: ev.new.map(Value::from),
                    confidence: ev.confidence,
                })
                .collect();
            for sev in stats.drain_events() {
                events.push(Event {
                    ts: sev.ts,
                    field: sev.field.into(),
                    reason: sev.reason.into(),
                    severity: sev.severity.into(),
                    old: sev.old.map(Value::from),
                    new: sev.new.map(Value::from),
                    confidence: None,
                });
            }
            events.extend(frame_events);

            let statistics = fused.statistics.clone().map(|s| StatisticsMap(s.0));
            let frame = OutputFrame {
                schema_version: SCHEMA_VERSION,
                seq: reading.seq,
                ts: reading.ts,
                region: Region::Ntsc,
                game_state: fused.state,
                fields: Fields {
                    score: fused.score,
                    lines: fused.lines,
                    level: fused.level,
                    next_piece: fused.next_piece,
                    current_piece: fused.current_piece,
                    current_piece_pos: fused.current_piece_pos,
                    current_piece_cells: fused.current_piece_cells.clone(),
                    playfield: fused.playfield.clone(),
                    statistics,
                },
                stats: game_stats,
                stats_ext: None,
                confidence: Confidence {
                    score: fused.confidence.score,
                    lines: fused.confidence.lines,
                    level: fused.confidence.level,
                    next_piece: fused.confidence.next_piece,
                    playfield: fused.confidence.playfield,
                    statistics: fused.confidence.statistics,
                    current_piece: fused.confidence.current_piece,
                    geometry: fused.confidence.geometry,
                    overall: fused.confidence.overall,
                },
                events,
            };
            let got: Value = serde_json::from_str(&frame.to_json()).unwrap();
            if let Err(msg) = json_close(&got, expected, "$") {
                panic!(
                    "{name} seq {}: {msg}\n got: {}\n exp: {}",
                    reading.seq,
                    frame.to_json(),
                    expected
                );
            }
            total_frames += 1;
        }
        fixtures += 1;
        eprintln!("{name}: {} frames exact", readings.len());
    }
    if fixtures == 0 {
        eprintln!("no stage dumps — skipped");
        return;
    }
    eprintln!("replay-readings: {fixtures} fixtures, {total_frames} frames, all exact");
}

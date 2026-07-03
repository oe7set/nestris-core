//! Phase-3 gate: replay Python-rectified canonical frames through the Rust
//! recognition layer and compare against the Python pipeline's dumped
//! pre-fusion readings (`testdata/stages/<fixture>/`).
//!
//! Stateless comparisons only (digits, next piece, statistics, pre-stabilizer
//! playfield with the fused level reconstructed from output.jsonl); temporal
//! chains (stabilizer, base latch, clear animation, current piece) are gated
//! by the Phase-4 replay-readings and Phase-5 full-pipeline diffs.
//!
//! Skips silently when the stage dumps are absent (they are regenerated from
//! fixtures by the Python repo's tools/dump_stage_artifacts.py).

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};

use nestris_engine::enums::Piece;
use nestris_engine::layout::get_layout;
use nestris_engine::palette::to_luma;
use nestris_engine::recognition::digits::{BaseMode, DigitReader};
use nestris_engine::recognition::next_piece::NextPieceReader;
use nestris_engine::recognition::playfield::PlayfieldReader;
use nestris_engine::recognition::statistics::StatisticsReader;
use nestris_vision::Image;
use serde_json::Value;

fn stages_dir() -> PathBuf {
    std::env::var("NESTRIS_STAGES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/stages"))
}

/// Canonical stage PNGs were written by `cv2.imwrite(path, bgr)`, so the
/// PNG's RGB channels are the BGR array reversed — swap back on load.
fn load_canonical(path: &Path) -> Image {
    let decoder = png::Decoder::new(File::open(path).expect("canonical png"));
    let mut reader = decoder.read_info().expect("png info");
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).expect("png frame");
    buf.truncate(info.buffer_size());
    assert_eq!(
        info.color_type,
        png::ColorType::Rgb,
        "canonical must be rgb"
    );
    for px in buf.chunks_exact_mut(3) {
        px.swap(0, 2);
    }
    Image::from_vec(buf, info.width as usize, info.height as usize, 3)
}

fn read_jsonl(path: &Path) -> Vec<Value> {
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("jsonl line"))
        .collect()
}

#[derive(Default)]
struct FieldTally {
    compared: usize,
    mismatched: usize,
}

impl FieldTally {
    fn add(&mut self, ok: bool) {
        self.compared += 1;
        if !ok {
            self.mismatched += 1;
        }
    }

    fn rate(&self) -> f64 {
        if self.compared == 0 {
            1.0
        } else {
            1.0 - self.mismatched as f64 / self.compared as f64
        }
    }
}

fn opt_i64(v: &Value) -> Option<i64> {
    v.as_i64()
}

#[test]
fn replay_canonical_matches_python_raw_readings() {
    let stages = stages_dir();
    let Ok(entries) = std::fs::read_dir(&stages) else {
        eprintln!("stage dumps not found at {} — skipped", stages.display());
        return;
    };
    let layout = get_layout();
    let digit_reader = DigitReader::new(BaseMode::Dec);
    let stats_reader = StatisticsReader::new();

    let mut grand: HashMap<&str, FieldTally> = HashMap::new();
    let mut fixtures_checked = 0usize;
    let mut playfield_cells_compared = 0usize;
    let mut playfield_cells_mismatched = 0usize;

    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.join("meta.json").exists() {
            continue;
        }
        let readings = read_jsonl(&dir.join("raw_readings.jsonl"));
        if !readings.iter().any(|r| r.get("raw_playfield").is_some()) {
            // Old-format dump (or no gameplay frames at all); skip.
            continue;
        }
        let by_seq: HashMap<i64, &Value> = readings
            .iter()
            .filter_map(|r| r["seq"].as_i64().map(|s| (s, r)))
            .collect();
        // Reconstruct the processor's `_last_level` (last non-null fused level
        // strictly before each frame) from the full per-frame output stream.
        let outputs = read_jsonl(&dir.join("output.jsonl"));
        let mut last_level_before: HashMap<i64, Option<i64>> = HashMap::new();
        let mut last_level: Option<i64> = None;
        for out in &outputs {
            let seq = out["seq"].as_i64().unwrap();
            last_level_before.insert(seq, last_level);
            if let Some(l) = out["fields"]["level"].as_i64() {
                last_level = Some(l);
            }
        }

        let canon_dir = dir.join("canonical");
        let Ok(pngs) = std::fs::read_dir(&canon_dir) else {
            continue;
        };
        let mut frames = 0usize;
        for png_entry in pngs.flatten() {
            let path = png_entry.path();
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Ok(seq) = stem.parse::<i64>() else {
                continue;
            };
            let Some(row) = by_seq.get(&seq) else {
                continue;
            };
            // Only gameplay frames carry recognition outputs.
            if row
                .get("raw_playfield")
                .map(|v| v.is_null())
                .unwrap_or(true)
            {
                continue;
            }
            let canon = load_canonical(&path);
            let gray = to_luma(&canon);
            frames += 1;

            // Digits: score (auto — the latch only changes cost), lines, level.
            let score = digit_reader.read_field(
                &gray,
                &layout.score,
                layout.score_digits,
                Some(BaseMode::Auto),
            );
            grand
                .entry("score")
                .or_default()
                .add(score.value == opt_i64(&row["score"]));
            let lines = digit_reader.read_field(&gray, &layout.lines, layout.lines_digits, None);
            grand
                .entry("lines")
                .or_default()
                .add(lines.value == opt_i64(&row["lines"]));
            let level = digit_reader.read_field(&gray, &layout.level, layout.level_digits, None);
            grand
                .entry("level")
                .or_default()
                .add(level.value == opt_i64(&row["level"]));

            // NEXT piece.
            let next = NextPieceReader::read(&canon, &layout.next_box);
            let expected_next = row["next_piece"].as_str().and_then(Piece::from_letter);
            grand
                .entry("next_piece")
                .or_default()
                .add(next.piece == expected_next);

            // STATISTICS (Python reads them on seq % 6 == 0; the 30-stride
            // canonical samples are always such frames).
            if let Some(expected_stats) = row["statistics"].as_object() {
                let got = stats_reader.read(&gray, layout);
                let mut ok = true;
                for (piece, value) in &got.counts {
                    let exp = expected_stats.get(piece.letter()).and_then(|v| v.as_i64());
                    if *value != exp {
                        ok = false;
                    }
                }
                grand.entry("statistics").or_default().add(ok);
            }

            // Pre-stabilizer playfield with the reconstructed fused level.
            let lvl = last_level_before.get(&seq).copied().flatten();
            let reading = PlayfieldReader::read(&canon, &gray, layout, lvl);
            let expected_grid = row["raw_playfield"].as_array().unwrap();
            let mut grid_ok = true;
            for (r, exp_row) in expected_grid.iter().enumerate() {
                for (c, exp_cell) in exp_row.as_array().unwrap().iter().enumerate() {
                    let exp = exp_cell.as_i64().unwrap() as u8;
                    playfield_cells_compared += 1;
                    if reading.grid[r][c] != exp {
                        playfield_cells_mismatched += 1;
                        grid_ok = false;
                    }
                }
            }
            grand.entry("playfield_grid").or_default().add(grid_ok);
        }
        if frames > 0 {
            fixtures_checked += 1;
        }
    }

    if fixtures_checked == 0 {
        eprintln!("no gameplay canonical frames found — skipped");
        return;
    }

    let mut names: Vec<&&str> = grand.keys().collect();
    names.sort();
    for name in names {
        let t = &grand[*name];
        eprintln!(
            "{name}: {}/{} frames match ({:.2}%)",
            t.compared - t.mismatched,
            t.compared,
            t.rate() * 100.0
        );
    }
    if playfield_cells_compared > 0 {
        eprintln!(
            "playfield cells: {}/{} match ({:.4}%)",
            playfield_cells_compared - playfield_cells_mismatched,
            playfield_cells_compared,
            (1.0 - playfield_cells_mismatched as f64 / playfield_cells_compared as f64) * 100.0
        );
    }

    // Phase-3 gate: >=99.5% frame parity per field across all fixtures.
    for (name, tally) in &grand {
        assert!(
            tally.rate() >= 0.995,
            "{name}: only {:.2}% of {} frames match Python",
            tally.rate() * 100.0,
            tally.compared
        );
    }
}

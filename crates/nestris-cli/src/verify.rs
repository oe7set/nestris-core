//! Full-pipeline verification against the Python oracle's stage dumps.
//!
//! Decodes each fixture with ffmpeg, runs the Rust `FrameProcessor` in
//! oracle-parity mode (inline solves, no background thread), and diffs the
//! emitted frames against the Python `output.jsonl` under the port plan's
//! policy: exact-class fields must match on >=99.5% of frames with a
//! ±3-frame transition slack; confidences are reported, not gated (they sit
//! downstream of RANSAC, whose RNG differs across languages by design).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use nestris_engine::processor::FrameProcessor;
use serde_json::Value;

use nestris_host::capture_ffmpeg::VideoDecoder;

use crate::engine_config;

const TRANSITION_SLACK: i64 = 3;
const PASS_RATE: f64 = 0.995;

/// Fields compared exactly (with transition slack), as JSON pointers.
const EXACT_FIELDS: [&str; 12] = [
    "/game_state",
    "/fields/score",
    "/fields/lines",
    "/fields/level",
    "/fields/next_piece",
    "/fields/current_piece",
    "/fields/playfield",
    "/fields/statistics",
    "/stats/burn",
    "/stats/clears",
    "/stats/pieces",
    "/stats/drought",
];

fn slugify(stem: &str) -> String {
    let mut slug: String = stem
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    // Collapse runs of '_' like the Python re.sub(r"[^A-Za-z0-9]+", "_").
    while slug.contains("__") {
        slug = slug.replace("__", "_");
    }
    let slug = slug.trim_matches('_').to_string();
    slug.chars().take(64).collect()
}

fn read_jsonl(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("jsonl line"))
        .collect()
}

#[derive(Default)]
struct Tally {
    compared: usize,
    mismatched: usize,
}

pub fn verify(
    fixtures: Option<PathBuf>,
    stages: Option<PathBuf>,
    frames: u64,
    only: Option<String>,
) -> Result<()> {
    let fixtures = fixtures
        .or_else(|| {
            std::env::var("NESTRIS_FIXTURES_DIR")
                .ok()
                .map(PathBuf::from)
        })
        .context("--fixtures or NESTRIS_FIXTURES_DIR required")?;
    let stages = stages
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/stages"));

    let mut videos: Vec<PathBuf> = std::fs::read_dir(&fixtures)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "mp4").unwrap_or(false))
        .collect();
    videos.sort();

    let mut all_pass = true;
    let mut grand: BTreeMap<&str, Tally> = BTreeMap::new();
    for video in videos {
        let stem = video.file_stem().unwrap().to_string_lossy();
        let slug = slugify(&stem);
        if let Some(filter) = &only
            && !slug.contains(filter.as_str())
        {
            continue;
        }
        let expected = read_jsonl(&stages.join(&slug).join("output.jsonl"));
        if expected.is_empty() {
            eprintln!("{slug}: no oracle dump, skipped");
            continue;
        }

        let mut decoder = VideoDecoder::open(&video.to_string_lossy(), 0.0)?;
        let mut processor = FrameProcessor::new(engine_config(true));
        let mut got: Vec<Value> = Vec::with_capacity(expected.len());
        while let Some(mut frame) = decoder.next_frame()? {
            if got.len() >= frames as usize || got.len() >= expected.len() {
                break;
            }
            // Use the oracle's capture timestamps so the pace metrics are
            // compared on identical clocks (PyAV pts vs seq/fps drift).
            frame.ts = expected[got.len()]["ts"].as_f64().unwrap_or(frame.ts);
            let output = processor.process(&frame);
            got.push(serde_json::from_str(&output.to_json())?);
        }
        if got.len() < expected.len().min(frames as usize) {
            eprintln!(
                "{slug}: decoded only {} of {} frames",
                got.len(),
                expected.len()
            );
        }

        let mut fixture_pass = true;
        let mut report: Vec<String> = Vec::new();
        for field in EXACT_FIELDS {
            let mut tally = Tally::default();
            for (i, (g, e)) in got.iter().zip(expected.iter()).enumerate() {
                let gv = g.pointer(field).unwrap_or(&Value::Null);
                let ev = e.pointer(field).unwrap_or(&Value::Null);
                tally.compared += 1;
                if gv == ev {
                    continue;
                }
                // Transition slack: forgive when the Rust value matches the
                // oracle within +/- TRANSITION_SLACK frames.
                let lo = i.saturating_sub(TRANSITION_SLACK as usize);
                let hi = (i + TRANSITION_SLACK as usize).min(expected.len() - 1);
                let forgiven = (lo..=hi).any(|j| expected[j].pointer(field) == Some(gv));
                if !forgiven {
                    tally.mismatched += 1;
                }
            }
            let rate = if tally.compared == 0 {
                1.0
            } else {
                1.0 - tally.mismatched as f64 / tally.compared as f64
            };
            if rate < PASS_RATE {
                fixture_pass = false;
                report.push(format!(
                    "  FAIL {field}: {:.2}% ({} of {} mismatched)",
                    rate * 100.0,
                    tally.mismatched,
                    tally.compared
                ));
            }
            let g = grand.entry(field).or_default();
            g.compared += tally.compared;
            g.mismatched += tally.mismatched;
        }
        println!(
            "{slug}: {} frames {}",
            got.len(),
            if fixture_pass { "PASS" } else { "FAIL" }
        );
        for line in report {
            println!("{line}");
        }
        all_pass &= fixture_pass;
    }

    println!("\n== overall ==");
    for (field, tally) in &grand {
        let rate = if tally.compared == 0 {
            1.0
        } else {
            1.0 - tally.mismatched as f64 / tally.compared as f64
        };
        println!(
            "{field}: {:.3}% ({}/{} mismatched)",
            rate * 100.0,
            tally.mismatched,
            tally.compared
        );
    }
    if !all_pass {
        bail!("verification policy violated");
    }
    println!("all fixtures PASS");
    Ok(())
}

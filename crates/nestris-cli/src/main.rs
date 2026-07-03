//! Native CLI frontend: `run` (process a source to JSONL), `bench`
//! (per-frame latency), and `verify` (full-pipeline diff against the Python
//! oracle's stage dumps — the Phase-5 gate).

mod capture_ffmpeg;
mod verify;

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use nestris_engine::config::EngineConfig;
use nestris_engine::frame::Frame;
use nestris_engine::processor::FrameProcessor;

use crate::capture_ffmpeg::VideoDecoder;

#[derive(Parser)]
#[command(name = "nestris", about = "NES-Tetris OCR engine (Rust port)")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Process a video source and write per-frame JSONL.
    Run {
        /// Input: video file path (or any ffmpeg-supported source).
        #[arg(long)]
        input: String,
        /// Output JSONL path (stdout when omitted).
        #[arg(long)]
        jsonl: Option<PathBuf>,
        /// Start position in seconds.
        #[arg(long, default_value_t = 0.0)]
        start: f64,
        /// Maximum frames to process (0 = all).
        #[arg(long, default_value_t = 0)]
        frames: u64,
        /// Deterministic oracle-parity mode: inline periodic solves instead
        /// of host-driven background recalibration.
        #[arg(long)]
        oracle_parity: bool,
    },
    /// Per-frame latency benchmark (p50/p90/p99).
    Bench {
        #[arg(long)]
        input: String,
        #[arg(long, default_value_t = 0.0)]
        start: f64,
        #[arg(long, default_value_t = 300)]
        frames: u64,
        #[arg(long, default_value_t = 30)]
        warmup: u64,
    },
    /// Diff the full Rust pipeline against the Python oracle's stage dumps.
    Verify {
        /// Fixtures directory (defaults to $NESTRIS_FIXTURES_DIR).
        #[arg(long)]
        fixtures: Option<PathBuf>,
        /// Stage-dump directory (defaults to testdata/stages next to the exe's
        /// workspace).
        #[arg(long)]
        stages: Option<PathBuf>,
        /// Frames per fixture (matches the dumps).
        #[arg(long, default_value_t = 1200)]
        frames: u64,
        /// Only verify fixtures whose slug contains this substring.
        #[arg(long)]
        only: Option<String>,
    },
}

fn engine_config(oracle_parity: bool) -> EngineConfig {
    let mut cfg = EngineConfig::default();
    if oracle_parity {
        cfg.calibration.background_recalibration = false;
    }
    cfg
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Cmd::Run {
            input,
            jsonl,
            start,
            frames,
            oracle_parity,
        } => run(&input, jsonl, start, frames, oracle_parity),
        Cmd::Bench {
            input,
            start,
            frames,
            warmup,
        } => bench(&input, start, frames, warmup),
        Cmd::Verify {
            fixtures,
            stages,
            frames,
            only,
        } => verify::verify(fixtures, stages, frames, only),
    }
}

fn run(
    input: &str,
    jsonl: Option<PathBuf>,
    start: f64,
    max_frames: u64,
    oracle_parity: bool,
) -> Result<()> {
    let mut decoder = VideoDecoder::open(input, start)?;
    let mut processor = FrameProcessor::new(engine_config(oracle_parity));
    let mut sink: Box<dyn Write> = match &jsonl {
        Some(path) => Box::new(std::io::BufWriter::new(
            std::fs::File::create(path).context("create jsonl")?,
        )),
        None => Box::new(std::io::stdout().lock()),
    };
    let mut count = 0u64;
    while let Some(frame) = decoder.next_frame()? {
        if max_frames > 0 && count >= max_frames {
            break;
        }
        let output = processor.process(&frame);
        sink.write_all(output.to_json().as_bytes())?;
        sink.write_all(b"\n")?;
        count += 1;
    }
    sink.flush()?;
    eprintln!("{count} frames processed");
    Ok(())
}

fn bench(input: &str, start: f64, frames: u64, warmup: u64) -> Result<()> {
    let mut decoder = VideoDecoder::open(input, start)?;
    let mut processor = FrameProcessor::new(engine_config(false));
    let mut times_ms: Vec<f64> = Vec::new();
    let mut fills: Vec<usize> = Vec::new();
    let mut locked_at: Option<u64> = None;
    let mut i = 0u64;
    let mut solver = BackgroundSolveDriver::default();
    while let Some(frame) = decoder.next_frame()? {
        if i >= frames {
            break;
        }
        let t0 = std::time::Instant::now();
        let output = processor.process(&frame);
        solver.drive(&mut processor, &frame);
        let dt = t0.elapsed().as_secs_f64() * 1000.0;
        if locked_at.is_none()
            && processor.lock_state() == nestris_engine::geometry_cal::lock::LockState::Locked
        {
            locked_at = Some(i);
        }
        if i >= warmup {
            times_ms.push(dt);
        }
        fills.push(
            output
                .fields
                .playfield
                .map(|g| g.iter().flatten().filter(|&&v| v != 0).count())
                .unwrap_or(0),
        );
        i += 1;
    }
    if times_ms.is_empty() {
        eprintln!("no frames timed");
        return Ok(());
    }
    times_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pct = |q: f64| times_ms[((times_ms.len() - 1) as f64 * q) as usize];
    println!(
        "{input} @ {start:.1}s: {} timed frames, lock at {:?}",
        times_ms.len(),
        locked_at
    );
    println!(
        "ms/frame: p50={:.2} p90={:.2} p99={:.2} max={:.2}  (p50 fps={:.1})",
        pct(0.50),
        pct(0.90),
        pct(0.99),
        times_ms.last().unwrap(),
        1000.0 / pct(0.50)
    );
    println!(
        "playfield fill: mean={:.1} max={}",
        fills.iter().sum::<usize>() as f64 / fills.len() as f64,
        fills.iter().max().unwrap()
    );
    Ok(())
}

/// Host-side driver for the lock's background-solve protocol: runs the solve
/// synchronously but *paced* (at most one per half second of stream time),
/// mirroring the Python recalibrator's cadence without a thread. The bench
/// includes its cost; `run` in live mode would put this on a worker thread.
#[derive(Default)]
struct BackgroundSolveDriver {
    last_solve_ts: Option<f64>,
}

impl BackgroundSolveDriver {
    fn drive(&mut self, processor: &mut FrameProcessor, frame: &Frame) {
        let lock = processor.lock();
        if !lock.wants_background_solve() {
            return;
        }
        if let Some(last) = self.last_solve_ts
            && frame.ts - last < 0.5
        {
            return;
        }
        self.last_solve_ts = Some(frame.ts);
        let undistort = lock.undistort_map();
        let result = nestris_engine::geometry_cal::calibration::estimate_geometry(
            &frame.image,
            nestris_engine::layout::get_layout(),
            undistort.clone(),
            undistort.is_none(),
            frame.seq as u64,
        );
        processor.lock().offer_solution(result);
    }
}

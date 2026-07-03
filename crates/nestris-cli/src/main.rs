//! Native CLI frontend: `run` (process a source to JSONL/WebSocket), `bench`
//! (per-frame latency), `verify` (full-pipeline diff against the Python
//! oracle's stage dumps — the Phase-5 gate), and `list-devices`.

mod config_load;
mod verify;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use nestris_engine::config::EngineConfig;
use nestris_engine::processor::FrameProcessor;
use nestris_host::capture_ffmpeg::{self, VideoDecoder};
use nestris_host::recalib_thread::RecalibThread;
use nestris_host::sinks::{JsonlSink, MultiSink, Sink, WebSocketSink};

#[derive(Parser)]
#[command(name = "nestris", about = "NES-Tetris OCR engine (Rust port)")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Process a video source and stream per-frame JSON to the sinks.
    Run {
        /// Input: video file path, or `dshow:<device name>` for live capture.
        #[arg(long)]
        input: String,
        /// Output JSONL file path.
        #[arg(long)]
        jsonl: Option<PathBuf>,
        /// WebSocket broadcast address, e.g. `127.0.0.1:8765`.
        #[arg(long)]
        ws: Option<String>,
        /// Also write NDJSON to stdout.
        #[arg(long)]
        ndjson: bool,
        /// Engine config file: .toml, .json, .yaml or .yml (defaults mirror
        /// the Python AppConfig).
        #[arg(long)]
        config: Option<PathBuf>,
        /// Named tuning preset applied over the config file. Available:
        /// `handheld` (continuous geometry tracking for shaky phone footage).
        #[arg(long)]
        preset: Option<String>,
        /// Override a single config field, e.g. `--set fusion.vote_window=7`.
        /// Repeatable; applied after the config file and preset.
        #[arg(long = "set", value_name = "PATH=VALUE")]
        set: Vec<String>,
        /// Start position in seconds (files only).
        #[arg(long, default_value_t = 0.0)]
        start: f64,
        /// Maximum frames to process (0 = all).
        #[arg(long, default_value_t = 0)]
        frames: u64,
        /// Deterministic oracle-parity mode: inline periodic solves instead
        /// of the background recalibration thread.
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
        /// Stage-dump directory (defaults to testdata/stages in the workspace).
        #[arg(long)]
        stages: Option<PathBuf>,
        /// Frames per fixture (matches the dumps).
        #[arg(long, default_value_t = 1200)]
        frames: u64,
        /// Only verify fixtures whose slug contains this substring.
        #[arg(long)]
        only: Option<String>,
    },
    /// List DirectShow capture devices (Windows).
    ListDevices,
}

pub fn engine_config(oracle_parity: bool) -> EngineConfig {
    let mut cfg = EngineConfig::default();
    if oracle_parity {
        cfg.calibration.background_recalibration = false;
    }
    cfg
}

fn load_config(
    path: Option<&PathBuf>,
    preset: Option<&str>,
    overrides: &[String],
    oracle_parity: bool,
) -> Result<EngineConfig> {
    let mut cfg = config_load::load(path.map(|p| p.as_path()), preset, overrides)?;
    if oracle_parity {
        cfg.calibration.background_recalibration = false;
    }
    Ok(cfg)
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Cmd::Run {
            input,
            jsonl,
            ws,
            ndjson,
            config,
            preset,
            set,
            start,
            frames,
            oracle_parity,
        } => run(RunArgs {
            input,
            jsonl,
            ws,
            ndjson,
            config,
            preset,
            set,
            start,
            frames,
            oracle_parity,
        }),
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
        Cmd::ListDevices => {
            print!("{}", capture_ffmpeg::list_devices()?);
            Ok(())
        }
    }
}

struct RunArgs {
    input: String,
    jsonl: Option<PathBuf>,
    ws: Option<String>,
    ndjson: bool,
    config: Option<PathBuf>,
    preset: Option<String>,
    set: Vec<String>,
    start: f64,
    frames: u64,
    oracle_parity: bool,
}

fn run(args: RunArgs) -> Result<()> {
    let cfg = load_config(
        args.config.as_ref(),
        args.preset.as_deref(),
        &args.set,
        args.oracle_parity,
    )?;
    let background = cfg.calibration.background_recalibration;
    let mut decoder = VideoDecoder::open(&args.input, args.start)?;
    let mut processor = FrameProcessor::new(cfg);

    let mut sinks: Vec<Box<dyn Sink>> = Vec::new();
    if let Some(path) = &args.jsonl {
        sinks.push(Box::new(JsonlSink::to_file(path)?));
    }
    if let Some(addr) = &args.ws {
        sinks.push(Box::new(WebSocketSink::bind(addr)?));
    }
    if args.ndjson || sinks.is_empty() {
        sinks.push(Box::new(JsonlSink::to_stdout()));
    }
    let mut sink = MultiSink { sinks };

    let mut recalib = background.then(RecalibThread::start);
    let mut count = 0u64;
    while let Some(frame) = decoder.next_frame()? {
        if args.frames > 0 && count >= args.frames {
            break;
        }
        let output = processor.process(&frame);
        if let Some(recalib) = &mut recalib {
            recalib.drive(&mut processor, &frame);
        }
        sink.publish(&output.to_json());
        count += 1;
    }
    sink.flush();
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
    // Bench measures the hot path the way production runs it: the solve is
    // on the worker thread, only snapshot/offer costs land in the loop.
    let mut recalib = RecalibThread::start();
    while let Some(frame) = decoder.next_frame()? {
        if i >= frames {
            break;
        }
        let t0 = std::time::Instant::now();
        let output = processor.process(&frame);
        recalib.drive(&mut processor, &frame);
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

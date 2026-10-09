//! Native CLI frontend: `run` (process a source to JSONL/WebSocket), `bench`
//! (per-frame latency), `compare` (field agreement of two runs), `verify`
//! (full-pipeline diff against the Python oracle's stage dumps — the
//! Phase-5 gate), and `list-devices`.

mod compare;
mod config_load;
mod screens;
mod verify;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use nestris_engine::config::EngineConfig;
use nestris_engine::processor::FrameProcessor;
use nestris_host::capture_ffmpeg;
use nestris_host::recalib_thread::RecalibThread;
use nestris_host::recording::{self, RecordingSink};
use nestris_host::sinks::{JsonlSink, MultiSink, Sink, WebSocketSink};
use nestris_ngf::recorder::{GameRecorder, RecorderConfig};
use screens::DecodeArgs;

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
        /// Input: video file path, `dshow:<device name>` (Windows) or
        /// `v4l2:<device path>` (Linux) for live capture.
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
        /// Disable the automatic per-game NGF recording.
        #[arg(long)]
        no_record: bool,
        /// Recording output directory (default: Documents\nestris-recordings).
        #[arg(long)]
        record_dir: Option<PathBuf>,
        /// Save recordings as plain .ngf instead of gzipped .ngf.gz.
        #[arg(long)]
        record_raw: bool,
        /// Also record games already in progress when capture starts.
        #[arg(long)]
        record_partial: bool,
        #[command(flatten)]
        decode: DecodeArgs,
    },
    /// Replay a recorded .ngf / .ngf.gz game as schema-v4 output frames.
    Replay {
        /// Path to the recording (.ngf, .ngf.gz, or a crash-left .ngf.part).
        file: PathBuf,
        /// Output JSONL file path.
        #[arg(long)]
        jsonl: Option<PathBuf>,
        /// WebSocket broadcast address, e.g. `127.0.0.1:8765`.
        #[arg(long)]
        ws: Option<String>,
        /// Playback speed multiplier; 0 = as fast as possible (default).
        #[arg(long, default_value_t = 0.0)]
        speed: f64,
        /// Omit the extended dashboard stats block from output frames.
        #[arg(long)]
        no_extended: bool,
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
        /// Acquisition benchmark: reopen the source N times with the GUI
        /// acquisition config (background solver + downscaled candidate
        /// detection) and report time-to-lock plus per-frame latency while
        /// unlocked (the "does the preview stutter" number).
        #[arg(long, default_value_t = 0)]
        acquire: u64,
        #[command(flatten)]
        decode: DecodeArgs,
        /// Engine config overrides (`path=value`, repeatable).
        #[arg(long = "set", value_name = "PATH=VALUE")]
        set: Vec<String>,
        /// Print the result as one JSON object.
        #[arg(long)]
        json: bool,
        /// Only decode (no engine): the maximum rate ffmpeg delivers.
        #[arg(long)]
        decode_only: bool,
    },
    /// Per-field agreement of two `run --jsonl` outputs of the same source
    /// (e.g. downscaled against native).
    Compare {
        /// Reference run.
        #[arg(long)]
        a: PathBuf,
        /// Run to check.
        #[arg(long)]
        b: PathBuf,
        /// Frames a value may lag or lead and still agree.
        #[arg(long, default_value_t = compare::DEFAULT_SLACK)]
        slack: usize,
        #[arg(long)]
        json: bool,
    },
    /// Screen-signature tools: build references, evaluate, dump frames.
    Screens {
        #[command(subcommand)]
        cmd: screens::ScreensCmd,
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
    /// List capture devices (DirectShow on Windows, V4L2 elsewhere).
    ListDevices,
}

pub fn engine_config(oracle_parity: bool) -> EngineConfig {
    let mut cfg = EngineConfig::default();
    if oracle_parity {
        cfg.calibration.background_recalibration = false;
        cfg.screen.mode = "legacy".into();
        cfg.fusion.new_game_confirm_frames = 0;
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
        cfg.screen.mode = "legacy".into();
        cfg.fusion.new_game_confirm_frames = 0;
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
            no_record,
            record_dir,
            record_raw,
            record_partial,
            decode,
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
            no_record,
            record_dir,
            record_raw,
            record_partial,
            decode,
        }),
        Cmd::Replay {
            file,
            jsonl,
            ws,
            speed,
            no_extended,
        } => replay(&file, jsonl.as_deref(), ws.as_deref(), speed, !no_extended),
        Cmd::Bench {
            input,
            start,
            frames,
            warmup,
            acquire,
            decode,
            set,
            json,
            decode_only,
        } => {
            if acquire > 0 {
                bench_acquire(&input, start, frames, acquire, &decode)
            } else {
                let cfg = config_load::load(None, None, &set)?;
                let opts = BenchOptions {
                    frames,
                    warmup,
                    json,
                    decode_only,
                };
                bench(&input, start, &decode, cfg, &opts)
            }
        }
        Cmd::Compare { a, b, slack, json } => {
            let result = compare::compare(&a, &b, slack)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                compare::print(&result);
            }
            Ok(())
        }
        Cmd::Screens { cmd } => screens::run(cmd),
        Cmd::Verify {
            fixtures,
            stages,
            frames,
            only,
        } => verify::verify(fixtures, stages, frames, only),
        Cmd::ListDevices => {
            if cfg!(windows) {
                print!("{}", capture_ffmpeg::list_devices()?);
            } else {
                for device in capture_ffmpeg::list_v4l2_devices() {
                    println!("v4l2:{}", device.display());
                }
            }
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
    no_record: bool,
    record_dir: Option<PathBuf>,
    record_raw: bool,
    record_partial: bool,
    decode: DecodeArgs,
}

fn run(args: RunArgs) -> Result<()> {
    let cfg = load_config(
        args.config.as_ref(),
        args.preset.as_deref(),
        &args.set,
        args.oracle_parity,
    )?;
    let background = cfg.calibration.background_recalibration;
    let mut decoder = screens::open(&args.input, args.start, &args.decode)?;
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

    // Per-game NGF recording (on by default; --no-record to disable).
    let mut recording = if args.no_record {
        None
    } else {
        let dir = args
            .record_dir
            .clone()
            .unwrap_or_else(recording::default_recording_dir);
        let sink = RecordingSink::new(dir, !args.record_raw)?;
        eprintln!("recording games to {}", sink.dir().display());
        let recorder = GameRecorder::new(RecorderConfig {
            record_partial: args.record_partial,
            ..Default::default()
        });
        Some((recorder, sink))
    };

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
        if let Some((recorder, rec_sink)) = &mut recording {
            let events = recorder.push(&output);
            for path in rec_sink.handle(recorder, events)? {
                eprintln!("recording saved: {}", path.display());
            }
        }
        count += 1;
    }
    if let Some((recorder, rec_sink)) = &mut recording
        && let Some(path) = rec_sink.finalize(recorder)?
    {
        eprintln!("recording saved: {}", path.display());
    }
    sink.flush();
    eprintln!("{count} frames processed");
    Ok(())
}

/// Headless replay: re-emit a recorded game as schema-v4 JSONL/WebSocket
/// frames, statistics recomputed, paced by the recorded timestamps.
fn replay(
    file: &std::path::Path,
    jsonl: Option<&std::path::Path>,
    ws: Option<&str>,
    speed: f64,
    extended: bool,
) -> Result<()> {
    let replay_file = nestris_ngf::replay::ReplayFile::load(file)
        .with_context(|| format!("load replay {}", file.display()))?;
    let mut engine = nestris_ngf::replay::ReplayEngine::new(replay_file);
    engine.extended_stats = extended;

    let mut sinks: Vec<Box<dyn Sink>> = Vec::new();
    if let Some(path) = jsonl {
        sinks.push(Box::new(JsonlSink::to_file(path)?));
    }
    if let Some(addr) = ws {
        sinks.push(Box::new(WebSocketSink::bind(addr)?));
    }
    if sinks.is_empty() {
        sinks.push(Box::new(JsonlSink::to_stdout()));
    }
    let mut sink = MultiSink { sinks };

    let count = engine.frame_count();
    for index in 0..count {
        let output = engine.output_at(index);
        sink.publish(&output.to_json());
        if speed > 0.0 && index + 1 < count {
            let dt_ms = engine
                .ctime_ms_at(index + 1)
                .saturating_sub(engine.ctime_ms_at(index));
            std::thread::sleep(std::time::Duration::from_secs_f64(
                dt_ms as f64 / 1000.0 / speed,
            ));
        }
    }
    sink.flush();
    eprintln!(
        "{count} frames replayed ({:.1}s of play)",
        engine.duration_ms() as f64 / 1000.0
    );
    Ok(())
}

struct BenchOptions {
    frames: u64,
    warmup: u64,
    json: bool,
    decode_only: bool,
}

/// The `q` quantile (0..=1) of an ascending sample.
fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f64 * q).round() as usize]
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn bench(
    input: &str,
    start: f64,
    decode: &DecodeArgs,
    cfg: EngineConfig,
    opts: &BenchOptions,
) -> Result<()> {
    let mut decoder = screens::open(input, start, decode)?;
    let (width, height) = (decoder.info().width, decoder.info().height);
    let mut processor = FrameProcessor::new(cfg);
    let mut times_ms: Vec<f64> = Vec::new();
    // Time spent waiting for the next frame on the pipe: ffmpeg decodes in
    // its own process, so this is what decoding costs the loop.
    let mut wait_ms: Vec<f64> = Vec::new();
    let mut fills: Vec<usize> = Vec::new();
    let mut locked_at: Option<u64> = None;
    let mut i = 0u64;
    // Bench measures the hot path the way production runs it: the solve is
    // on the worker thread, only snapshot/offer costs land in the loop.
    let mut recalib = RecalibThread::start();
    let mut timed_since: Option<std::time::Instant> = None;
    while i < opts.frames {
        if i == opts.warmup {
            timed_since = Some(std::time::Instant::now());
        }
        let t_read = std::time::Instant::now();
        let Some(frame) = decoder.next_frame()? else {
            break;
        };
        let read_ms = t_read.elapsed().as_secs_f64() * 1000.0;
        if opts.decode_only {
            if i >= opts.warmup {
                wait_ms.push(read_ms);
            }
            i += 1;
            continue;
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
        if i >= opts.warmup {
            times_ms.push(dt);
            wait_ms.push(read_ms);
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
    let timed = wait_ms.len();
    if timed == 0 {
        eprintln!("no frames timed");
        return Ok(());
    }
    let wall_s = timed_since.map_or(0.0, |t| t.elapsed().as_secs_f64());
    let wall_fps = timed as f64 / wall_s.max(1e-9);
    times_ms.sort_by(|a, b| a.total_cmp(b));
    wait_ms.sort_by(|a, b| a.total_cmp(b));
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len().max(1) as f64;
    let fill_mean =
        (!fills.is_empty()).then(|| fills.iter().sum::<usize>() as f64 / fills.len() as f64);
    if opts.json {
        let engine = (!times_ms.is_empty()).then(|| {
            serde_json::json!({
                "p50": round2(percentile(&times_ms, 0.50)),
                "p90": round2(percentile(&times_ms, 0.90)),
                "p95": round2(percentile(&times_ms, 0.95)),
                "p99": round2(percentile(&times_ms, 0.99)),
                "max": round2(*times_ms.last().unwrap()),
                "mean": round2(mean(&times_ms)),
            })
        });
        let report = serde_json::json!({
            "input": input,
            "start": start,
            "size": format!("{width}x{height}"),
            "scale": decode.scale.map(|(w, h)| format!("{w}x{h}")),
            "lowres": decode.lowres,
            "decode_only": opts.decode_only,
            "timed_frames": timed,
            "wall_fps": round2(wall_fps),
            "engine_ms": engine,
            "wait_ms": {
                "p50": round2(percentile(&wait_ms, 0.50)),
                "p95": round2(percentile(&wait_ms, 0.95)),
                "mean": round2(mean(&wait_ms)),
            },
            "lock_frame": locked_at,
            "playfield_fill_mean": fill_mean.map(round2),
        });
        println!("{report}");
        return Ok(());
    }
    println!(
        "{input} @ {start:.1}s ({width}x{height}): {timed} timed frames, lock at {locked_at:?}"
    );
    if !times_ms.is_empty() {
        println!(
            "engine ms/frame: p50={:.2} p90={:.2} p99={:.2} max={:.2}  (p50 fps={:.1})",
            percentile(&times_ms, 0.50),
            percentile(&times_ms, 0.90),
            percentile(&times_ms, 0.99),
            times_ms.last().unwrap(),
            1000.0 / percentile(&times_ms, 0.50).max(1e-9)
        );
    }
    println!(
        "frame wait ms: p50={:.2} p95={:.2} mean={:.2}  wall fps={:.1}",
        percentile(&wait_ms, 0.50),
        percentile(&wait_ms, 0.95),
        mean(&wait_ms),
        wall_fps
    );
    if let Some(fill) = fill_mean {
        println!(
            "playfield fill: mean={fill:.1} max={}",
            fills.iter().max().unwrap()
        );
    }
    Ok(())
}

/// Acquisition benchmark: `runs` cold starts with the GUI acquisition config
/// (background solver + 640-wide candidate detection). Reports wall time to
/// `Locked` and the per-frame pipeline latency while unlocked — the number
/// that decides whether a live preview stutters during acquisition.
fn bench_acquire(
    input: &str,
    start: f64,
    max_frames: u64,
    runs: u64,
    decode: &DecodeArgs,
) -> Result<()> {
    let mut lock_times_s: Vec<f64> = Vec::new();
    let mut unlocked_ms: Vec<f64> = Vec::new();
    for run in 0..runs {
        let mut decoder = screens::open(input, start, decode)?;
        // Pace file input at source fps like a live device: time-to-lock is
        // wall-clock bound (background solves run while frames flow), so an
        // unpaced file would starve the solver of wall time.
        let fps = decoder.info().fps.max(1.0);
        let mut cfg = engine_config(false);
        cfg.calibration.background_acquisition = true;
        cfg.calibration.acquire_downscale_width = 640;
        let mut processor = FrameProcessor::new(cfg);
        let mut recalib = RecalibThread::start();
        let started = std::time::Instant::now();
        let mut locked_at: Option<(u64, f64)> = None;
        let mut i = 0u64;
        while let Some(mut frame) = decoder.next_frame()? {
            if i >= max_frames {
                break;
            }
            let due = i as f64 / fps;
            let now = started.elapsed().as_secs_f64();
            if now < due {
                std::thread::sleep(std::time::Duration::from_secs_f64(due - now));
            }
            // Pacing hint for RecalibThread's interval check (file frames
            // carry seq/fps timestamps already; keep them).
            frame.ts = started.elapsed().as_secs_f64();
            let t0 = std::time::Instant::now();
            processor.process(&frame);
            recalib.drive(&mut processor, &frame);
            let dt = t0.elapsed().as_secs_f64() * 1000.0;
            let locked =
                processor.lock_state() == nestris_engine::geometry_cal::lock::LockState::Locked;
            if !locked && locked_at.is_none() {
                unlocked_ms.push(dt);
            }
            if locked && locked_at.is_none() {
                locked_at = Some((i, started.elapsed().as_secs_f64()));
                break;
            }
            i += 1;
        }
        match locked_at {
            Some((frame, secs)) => {
                lock_times_s.push(secs);
                println!("run {run}: locked after {frame} frames ({secs:.2}s)");
            }
            None => println!("run {run}: no lock within {max_frames} frames"),
        }
    }
    if !unlocked_ms.is_empty() {
        unlocked_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let pct = |q: f64| unlocked_ms[((unlocked_ms.len() - 1) as f64 * q) as usize];
        println!(
            "unlocked pipeline ms/frame: p50={:.2} p99={:.2} max={:.2} ({} frames)",
            pct(0.50),
            pct(0.99),
            unlocked_ms.last().unwrap(),
            unlocked_ms.len()
        );
    }
    if !lock_times_s.is_empty() {
        println!(
            "time to lock: mean={:.2}s min={:.2}s max={:.2}s ({}/{} runs locked)",
            lock_times_s.iter().sum::<f64>() / lock_times_s.len() as f64,
            lock_times_s.iter().cloned().fold(f64::INFINITY, f64::min),
            lock_times_s.iter().cloned().fold(0.0, f64::max),
            lock_times_s.len(),
            runs
        );
    }
    Ok(())
}

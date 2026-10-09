//! `nestris-station bench`: the station's capture + engine pipeline on this
//! machine, without MQTT, sessions or the reader. A file input is paced at
//! its real frame rate and drops frames like a live device when the engine
//! falls behind, so the numbers match what the station would do live.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use nestris_engine::geometry_cal::lock::LockState;
use nestris_engine::processor::FrameProcessor;
use nestris_host::capture_supervisor::{CaptureMsg, CaptureStatus, CaptureSupervisor, Recv};
use nestris_host::recalib_thread::RecalibThread;
use serde::Serialize;

use crate::config::StationConfig;
use crate::telemetry::{Perf, Telemetry, percentile};

pub struct BenchArgs {
    /// Real frame rate of a file muxed with a wrong one.
    pub fps: Option<f64>,
    pub start: f64,
    pub seconds: f64,
    pub json: bool,
}

#[derive(Serialize)]
struct Summary {
    input: String,
    size: Option<String>,
    lowres: u8,
    seconds: f64,
    capture_fps: f64,
    fps: f64,
    drop_rate: f64,
    dropped: u64,
    missing: u64,
    engine_ms_p50: Option<f64>,
    engine_ms_p95: Option<f64>,
    frame_age_ms_p95: Option<f64>,
    cpu_pct: Option<f64>,
    /// Seconds until the geometry first locked.
    lock_after_s: Option<f64>,
    /// Share of processed frames with a locked geometry.
    locked_share: f64,
    windows: Vec<Perf>,
}

pub fn run(cfg: StationConfig, args: &BenchArgs, term: &AtomicBool) -> Result<()> {
    let mut sup = cfg.supervisor();
    sup.file.fps = args.fps;
    sup.file_start = args.start;
    sup.drop_paced_files = true;
    sup.pace_files = true;
    sup.loop_files = false;
    let input = sup.input.clone();
    eprintln!(
        "bench: {input}, engine input {}x{} lowres {}, {:.0} s",
        cfg.capture.scale_width, cfg.capture.scale_height, cfg.capture.lowres, args.seconds
    );
    let supervisor = CaptureSupervisor::start(sup);
    let mut processor = FrameProcessor::new(cfg.engine.clone());
    let mut recalib = cfg
        .engine
        .calibration
        .background_recalibration
        .then(RecalibThread::start);
    let mut telemetry = Telemetry::new((cfg.capture.fps > 0.0).then_some(cfg.capture.fps));
    let mut windows: Vec<Perf> = Vec::new();
    let mut engine_all: Vec<f64> = Vec::new();
    let mut age_all: Vec<f64> = Vec::new();
    let (mut processed, mut locked) = (0u64, 0u64);
    let mut lock_after: Option<f64> = None;
    let mut started: Option<Instant> = None;
    let limit = Duration::from_secs_f64(args.seconds.max(1.0));
    while !term.load(Ordering::Relaxed) && started.is_none_or(|t| t.elapsed() < limit) {
        match supervisor.recv(Duration::from_millis(200)) {
            Recv::Msg(CaptureMsg::Frame(frame, read_at)) => {
                let t0 = Instant::now();
                started.get_or_insert(t0);
                processor.process(&frame);
                if let Some(r) = &mut recalib {
                    r.drive(&mut processor, &frame);
                }
                let engine_ms = t0.elapsed().as_secs_f64() * 1000.0;
                let age_ms = read_at.elapsed().as_secs_f64() * 1000.0;
                telemetry.frame(
                    Some(engine_ms),
                    Some(age_ms),
                    Some((frame.image.width, frame.image.height)),
                );
                engine_all.push(engine_ms);
                age_all.push(age_ms);
                processed += 1;
                if processor.lock_state() == LockState::Locked {
                    locked += 1;
                    if lock_after.is_none() {
                        lock_after = started.map(|t| t.elapsed().as_secs_f64());
                    }
                }
            }
            Recv::Msg(CaptureMsg::Status(CaptureStatus::Ended)) | Recv::Closed => break,
            Recv::Msg(CaptureMsg::Status(CaptureStatus::Failed { reason, .. })) => {
                eprintln!("capture failed: {reason}");
            }
            Recv::Msg(CaptureMsg::Status(_)) | Recv::Timeout => {}
        }
        let before = telemetry.perf().cloned();
        telemetry.roll(supervisor.stats());
        if let Some(p) = telemetry.perf().filter(|p| Some(*p) != before.as_ref()) {
            if !args.json {
                eprintln!(
                    "{:5.1}/{:5.1} fps  drop {:5.1}%  missing {:3}  engine p50 {:6.2} p95 {:6.2} ms  age p95 {:6.2} ms  cpu {}",
                    p.fps,
                    p.capture_fps,
                    p.drop_rate * 100.0,
                    p.missing,
                    p.engine_ms_p50.unwrap_or(0.0),
                    p.engine_ms_p95.unwrap_or(0.0),
                    p.frame_age_ms_p95.unwrap_or(0.0),
                    p.cpu_pct.map_or("-".into(), |c| format!("{c:.0}%")),
                );
            }
            windows.push(p.clone());
        }
    }
    let stats = supervisor.stats();
    supervisor.stop();
    let secs = started.map_or(0.0, |t| t.elapsed().as_secs_f64()).max(1e-9);
    engine_all.sort_by(|a, b| a.total_cmp(b));
    age_all.sort_by(|a, b| a.total_cmp(b));
    let round2 = |v: f64| (v * 100.0).round() / 100.0;
    let cpu: Vec<f64> = windows.iter().filter_map(|w| w.cpu_pct).collect();
    let summary = Summary {
        input,
        size: windows.last().and_then(|w| w.size.clone()),
        lowres: cfg.capture.lowres,
        seconds: round2(secs),
        capture_fps: round2(stats.delivered as f64 / secs),
        fps: round2(processed as f64 / secs),
        drop_rate: round2(100.0 * stats.dropped as f64 / stats.delivered.max(1) as f64) / 100.0,
        dropped: stats.dropped,
        missing: stats.missing,
        engine_ms_p50: percentile(&engine_all, 0.50).map(round2),
        engine_ms_p95: percentile(&engine_all, 0.95).map(round2),
        frame_age_ms_p95: percentile(&age_all, 0.95).map(round2),
        cpu_pct: (!cpu.is_empty()).then(|| round2(cpu.iter().sum::<f64>() / cpu.len() as f64)),
        lock_after_s: lock_after.map(round2),
        locked_share: round2(locked as f64 / processed.max(1) as f64),
        windows,
    };
    if args.json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        println!(
            "{} frames in {:.1} s: {:.1} of {:.1} fps processed, {:.2}% dropped ({}), {} missing",
            processed,
            summary.seconds,
            summary.fps,
            summary.capture_fps,
            summary.drop_rate * 100.0,
            summary.dropped,
            summary.missing
        );
        println!(
            "engine ms p50 {:?} p95 {:?}, frame age p95 {:?} ms, cpu {:?}%",
            summary.engine_ms_p50, summary.engine_ms_p95, summary.frame_age_ms_p95, summary.cpu_pct
        );
        println!(
            "lock after {:?} s, locked {:.0}% of frames",
            summary.lock_after_s,
            summary.locked_share * 100.0
        );
    }
    Ok(())
}

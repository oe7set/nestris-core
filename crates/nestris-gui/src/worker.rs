//! The GUI's pipeline thread: decode → process → recalibrate → publish
//! snapshots to the UI. Mirrors the CLI's `run` loop plus transport control
//! (pause / speed / restart-based seek — the ffmpeg pipe cannot seek in
//! place, so a seek reopens the decoder at the target position).

use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use nestris_engine::config::EngineConfig;
use nestris_engine::frame::Frame;
use nestris_engine::output::OutputFrame;
use nestris_engine::processor::FrameProcessor;
use nestris_host::capture_ffmpeg::VideoDecoder;
use nestris_host::recalib_thread::RecalibThread;
use nestris_host::recording::{RecordingSink, default_recording_dir};
use nestris_host::sinks::{JsonlSink, MultiSink, Sink, WebSocketSink};
use nestris_ngf::recorder::{GameRecorder, RecorderConfig};
use nestris_vision::homography::{mat3_inv, project};

/// UI → worker control.
pub enum Cmd {
    Pause(bool),
    SetSpeed(f32),
    Seek(f64),
    ResetLock,
    Stop,
}

/// One per-frame snapshot for the UI (latest wins).
pub struct GuiUpdate {
    pub output: OutputFrame,
    /// Downscaled raw frame as RGBA + its size.
    pub raw_rgba: Vec<u8>,
    pub raw_w: usize,
    pub raw_h: usize,
    /// Canonical 256×240 RGBA (when locked).
    pub canon_rgba: Option<Vec<u8>>,
    /// Detected-playfield quad in downscaled raw coordinates.
    pub lock_quad: Option<[(f32, f32); 4]>,
    pub lock_state: &'static str,
    pub position_s: f64,
    pub duration_s: Option<f64>,
    pub fps: f32,
}

/// Host-side sink options (mirrors the CLI flags).
#[derive(Clone)]
pub struct SinkOptions {
    pub jsonl_path: Option<std::path::PathBuf>,
    pub ws_addr: Option<String>,
    /// Record every detected game as an .ngf.gz file.
    pub record: bool,
    /// Recording directory (`None` = Documents\nestris-recordings).
    pub record_dir: Option<std::path::PathBuf>,
}

impl Default for SinkOptions {
    fn default() -> Self {
        Self {
            jsonl_path: None,
            ws_addr: None,
            record: true,
            record_dir: None,
        }
    }
}

pub struct WorkerHandle {
    pub cmd: Sender<Cmd>,
    pub updates: Receiver<Box<GuiUpdate>>,
    pub join: JoinHandle<()>,
}

/// Maximum width of the raw preview sent to the UI (texture-upload budget).
const RAW_PREVIEW_MAX_W: usize = 960;

pub fn spawn(
    input: String,
    config: EngineConfig,
    sinks: SinkOptions,
    start_s: f64,
    speed: f32,
) -> WorkerHandle {
    let (cmd_tx, cmd_rx) = channel::<Cmd>();
    let (update_tx, update_rx) = channel::<Box<GuiUpdate>>();
    let join = std::thread::spawn(move || {
        if let Err(err) = run(&input, config, sinks, start_s, speed, &cmd_rx, &update_tx) {
            eprintln!("worker: {err:#}");
        }
    });
    WorkerHandle {
        cmd: cmd_tx,
        updates: update_rx,
        join,
    }
}

#[allow(clippy::too_many_lines)]
fn run(
    input: &str,
    config: EngineConfig,
    sink_opts: SinkOptions,
    mut start_s: f64,
    mut speed: f32,
    cmd_rx: &Receiver<Cmd>,
    update_tx: &Sender<Box<GuiUpdate>>,
) -> anyhow::Result<()> {
    let background = config.calibration.background_recalibration;
    let mut decoder = VideoDecoder::open(input, start_s)?;
    let duration_s = decoder.info().duration_s;
    let fps = decoder.info().fps.max(1.0);
    let live = input.starts_with("dshow:");
    let mut processor = FrameProcessor::new(config);
    let mut recalib = background.then(RecalibThread::start);

    let mut sinks: Vec<Box<dyn Sink>> = Vec::new();
    if let Some(path) = &sink_opts.jsonl_path {
        sinks.push(Box::new(JsonlSink::to_file(path)?));
    }
    if let Some(addr) = &sink_opts.ws_addr {
        sinks.push(Box::new(WebSocketSink::bind(addr)?));
    }
    let mut sink = MultiSink { sinks };

    let mut recording = if sink_opts.record {
        let dir = sink_opts
            .record_dir
            .clone()
            .unwrap_or_else(default_recording_dir);
        let rec_sink = RecordingSink::new(dir, true)?;
        Some((GameRecorder::new(RecorderConfig::default()), rec_sink))
    } else {
        None
    };

    let mut paused = false;
    let mut frame_idx: u64 = 0;
    let mut discontinuity = false;
    let mut fps_counter = 0u32;
    let mut fps_value = 0.0f32;
    let mut fps_window = Instant::now();
    let mut pace_anchor = Instant::now();
    let mut pace_frames = 0u64;

    'pipeline: loop {
        // Drain control commands.
        loop {
            match cmd_rx.try_recv() {
                Ok(Cmd::Stop) => break 'pipeline,
                Ok(Cmd::Pause(p)) => {
                    paused = p;
                    pace_anchor = Instant::now();
                    pace_frames = 0;
                }
                Ok(Cmd::SetSpeed(s)) => {
                    speed = s;
                    pace_anchor = Instant::now();
                    pace_frames = 0;
                }
                Ok(Cmd::Seek(pos)) => {
                    if !live {
                        decoder = VideoDecoder::open(input, pos)?;
                        start_s = pos;
                        frame_idx = 0;
                        discontinuity = true;
                        pace_anchor = Instant::now();
                        pace_frames = 0;
                        // A seek tears the game's frame continuity: drop the
                        // partial recording instead of saving a spliced game.
                        if let Some((recorder, rec_sink)) = &mut recording {
                            rec_sink.abort(recorder);
                        }
                    }
                }
                Ok(Cmd::ResetLock) => processor.reset_lock(),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break 'pipeline,
            }
        }
        if paused {
            std::thread::sleep(Duration::from_millis(30));
            continue;
        }

        let Some(mut frame) = decoder.next_frame()? else {
            break 'pipeline; // end of stream
        };
        if !live {
            frame.ts = start_s + frame_idx as f64 / fps;
            frame.seq = frame_idx as i64;
        }
        frame.discontinuity = std::mem::take(&mut discontinuity);
        frame_idx += 1;

        // Real-time pacing for files (speed <= 0 = as fast as possible).
        if !live && speed > 0.0 {
            pace_frames += 1;
            let due =
                pace_anchor + Duration::from_secs_f64(pace_frames as f64 / (fps * speed as f64));
            let now = Instant::now();
            if due > now {
                std::thread::sleep(due - now);
            }
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

        fps_counter += 1;
        if fps_window.elapsed() >= Duration::from_secs(1) {
            fps_value = fps_counter as f32 / fps_window.elapsed().as_secs_f32();
            fps_counter = 0;
            fps_window = Instant::now();
        }

        let update = build_update(&mut processor, &frame, output, duration_s, fps_value);
        if update_tx.send(Box::new(update)).is_err() {
            break 'pipeline; // UI gone
        }
    }

    // Graceful shutdown: persist a still-running recording.
    if let Some((recorder, rec_sink)) = &mut recording
        && let Some(path) = rec_sink.finalize(recorder)?
    {
        eprintln!("recording saved: {}", path.display());
    }
    Ok(())
}

fn build_update(
    processor: &mut FrameProcessor,
    frame: &Frame,
    output: OutputFrame,
    duration_s: Option<f64>,
    fps: f32,
) -> GuiUpdate {
    // Downscale the raw frame by integer stride (nearest) to bound texture cost.
    let src = &frame.image;
    let stride = src.width.div_ceil(RAW_PREVIEW_MAX_W).max(1);
    let (rw, rh) = (src.width.div_ceil(stride), src.height.div_ceil(stride));
    let mut raw_rgba = Vec::with_capacity(rw * rh * 4);
    for y in (0..src.height).step_by(stride) {
        for x in (0..src.width).step_by(stride) {
            let px = src.pixel(x, y);
            raw_rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
        }
    }

    let canon_rgba = processor.last_canonical().map(|canon| {
        let mut out = Vec::with_capacity(canon.width * canon.height * 4);
        for px in canon.data.chunks_exact(3) {
            out.extend_from_slice(&[px[2], px[1], px[0], 255]);
        }
        out
    });

    let lock_quad = processor.lock().rectifier().and_then(|rectifier| {
        let inv = mat3_inv(rectifier.matrix())?;
        let corners = [(0.0, 0.0), (256.0, 0.0), (256.0, 240.0), (0.0, 240.0)];
        Some(corners.map(|(x, y)| {
            let (sx, sy) = project(&inv, x, y);
            ((sx / stride as f64) as f32, (sy / stride as f64) as f32)
        }))
    });

    GuiUpdate {
        lock_state: processor.lock_state().name(),
        output,
        raw_rgba,
        raw_w: rw,
        raw_h: rh,
        canon_rgba,
        lock_quad,
        position_s: frame.ts,
        duration_s,
        fps,
    }
}

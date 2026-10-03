//! `nestris-station`: the headless tournament station daemon.
//!
//! Captures the NES picture from a USB capture device, runs the recognition
//! engine, attributes games to the player on the RFID reader, detects the
//! Select score cheat, validates every result and publishes everything to
//! the host over MQTT. Built to run unattended under systemd: every
//! component reconnects on its own, and the watchdog restarts the process
//! if the main loop ever wedges. See `docs/STATION.md`.

mod config;
mod health;
mod mqtt;
mod payload;
mod rfid;
mod session;
mod spool;
mod upload;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use nestris_engine::enums::GameState;
use nestris_engine::output::OutputFrame;
use nestris_engine::processor::FrameProcessor;
use nestris_host::capture_ffmpeg;
use nestris_host::capture_supervisor::{CaptureMsg, CaptureStatus, CaptureSupervisor, Recv};
use nestris_host::recalib_thread::RecalibThread;
use nestris_host::recording::RecordingSink;
use nestris_ngf::recorder::{GameRecorder, RecorderConfig};
use tracing::{error, info, warn};

use crate::config::{DEFAULT_CONFIG, StationConfig};
use crate::health::Watchdog;
use crate::mqtt::{MqttLink, topic};
use crate::payload::{PlayerPayload, Status};
use crate::rfid::{RfidReader, RfidSnapshot};
use crate::session::{SessionEvent, SessionTracker, game_state_str};
use crate::spool::Spool;
use crate::upload::{UploadJob, UploadQueue, Uploader, UploaderConfig};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const PRUNE_EVERY: Duration = Duration::from_secs(3600);
const STATUS_MIN_GAP: Duration = Duration::from_millis(500);
/// Commands from `<base>/cmd` that are forwarded to the RFID reader
/// (protocol v2, `nestris-rfid-reader/docs/PROTOCOL.md`): text on its display,
/// writing a card, its display settings. `reboot`/`hello` stay with the station.
const READER_COMMANDS: [&str; 3] = ["show", "write", "config"];

#[derive(Parser)]
#[command(
    name = "nestris-station",
    version,
    about = "Headless NES-Tetris tournament station"
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(clap::Args)]
struct ConfigArgs {
    /// Station config file (.toml/.json/.yaml).
    #[arg(long, short, default_value = DEFAULT_CONFIG)]
    config: PathBuf,
    /// Override a config field, e.g. `--set mqtt.host=10.0.0.5`. Repeatable;
    /// applied after the file and `NESTRIS_STATION__*` environment variables.
    #[arg(long = "set", value_name = "PATH=VALUE")]
    set: Vec<String>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the station (the systemd service command).
    Run {
        #[command(flatten)]
        cfg: ConfigArgs,
        /// Feed a recorded .ngf/.ngf.gz game instead of the capture device
        /// (end-to-end test of sessions, cheat detection and MQTT).
        #[arg(long)]
        replay: Option<PathBuf>,
        /// Replay as fast as possible instead of at recorded speed.
        #[arg(long)]
        fast: bool,
    },
    /// Validate the configuration and print it resolved (secrets masked).
    CheckConfig {
        #[command(flatten)]
        cfg: ConfigArgs,
    },
    /// List capture devices and serial ports.
    ListDevices,
    /// Connect to the broker, publish a test status and report.
    TestMqtt {
        #[command(flatten)]
        cfg: ConfigArgs,
    },
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Cmd::Run { cfg, replay, fast } => load(&cfg).and_then(|c| run(c, replay, fast)),
        Cmd::CheckConfig { cfg } => load(&cfg).and_then(|c| {
            println!("{}", serde_json::to_string_pretty(&c.masked())?);
            c.mqtt_password().context("mqtt.password_file")?;
            eprintln!(
                "config OK: capture {}, topics {}/#, state dir {}",
                c.capture_input(),
                c.topic_base(),
                c.state_dir().display()
            );
            Ok(())
        }),
        Cmd::ListDevices => {
            list_devices();
            Ok(())
        }
        Cmd::TestMqtt { cfg } => load(&cfg).and_then(test_mqtt),
    };
    if let Err(e) = result {
        error!("{e:#}");
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn load(args: &ConfigArgs) -> Result<StationConfig> {
    let path = args.config.exists().then_some(args.config.as_path());
    if path.is_none() && args.config != Path::new(DEFAULT_CONFIG) {
        bail!("config file {} not found", args.config.display());
    }
    let cfg = StationConfig::load(path, &args.set)?;
    init_logging(&cfg.log.level);
    Ok(cfg)
}

fn init_logging(level: &str) {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(level))
        .unwrap_or_else(|_| EnvFilter::new("info"));
    // journald stamps every line itself.
    let under_systemd = std::env::var_os("JOURNAL_STREAM").is_some();
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_ansi(!under_systemd);
    let _ = if under_systemd {
        builder.without_time().try_init()
    } else {
        builder.try_init()
    };
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        error!("panic: {info}");
        default_hook(info);
    }));
}

fn list_devices() {
    if cfg!(windows) {
        match capture_ffmpeg::list_devices() {
            Ok(raw) => print!("{raw}"),
            Err(e) => eprintln!("{e:#}"),
        }
    } else {
        println!("capture devices (capture.device):");
        for device in capture_ffmpeg::list_v4l2_devices() {
            println!("  {}", device.display());
        }
        println!("serial ports (rfid.port):");
        for dir in ["/dev/serial/by-id"] {
            if let Ok(entries) = std::fs::read_dir(dir) {
                let mut ports: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
                ports.sort();
                for port in ports {
                    println!("  {}", port.display());
                }
            }
        }
    }
}

fn test_mqtt(cfg: StationConfig) -> Result<()> {
    let spool = Arc::new(Spool::open(
        std::env::temp_dir().join("nestris-station-test-spool"),
        10,
    )?);
    let link = MqttLink::start(&cfg, spool)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !link.connected() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    if !link.connected() {
        link.shutdown(&cfg.station.id);
        bail!(
            "no connection to {}:{} within 10 s (see the log above)",
            cfg.mqtt.host,
            cfg.mqtt.port
        );
    }
    link.publish_live(
        "test",
        &serde_json::json!({"station": cfg.station.id, "ts": payload::now()}).to_string(),
    );
    std::thread::sleep(Duration::from_millis(500));
    println!(
        "connected to {}:{}; published {}",
        cfg.mqtt.host,
        cfg.mqtt.port,
        link.topic("test")
    );
    link.shutdown(&cfg.station.id);
    Ok(())
}

/// Shutdown request flag (SIGTERM/SIGINT/SIGHUP on Unix).
fn shutdown_flag() -> Result<Arc<AtomicBool>> {
    let term = Arc::new(AtomicBool::new(false));
    #[cfg(unix)]
    for sig in [
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGHUP,
    ] {
        signal_hook::flag::register(sig, term.clone())?;
    }
    Ok(term)
}

fn run(cfg: StationConfig, replay: Option<PathBuf>, fast: bool) -> Result<()> {
    info!(station = %cfg.station.id, version = VERSION, "starting");
    let term = shutdown_flag()?;
    let spool = Arc::new(Spool::open(cfg.spool_dir(), cfg.spool.max_files)?);
    let pending = spool.list().len();
    info!(dir = %spool.dir().display(), pending, "message spool ready");
    let mut station = Station::new(cfg, spool)?;
    let mut watchdog = Watchdog::new();
    watchdog.ready();

    let result = match replay {
        Some(file) => station.run_replay(&file, fast, &term, &mut watchdog),
        None => station.run_capture(&term, &mut watchdog),
    };
    watchdog.stopping();
    station.shutdown();
    result
}

struct Station {
    cfg: StationConfig,
    mqtt: Option<MqttLink>,
    rfid: Option<RfidReader>,
    session: SessionTracker,
    recording: Option<(GameRecorder, RecordingSink)>,
    /// Upload queue for finished recordings (None when host.url is empty).
    uploads: Option<Arc<UploadQueue>>,
    uploader: Option<Uploader>,
    /// Game id of the session game that belongs to the running recording.
    recording_game: Option<String>,
    last_game: Option<String>,
    started: Instant,
    capture: &'static str,
    capture_detail: Option<String>,
    lock: String,
    game_state: GameState,
    last_status_key: Option<String>,
    last_status_at: Instant,
    last_player: Option<(bool, Option<rfid::Player>, bool)>,
    live_interval: Duration,
    last_live_at: Instant,
    last_live_key: String,
    fps: f64,
    fps_window: (Instant, u64),
    dropped_frames: u64,
    last_prune: Option<Instant>,
}

impl Station {
    fn new(cfg: StationConfig, spool: Arc<Spool>) -> Result<Station> {
        let mqtt = MqttLink::start(&cfg, spool)?;
        let rfid = cfg
            .rfid
            .enabled
            .then(|| RfidReader::start(cfg.rfid.clone()));
        let recording = if cfg.recording.enabled {
            let dir = cfg.recording_dir();
            let sink = RecordingSink::new(dir.clone(), cfg.recording.gzip)?;
            info!(dir = %dir.display(), "recording games");
            // record_partial: a game the station joined mid-way (restart,
            // replay) is still recorded; the session flags it as partial_game.
            let recorder = GameRecorder::new(RecorderConfig {
                record_partial: true,
                ..RecorderConfig::default()
            });
            Some((recorder, sink))
        } else {
            None
        };
        let (uploads, uploader) = if cfg.recording.enabled && !cfg.host.url.is_empty() {
            let token = cfg
                .host_token()?
                .context("host.url is set but host.token / host.token_file is empty")?;
            let queue = Arc::new(UploadQueue::open(cfg.uploads_dir())?);
            let uploader = Uploader::start(
                queue.clone(),
                UploaderConfig {
                    base_url: cfg.host.url.clone(),
                    station: cfg.station.id.clone(),
                    token,
                    retry_max: Duration::from_secs_f64(cfg.host.retry_max_s.max(5.0)),
                    max_age: Duration::from_secs_f64(cfg.host.max_age_h.max(1.0) * 3600.0),
                    timeout: Duration::from_secs_f64(cfg.host.timeout_s.max(5.0)),
                },
            );
            info!(host = %cfg.host.url, "uploading recordings to the host");
            (Some(queue), Some(uploader))
        } else {
            (None, None)
        };
        let session = SessionTracker::new(
            cfg.session.clone(),
            cfg.integrity.clone(),
            cfg.station.id.clone(),
            Duration::from_secs_f64(cfg.rfid.player_grace_s.max(0.0)),
        );
        let live_interval = Duration::from_secs_f64(1.0 / cfg.mqtt.live_max_hz);
        Ok(Station {
            cfg,
            mqtt: Some(mqtt),
            rfid,
            session,
            recording,
            uploads,
            uploader,
            recording_game: None,
            last_game: None,
            started: Instant::now(),
            capture: "opening",
            capture_detail: None,
            lock: "unlocked".into(),
            game_state: GameState::Unknown,
            last_status_key: None,
            last_status_at: Instant::now(),
            last_player: None,
            live_interval,
            last_live_at: Instant::now() - live_interval,
            last_live_key: String::new(),
            fps: 0.0,
            fps_window: (Instant::now(), 0),
            dropped_frames: 0,
            last_prune: None,
        })
    }

    fn mqtt(&self) -> &MqttLink {
        self.mqtt.as_ref().expect("mqtt link lives until shutdown")
    }

    fn run_capture(&mut self, term: &AtomicBool, watchdog: &mut Watchdog) -> Result<()> {
        let supervisor = CaptureSupervisor::start(self.cfg.supervisor());
        let mut processor = FrameProcessor::new(self.cfg.engine.clone());
        let background = self.cfg.engine.calibration.background_recalibration;
        let mut recalib = background.then(RecalibThread::start);
        let mut down_since: Option<Instant> = Some(Instant::now());
        while !term.load(Ordering::Relaxed) {
            match supervisor.recv(Duration::from_millis(200)) {
                Recv::Msg(CaptureMsg::Frame(frame)) => {
                    if let Some(since) = down_since.take() {
                        self.session.capture_gap(since.elapsed().as_secs_f64());
                    }
                    let result = catch_unwind(AssertUnwindSafe(|| processor.process(&frame)));
                    match result {
                        Ok(out) => {
                            if let Some(recalib) = &mut recalib {
                                recalib.drive(&mut processor, &frame);
                            }
                            self.lock = format!("{:?}", processor.lock_state()).to_lowercase();
                            self.handle_output(&out);
                        }
                        Err(_) => {
                            error!(seq = frame.seq, "engine panicked; rebuilding it");
                            processor = FrameProcessor::new(self.cfg.engine.clone());
                            recalib = background.then(RecalibThread::start);
                            self.session.engine_restart();
                        }
                    }
                    self.count_frame();
                }
                Recv::Msg(CaptureMsg::Status(status)) => {
                    let ended = status == CaptureStatus::Ended;
                    self.set_capture(status);
                    if self.capture != "ok" {
                        down_since.get_or_insert_with(Instant::now);
                    }
                    if ended {
                        info!("capture source ended");
                        break;
                    }
                }
                Recv::Timeout => {}
                Recv::Closed => break,
            }
            if let Some(since) = down_since {
                let events = self.session.capture_down_for(since.elapsed().as_secs_f64());
                self.publish_events(events);
            }
            self.dropped_frames = supervisor.dropped_frames();
            self.tick(watchdog);
        }
        supervisor.stop();
        Ok(())
    }

    fn run_replay(
        &mut self,
        file: &Path,
        fast: bool,
        term: &AtomicBool,
        watchdog: &mut Watchdog,
    ) -> Result<()> {
        let replay = nestris_ngf::replay::ReplayFile::load(file)
            .with_context(|| format!("load replay {}", file.display()))?;
        let mut engine = nestris_ngf::replay::ReplayEngine::new(replay);
        info!(file = %file.display(), frames = engine.frame_count(), "replaying");
        self.capture = "ok";
        self.capture_detail = Some(format!("replay {}", file.display()));
        let started = Instant::now();
        for index in 0..engine.frame_count() {
            if term.load(Ordering::Relaxed) {
                break;
            }
            if !fast {
                let due = Duration::from_millis(u64::from(engine.ctime_ms_at(index)));
                if let Some(wait) = due.checked_sub(started.elapsed()) {
                    std::thread::sleep(wait);
                }
            }
            let out = engine.output_at(index);
            self.handle_output(&out);
            self.count_frame();
            self.tick(watchdog);
        }
        // Let the end-of-game logic see the recording's end: feed game-over
        // frames (a replay stops at the last in-game frame).
        let mut tail = engine.output_at(engine.frame_count().saturating_sub(1));
        tail.game_state = GameState::GameOver;
        tail.events.clear();
        for _ in 0..=self.cfg.session.end_confirm_frames {
            self.handle_output(&tail);
        }
        // Give the MQTT link a moment to flush the result.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && !term.load(Ordering::Relaxed) && self.mqtt().connected()
        {
            if self.mqtt().pending() == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    }

    fn rfid_snapshot(&self) -> RfidSnapshot {
        self.rfid.as_ref().map(|r| r.snapshot()).unwrap_or_default()
    }

    fn handle_output(&mut self, out: &OutputFrame) {
        self.game_state = out.game_state;
        let rfid = self.rfid_snapshot();
        let events = self.session.push(out, &rfid);
        self.note_games(&events);
        self.publish_events(events);

        let mut saved = Vec::new();
        if let Some((recorder, sink)) = &mut self.recording {
            let events = recorder.push(out);
            match sink.handle(recorder, events) {
                Ok(paths) => saved = paths,
                Err(e) => warn!(error = %format!("{e:#}"), "recording failed"),
            }
        }
        for path in saved {
            info!(path = %path.display(), "recording saved");
            self.queue_upload(path);
        }

        // Capture frames jitter around 16.7 ms; without a little slack a
        // 60 Hz limit would drop every frame that arrives slightly early and
        // halve the effective rate.
        let slack = self
            .live_interval
            .mul_f64(0.25)
            .min(Duration::from_millis(4));
        if self.last_live_at.elapsed() + slack >= self.live_interval {
            let mut live = self.session.live(out, &rfid, self.cfg.mqtt.live_playfield);
            let ts = std::mem::take(&mut live.ts);
            let key = serde_json::to_string(&live).unwrap_or_default();
            if key != self.last_live_key {
                live.ts = ts;
                if let Ok(json) = serde_json::to_string(&live) {
                    self.mqtt().publish_live(topic::LIVE, &json);
                }
                self.last_live_key = key;
                self.last_live_at = Instant::now();
            }
        }
    }

    /// Remember which session game the running recording belongs to.
    fn note_games(&mut self, events: &[SessionEvent]) {
        for event in events {
            if let SessionEvent::Start(start) = event {
                self.last_game = Some(start.game_id.clone());
                let recording = self.recording.as_ref().is_some_and(|(r, _)| r.recording());
                if recording {
                    self.recording_game = Some(start.game_id.clone());
                }
            }
        }
    }

    fn queue_upload(&mut self, path: PathBuf) {
        let Some(queue) = &self.uploads else {
            self.recording_game = None;
            return;
        };
        // Normally the session game started while this recording ran; a
        // recording that began mid-game falls back to the last known game.
        let Some(game_id) = self
            .recording_game
            .take()
            .or_else(|| self.last_game.clone())
        else {
            warn!(path = %path.display(), "recording without a session game, not uploaded");
            return;
        };
        match queue.push(&UploadJob { game_id, path }) {
            Ok(()) => {
                if let Some(uploader) = &self.uploader {
                    uploader.notify();
                }
            }
            Err(e) => error!(error = %format!("{e:#}"), "could not queue recording upload"),
        }
    }

    fn publish_events(&self, events: Vec<SessionEvent>) {
        for event in events {
            let (suffix, json) = match &event {
                SessionEvent::Start(p) => (topic::GAME_START, serde_json::to_string(p)),
                SessionEvent::Cheat(p) => (topic::CHEAT, serde_json::to_string(p)),
                SessionEvent::End(p) => (topic::GAME_END, serde_json::to_string(p)),
            };
            match json {
                Ok(json) => {
                    if let Err(e) = self.mqtt().publish_durable(suffix, json) {
                        // Disk trouble: still try to deliver it live.
                        error!(error = %format!("{e:#}"), suffix, "could not spool message");
                    }
                }
                Err(e) => error!(error = %e, suffix, "could not serialize message"),
            }
        }
    }

    fn set_capture(&mut self, status: CaptureStatus) {
        let (state, detail) = match &status {
            CaptureStatus::Opening => ("opening", None),
            CaptureStatus::Running { width, height } => ("ok", Some(format!("{width}x{height}"))),
            CaptureStatus::WaitingForDevice { path } => {
                ("waiting_for_device", Some(path.display().to_string()))
            }
            CaptureStatus::Failed { reason, retry_in } => (
                "reconnecting",
                Some(format!("{reason} (retry in {}s)", retry_in.as_secs())),
            ),
            CaptureStatus::Ended => ("ended", None),
        };
        match state {
            "ok" | "opening" => info!(capture = state, detail = ?detail, "capture status"),
            _ => warn!(capture = state, detail = ?detail, "capture status"),
        }
        self.capture = state;
        self.capture_detail = detail;
    }

    fn count_frame(&mut self) {
        self.fps_window.1 += 1;
        let elapsed = self.fps_window.0.elapsed();
        if elapsed >= Duration::from_secs(2) {
            self.fps = (self.fps_window.1 as f64 / elapsed.as_secs_f64() * 10.0).round() / 10.0;
            self.fps_window = (Instant::now(), 0);
        }
    }

    /// Periodic work: commands, player, status, pruning, watchdog.
    fn tick(&mut self, watchdog: &mut Watchdog) {
        for cmd in self.mqtt().take_commands() {
            self.handle_command(&cmd);
        }

        let rfid = self.rfid_snapshot();
        let rfid_state = match &self.rfid {
            None => "disabled",
            Some(_) if rfid.connected => "ok",
            // A device answers but with an old protocol: needs a firmware update.
            Some(_) if rfid.protocol_error.is_some() => "outdated",
            Some(_) => "offline",
        };
        let player_key = (rfid.present.is_some(), rfid.present.clone(), rfid.connected);
        if self.last_player.as_ref() != Some(&player_key) {
            let payload = PlayerPayload {
                present: rfid.present.is_some(),
                player: rfid.present.clone(),
                rfid: rfid_state,
                ts: payload::now(),
            };
            if let Ok(json) = serde_json::to_string(&payload) {
                self.mqtt().publish_retained(topic::PLAYER, json);
            }
            self.last_player = Some(player_key);
        }

        let mut status = Status {
            state: "online",
            station: self.cfg.station.id.clone(),
            name: self.cfg.station.name.clone(),
            version: VERSION,
            capture: self.capture,
            capture_detail: self.capture_detail.clone(),
            lock: self.lock.clone(),
            game_state: game_state_str(self.game_state),
            rfid: rfid_state,
            reader_fw: rfid.info.as_ref().map(|i| i.fw.clone()),
            reader_serial: rfid.info.as_ref().map(|i| i.serial.clone()),
            game_id: self.session.game_id().map(str::to_owned),
            fps: 0.0,
            dropped_frames: 0,
            uptime_s: 0,
            ts: String::new(),
        };
        let key = serde_json::to_string(&status).unwrap_or_default();
        let interval = Duration::from_secs_f64(self.cfg.mqtt.status_interval_s.max(1.0));
        let changed = self.last_status_key.as_deref() != Some(key.as_str());
        let since = self.last_status_at.elapsed();
        // Changes go out promptly but at most every STATUS_MIN_GAP (a
        // flickering game state must not flood a retained topic).
        if (changed && since >= STATUS_MIN_GAP) || since >= interval {
            status.fps = self.fps;
            status.dropped_frames = self.dropped_frames;
            status.uptime_s = self.started.elapsed().as_secs();
            status.ts = payload::now();
            if let Ok(json) = serde_json::to_string(&status) {
                self.mqtt().publish_retained(topic::STATUS, json);
            }
            self.last_status_key = Some(key);
            self.last_status_at = Instant::now();
        }

        if self.recording.is_some() && self.last_prune.is_none_or(|t| t.elapsed() >= PRUNE_EVERY) {
            self.last_prune = Some(Instant::now());
            health::prune_recordings(
                &self.cfg.recording_dir(),
                self.cfg.recording.keep_days,
                (self.cfg.recording.max_gb * 1024.0 * 1024.0 * 1024.0) as u64,
            );
        }

        watchdog.tick(&format!(
            "capture {}, rfid {}, mqtt {}, {:.0} fps",
            self.capture,
            rfid_state,
            if self.mqtt().connected() {
                "ok"
            } else {
                "offline"
            },
            self.fps
        ));
    }

    fn handle_command(&self, raw: &str) {
        let parsed: Option<serde_json::Value> = serde_json::from_str(raw).ok();
        let kind = parsed
            .as_ref()
            .and_then(|v| v.get("type"))
            .and_then(|t| t.as_str());
        match (kind, &self.rfid) {
            (Some(kind), Some(reader)) if READER_COMMANDS.contains(&kind) => {
                // Re-serialize: one compact line, no embedded newlines.
                let line = parsed.as_ref().map(|v| v.to_string()).unwrap_or_default();
                info!(kind, "forwarding command to the RFID reader");
                reader.send(line);
            }
            (Some(kind), None) if READER_COMMANDS.contains(&kind) => {
                warn!(kind, "command ignored: RFID reader disabled");
            }
            _ => warn!(command = raw, "ignoring unknown command"),
        }
    }

    fn shutdown(&mut self) {
        info!("shutting down");
        let events = self.session.shutdown();
        self.note_games(&events);
        self.publish_events(events);
        let mut saved = None;
        if let Some((recorder, sink)) = &mut self.recording {
            match sink.finalize(recorder) {
                Ok(Some(path)) => {
                    info!(path = %path.display(), "recording saved");
                    saved = Some(path);
                }
                Ok(None) => {}
                Err(e) => warn!(error = %format!("{e:#}"), "recording finalize failed"),
            }
        }
        if let Some(path) = saved {
            self.queue_upload(path);
        }
        if let Some(uploader) = &self.uploader {
            // Pending jobs survive a restart; this only avoids a needless delay.
            uploader.drain(Duration::from_secs(10));
        }
        if let Some(mqtt) = self.mqtt.take() {
            mqtt.shutdown(&self.cfg.station.id);
        }
        // Dropping the reader joins its thread.
        self.rfid = None;
    }
}

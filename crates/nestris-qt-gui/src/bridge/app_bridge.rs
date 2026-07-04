//! The QML-facing application singleton: source control, transport,
//! dashboard values, and the bridge between the pipeline worker thread
//! and the Qt event loop.

use core::pin::Pin;
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;
use std::time::Instant;

use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::{QString, QStringList, QUrl};
use nestris_engine::enums::GameState;
use nestris_gui_core::settings::GuiSettings;
use nestris_gui_core::worker::{self, Cmd, GuiUpdate, WorkerMsg};
use nestris_host::capture_ffmpeg;

use crate::frames;

/// This frontend's settings file (the egui GUI persists its own).
pub const SETTINGS_FILE: &str = "qt-gui-settings.toml";

/// Below this confidence a dashboard value renders grayed out.
/// (Mirrors the egui GUI's `STALE_CONFIDENCE`; exposed to QML as-is.)
const ALARM_AFTER_S: f64 = 2.0;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        /// QString from cxx_qt_lib
        type QString = cxx_qt_lib::QString;

        include!("cxx-qt-lib/qstringlist.h");
        /// QStringList from cxx_qt_lib
        type QStringList = cxx_qt_lib::QStringList;

        include!("cxx-qt-lib/qurl.h");
        /// QUrl from cxx_qt_lib
        type QUrl = cxx_qt_lib::QUrl;
    }

    unsafe extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        // Lifecycle / transport state
        #[qproperty(bool, running)]
        #[qproperty(bool, live)]
        #[qproperty(bool, paused)]
        #[qproperty(bool, buffering)]
        #[qproperty(bool, recording)]
        #[qproperty(bool, loop_enabled, cxx_name = "loopEnabled")]
        #[qproperty(f64, speed)]
        #[qproperty(f64, position_s, cxx_name = "positionS")]
        #[qproperty(f64, duration_s, cxx_name = "durationS")]
        #[qproperty(f64, fps)]
        #[qproperty(i32, frame_serial, cxx_name = "frameSerial")]
        // Source / status
        #[qproperty(QString, source)]
        #[qproperty(QString, lock_state, cxx_name = "lockState")]
        #[qproperty(QString, game_state, cxx_name = "gameState")]
        #[qproperty(QString, last_error, cxx_name = "lastError")]
        #[qproperty(bool, alarm)]
        #[qproperty(bool, supports_capture, cxx_name = "supportsCapture")]
        #[qproperty(QStringList, devices)]
        // Dashboard values (-1 = unknown)
        #[qproperty(i32, score)]
        #[qproperty(i32, lines)]
        #[qproperty(i32, level)]
        #[qproperty(QString, next_piece, cxx_name = "nextPiece")]
        #[qproperty(i32, pieces)]
        #[qproperty(f64, tetris_rate, cxx_name = "tetrisRate")]
        #[qproperty(f64, pps)]
        #[qproperty(i32, burn)]
        #[qproperty(i32, drought)]
        #[qproperty(i32, clears_single, cxx_name = "clearsSingle")]
        #[qproperty(i32, clears_double, cxx_name = "clearsDouble")]
        #[qproperty(i32, clears_triple, cxx_name = "clearsTriple")]
        #[qproperty(i32, clears_tetris, cxx_name = "clearsTetris")]
        // Per-field confidences for graying
        #[qproperty(f64, conf_score, cxx_name = "confScore")]
        #[qproperty(f64, conf_lines, cxx_name = "confLines")]
        #[qproperty(f64, conf_level, cxx_name = "confLevel")]
        #[qproperty(f64, conf_next, cxx_name = "confNext")]
        #[qproperty(f64, conf_overall, cxx_name = "confOverall")]
        type AppBridge = super::AppBridgeRust;

        // ---- signals ----
        /// One formatted event-stream row arrived.
        #[qsignal]
        #[cxx_name = "eventAdded"]
        fn event_added(self: Pin<&mut Self>, text: QString, severity: QString);

        /// Transient notification (kind: info / success / error).
        #[qsignal]
        fn toast(self: Pin<&mut Self>, kind: QString, message: QString);

        // ---- invokables ----
        #[qinvokable]
        #[cxx_name = "openVideoDialog"]
        fn open_video_dialog(self: Pin<&mut Self>);

        #[qinvokable]
        #[cxx_name = "openReplayDialog"]
        fn open_replay_dialog(self: Pin<&mut Self>);

        /// Open a dropped file (video or NGF replay).
        #[qinvokable]
        #[cxx_name = "openUrl"]
        fn open_url(self: Pin<&mut Self>, url: &QUrl);

        #[qinvokable]
        #[cxx_name = "refreshDevices"]
        fn refresh_devices(self: Pin<&mut Self>);

        /// Start capturing from a DirectShow device by name.
        #[qinvokable]
        #[cxx_name = "openDevice"]
        fn open_device(self: Pin<&mut Self>, name: &QString);

        #[qinvokable]
        #[cxx_name = "startSource"]
        fn start_source(self: Pin<&mut Self>);

        #[qinvokable]
        #[cxx_name = "stopSource"]
        fn stop_source(self: Pin<&mut Self>);

        #[qinvokable]
        #[cxx_name = "togglePause"]
        fn toggle_pause(self: Pin<&mut Self>);

        /// Step one frame forward (pauses first).
        #[qinvokable]
        #[cxx_name = "stepFrame"]
        fn step_frame(self: Pin<&mut Self>);

        /// One frame back = a short reverse seek (the pipe can't reverse).
        #[qinvokable]
        #[cxx_name = "stepBack"]
        fn step_back(self: Pin<&mut Self>);

        /// Set the playback speed (<= 0 = as fast as possible).
        #[qinvokable]
        #[cxx_name = "applySpeed"]
        fn apply_speed(self: Pin<&mut Self>, speed: f64);

        /// Cycle through the speed steps (up = faster).
        #[qinvokable]
        #[cxx_name = "cycleSpeed"]
        fn cycle_speed(self: Pin<&mut Self>, up: bool);

        #[qinvokable]
        #[cxx_name = "seekTo"]
        fn seek_to(self: Pin<&mut Self>, position_s: f64);

        #[qinvokable]
        #[cxx_name = "seekBy"]
        fn seek_by(self: Pin<&mut Self>, delta_s: f64);

        #[qinvokable]
        #[cxx_name = "resetLock"]
        fn reset_lock(self: Pin<&mut Self>);

        /// Whether the current source is an NGF replay (instant seeks).
        #[qinvokable]
        #[cxx_name = "isReplay"]
        fn is_replay(self: &Self) -> bool;
    }

    impl cxx_qt::Threading for AppBridge {}
    impl cxx_qt::Initialize for AppBridge {}
}

pub struct AppBridgeRust {
    // Property backing fields.
    running: bool,
    live: bool,
    paused: bool,
    buffering: bool,
    recording: bool,
    loop_enabled: bool,
    speed: f64,
    position_s: f64,
    duration_s: f64,
    fps: f64,
    frame_serial: i32,
    source: QString,
    lock_state: QString,
    game_state: QString,
    last_error: QString,
    alarm: bool,
    supports_capture: bool,
    devices: QStringList,
    score: i32,
    lines: i32,
    level: i32,
    next_piece: QString,
    pieces: i32,
    tetris_rate: f64,
    pps: f64,
    burn: i32,
    drought: i32,
    clears_single: i32,
    clears_double: i32,
    clears_triple: i32,
    clears_tetris: i32,
    conf_score: f64,
    conf_lines: f64,
    conf_level: f64,
    conf_next: f64,
    conf_overall: f64,
    // Non-property state.
    settings: GuiSettings,
    cmd: Option<Sender<Cmd>>,
    join: Option<JoinHandle<()>>,
    /// Bumped on every start/stop; stale queued messages are dropped.
    generation: u64,
    /// When the lock first left LOCKED (for the CHECK CAPTURE alarm).
    unhealthy_since: Option<Instant>,
}

impl Default for AppBridgeRust {
    fn default() -> Self {
        let settings = GuiSettings::load_from(SETTINGS_FILE);
        Self {
            running: false,
            live: false,
            paused: false,
            buffering: false,
            recording: false,
            loop_enabled: false,
            speed: f64::from(settings.speed),
            position_s: 0.0,
            duration_s: -1.0,
            fps: 0.0,
            frame_serial: 0,
            source: QString::from(&settings.last_source),
            lock_state: QString::from("UNLOCKED"),
            game_state: QString::from(""),
            last_error: QString::from(""),
            alarm: false,
            supports_capture: cfg!(windows),
            devices: QStringList::default(),
            score: -1,
            lines: -1,
            level: -1,
            next_piece: QString::from("—"),
            pieces: 0,
            tetris_rate: -1.0,
            pps: -1.0,
            burn: 0,
            drought: 0,
            clears_single: 0,
            clears_double: 0,
            clears_triple: 0,
            clears_tetris: 0,
            conf_score: 1.0,
            conf_lines: 1.0,
            conf_level: 1.0,
            conf_next: 1.0,
            conf_overall: 1.0,
            settings,
            cmd: None,
            join: None,
            generation: 0,
            unhealthy_since: None,
        }
    }
}

impl cxx_qt::Initialize for qobject::AppBridge {
    fn initialize(mut self: Pin<&mut Self>) {
        if let Some(auto) = crate::AUTO_START.get() {
            self.as_mut().set_source(QString::from(&auto.source));
            self.start_source_at(auto.start_s);
        }
    }
}

const VIDEO_FILTER: [&str; 6] = ["mp4", "mkv", "avi", "mov", "webm", "ts"];
const SPEEDS: [f64; 6] = [0.25, 0.5, 1.0, 2.0, 4.0, -1.0];

impl qobject::AppBridge {
    // ---- source control ----

    pub fn open_video_dialog(self: Pin<&mut Self>) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Video", &VIDEO_FILTER)
            .pick_file()
        {
            self.open_path(path);
        }
    }

    pub fn open_replay_dialog(self: Pin<&mut Self>) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("NGF replay", &["ngf", "gz", "part"])
            .pick_file()
        {
            self.open_path(path);
        }
    }

    pub fn open_url(self: Pin<&mut Self>, url: &QUrl) {
        let path = url.to_local_file().map(|p| p.to_string());
        if let Some(path) = path.filter(|p| !p.is_empty()) {
            self.open_path(std::path::PathBuf::from(path));
        }
    }

    fn open_path(mut self: Pin<&mut Self>, path: std::path::PathBuf) {
        self.as_mut()
            .set_source(QString::from(path.to_string_lossy().as_ref()));
        self.start_source_at(0.0);
    }

    pub fn refresh_devices(mut self: Pin<&mut Self>) {
        let devices: Vec<String> = capture_ffmpeg::list_devices()
            .map(|raw| {
                raw.lines()
                    .filter(|l| l.contains("(video)"))
                    .filter_map(|l| l.split('"').nth(1).map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let mut list = QStringList::default();
        for device in &devices {
            list.append(QString::from(device));
        }
        self.as_mut().set_devices(list);
    }

    pub fn open_device(mut self: Pin<&mut Self>, name: &QString) {
        self.as_mut()
            .set_source(QString::from(&format!("dshow:{name}")));
        self.start_source_at(0.0);
    }

    pub fn start_source(self: Pin<&mut Self>) {
        self.start_source_at(0.0);
    }

    pub(crate) fn start_source_at(mut self: Pin<&mut Self>, start_s: f64) {
        self.as_mut().stop_source();
        let source = self.source().to_string();
        if source.is_empty() {
            self.as_mut().set_last_error(QString::from(
                "Choose a video file, capture device, or replay first.",
            ));
            return;
        }
        self.as_mut().set_last_error(QString::from(""));
        self.as_mut().set_paused(false);
        self.as_mut().set_buffering(true);
        self.as_mut().set_duration_s(-1.0);
        self.as_mut().set_live(false);
        self.as_mut().set_alarm(false);
        frames::clear();

        let speed = *self.speed() as f32;
        let (config, sinks, generation) = {
            let mut rust = self.as_mut().rust_mut();
            rust.settings.last_source = source.clone();
            rust.settings.speed = speed;
            rust.settings.save_to(SETTINGS_FILE);
            rust.generation += 1;
            rust.unhealthy_since = None;
            (
                rust.settings.engine.clone(),
                rust.settings.sink_options(),
                rust.generation,
            )
        };

        let handle = worker::spawn(source, config, sinks, start_s, speed);
        let qt_thread = self.qt_thread();
        let updates = handle.updates;
        std::thread::spawn(move || crate::worker_glue::forward(updates, qt_thread, generation));

        {
            let mut rust = self.as_mut().rust_mut();
            rust.cmd = Some(handle.cmd);
            rust.join = Some(handle.join);
        }
        self.as_mut().set_running(true);
    }

    pub fn stop_source(mut self: Pin<&mut Self>) {
        let (cmd, join) = {
            let mut rust = self.as_mut().rust_mut();
            rust.generation += 1; // queued messages from this source are stale now
            (rust.cmd.take(), rust.join.take())
        };
        if let Some(cmd) = cmd {
            let _ = cmd.send(Cmd::Stop);
        }
        if let Some(join) = join {
            let _ = join.join();
        }
        self.as_mut().set_running(false);
        self.as_mut().set_buffering(false);
        self.as_mut().set_recording(false);
        self.as_mut().set_alarm(false);
    }

    // ---- transport ----

    fn send(&self, cmd: Cmd) {
        if let Some(sender) = &self.rust().cmd {
            let _ = sender.send(cmd);
        }
    }

    pub fn toggle_pause(mut self: Pin<&mut Self>) {
        if !*self.running() || *self.live() {
            return;
        }
        let paused = !*self.paused();
        self.as_mut().set_paused(paused);
        self.send(Cmd::Pause(paused));
    }

    pub fn step_frame(mut self: Pin<&mut Self>) {
        if !*self.running() || *self.live() {
            return;
        }
        if !*self.paused() {
            self.as_mut().set_paused(true);
            self.send(Cmd::Pause(true));
        }
        self.send(Cmd::StepFrame);
    }

    pub fn step_back(mut self: Pin<&mut Self>) {
        if !*self.running() || *self.live() {
            return;
        }
        self.as_mut().seek_by(-1.0 / 30.0);
        if !*self.paused() {
            self.as_mut().set_paused(true);
            self.send(Cmd::Pause(true));
        }
    }

    pub fn apply_speed(mut self: Pin<&mut Self>, speed: f64) {
        self.as_mut().set_speed(speed);
        {
            let mut rust = self.as_mut().rust_mut();
            rust.settings.speed = speed as f32;
            rust.settings.save_to(SETTINGS_FILE);
        }
        self.send(Cmd::SetSpeed(speed as f32));
    }

    pub fn cycle_speed(self: Pin<&mut Self>, up: bool) {
        let current = SPEEDS.iter().position(|&s| s == *self.speed()).unwrap_or(2);
        let next = if up {
            (current + 1).min(SPEEDS.len() - 1)
        } else {
            current.saturating_sub(1)
        };
        self.apply_speed(SPEEDS[next]);
    }

    pub fn seek_to(mut self: Pin<&mut Self>, position_s: f64) {
        if !*self.running() || *self.live() {
            return;
        }
        let max = match *self.duration_s() {
            d if d > 0.0 => d,
            _ => f64::MAX,
        };
        let target = position_s.clamp(0.0, max);
        let replay = self.is_replay();
        self.as_mut().set_position_s(target);
        self.as_mut().set_buffering(!replay);
        self.send(Cmd::Seek(target));
    }

    pub fn seek_by(mut self: Pin<&mut Self>, delta_s: f64) {
        let target = *self.position_s() + delta_s;
        self.as_mut().seek_to(target);
    }

    pub fn reset_lock(self: Pin<&mut Self>) {
        self.send(Cmd::ResetLock);
    }

    pub fn is_replay(&self) -> bool {
        worker::is_replay_path(&self.source().to_string())
    }

    // ---- worker messages (queued from the forwarder thread) ----

    pub(crate) fn handle_worker_msg(mut self: Pin<&mut Self>, msg: WorkerMsg, generation: u64) {
        if generation != self.rust().generation {
            return;
        }
        match msg {
            WorkerMsg::Opened { duration_s, live } => {
                self.as_mut().set_duration_s(duration_s.unwrap_or(-1.0));
                self.as_mut().set_live(live);
                self.as_mut().set_buffering(false);
            }
            WorkerMsg::GameSaved(path) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.as_mut().toast(
                    QString::from("success"),
                    QString::from(&format!("Recording saved: {name}")),
                );
            }
            WorkerMsg::Ended => {
                self.as_mut().set_buffering(false);
                self.as_mut().set_running(false);
                self.as_mut()
                    .toast(QString::from("info"), QString::from("End of stream"));
            }
            WorkerMsg::Error(message) => {
                self.as_mut().set_buffering(false);
                self.as_mut().set_running(false);
                self.as_mut().set_last_error(QString::from(&message));
                self.as_mut()
                    .toast(QString::from("error"), QString::from(&message));
            }
            WorkerMsg::Update(update) => self.apply_update(update, generation),
        }
    }

    pub(crate) fn apply_update(mut self: Pin<&mut Self>, update: Box<GuiUpdate>, generation: u64) {
        if generation != self.rust().generation {
            return;
        }

        // Frame images + playfield for the painted items.
        {
            let mut store = frames::FRAMES.lock().unwrap();
            if update.raw_w > 0 {
                store.raw = Some((update.raw_rgba, update.raw_w, update.raw_h));
            } else {
                store.raw = None; // replay: no source video
            }
            if let Some(canon) = update.canon_rgba {
                store.canon = Some(canon);
            }
            store.lock_quad = update.lock_quad;
            store.grid = update.output.fields.playfield.clone();
            store.level = update.output.fields.level;
            store.piece = update.output.fields.current_piece;
            store.piece_cells = update
                .output
                .fields
                .current_piece_cells
                .clone()
                .unwrap_or_default();
        }

        // Events (shared formatting with the egui GUI; QML list caps rows).
        let mut rows = Vec::new();
        nestris_gui_core::events::push_events(&mut rows, &update.output);
        for row in rows {
            self.as_mut()
                .event_added(QString::from(&row.text), QString::from(&row.severity));
        }

        // Scalar dashboard values.
        let output = &update.output;
        let fields = &output.fields;
        let conf = &output.confidence;
        let opt_i32 = |v: Option<i64>| v.map_or(-1, |v| v.clamp(0, i64::from(i32::MAX)) as i32);
        self.as_mut().set_score(opt_i32(fields.score));
        self.as_mut().set_lines(opt_i32(fields.lines));
        self.as_mut().set_level(opt_i32(fields.level));
        self.as_mut().set_next_piece(QString::from(
            fields.next_piece.map_or("—", |piece| piece.letter()),
        ));
        self.as_mut().set_pieces(output.stats.pieces as i32);
        self.as_mut()
            .set_tetris_rate(output.stats.tetris_rate.unwrap_or(-1.0));
        self.as_mut().set_pps(output.stats.pps.unwrap_or(-1.0));
        self.as_mut().set_burn(output.stats.burn as i32);
        self.as_mut().set_drought(output.stats.drought as i32);
        self.as_mut()
            .set_clears_single(output.stats.clears.single as i32);
        self.as_mut()
            .set_clears_double(output.stats.clears.double as i32);
        self.as_mut()
            .set_clears_triple(output.stats.clears.triple as i32);
        self.as_mut()
            .set_clears_tetris(output.stats.clears.tetris as i32);
        self.as_mut().set_conf_score(conf.score);
        self.as_mut().set_conf_lines(conf.lines);
        self.as_mut().set_conf_level(conf.level);
        self.as_mut().set_conf_next(conf.next_piece);
        self.as_mut().set_conf_overall(conf.overall);
        self.as_mut()
            .set_game_state(QString::from(state_label(output.game_state)));
        if self.lock_state().to_string() != update.lock_state {
            self.as_mut()
                .set_lock_state(QString::from(update.lock_state));
        }

        // Transport / status line.
        self.as_mut().set_position_s(update.position_s);
        if let Some(duration) = update.duration_s
            && *self.duration_s() != duration
        {
            self.as_mut().set_duration_s(duration);
        }
        self.as_mut().set_fps(f64::from(update.fps));
        self.as_mut().set_recording(update.recording);
        self.as_mut().set_buffering(false);

        // Capture health → ⚠ CHECK CAPTURE (same thresholds as egui).
        let healthy =
            matches!(update.lock_state, "LOCKED" | "REPLAY") && output.confidence.overall >= 0.3;
        let alarm = {
            let mut rust = self.as_mut().rust_mut();
            if healthy {
                rust.unhealthy_since = None;
            } else if rust.unhealthy_since.is_none() {
                rust.unhealthy_since = Some(Instant::now());
            }
            rust.unhealthy_since
                .is_some_and(|t| t.elapsed().as_secs_f64() > ALARM_AFTER_S)
        };
        if *self.alarm() != alarm {
            self.as_mut().set_alarm(alarm);
        }

        // Repaint trigger for the painted items.
        let serial = self.frame_serial().wrapping_add(1);
        self.as_mut().set_frame_serial(serial);
    }
}

/// Display label for the game-state banner.
fn state_label(state: GameState) -> &'static str {
    match state {
        GameState::NoSignal => "NO SIGNAL",
        GameState::Unknown => "—",
        GameState::Title => "TITLE",
        GameState::TypeSelect => "TYPE SELECT",
        GameState::LevelSelect => "LEVEL SELECT",
        GameState::InGame => "IN GAME",
        GameState::Paused => "PAUSED",
        GameState::GameOver => "GAME OVER",
        GameState::HighscoreEntry => "HIGH SCORE",
    }
}

impl Drop for AppBridgeRust {
    fn drop(&mut self) {
        if let Some(cmd) = self.cmd.take() {
            let _ = cmd.send(Cmd::Stop);
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        self.settings.save_to(SETTINGS_FILE);
    }
}

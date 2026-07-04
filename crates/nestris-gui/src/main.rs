//! Native desktop GUI (egui): the Rust counterpart of the Python PySide6
//! app. Source picker (file / DirectShow device / NGF replay), live raw
//! preview with the detected-playfield overlay, canonical preview,
//! tracked-field rendering in the NES level palette, dashboard with
//! confidence-aware graying and a capture alarm, event stream, a
//! NestrisChamps-style statistics window with persistent PB tables, full
//! transport (pause / speed / seek with hover preview / frame stepping),
//! keyboard shortcuts, toasts, drag & drop, and a settings dialog
//! persisted as TOML.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod session;
mod settings_ui;
mod stats_panels;
mod toasts;
mod worker;

use std::time::Instant;

use eframe::egui;
use nestris_engine::enums::{GameState, Piece};
use nestris_engine::nes_palette;
use nestris_engine::output::OutputFrame;
use nestris_host::capture_ffmpeg;

use crate::session::SessionStore;
use crate::settings_ui::{GuiSettings, settings_window};
use crate::toasts::{ToastKind, Toasts};
use crate::worker::{Cmd, GuiUpdate, WorkerHandle, WorkerMsg};

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1480.0, 900.0])
            .with_min_inner_size([840.0, 560.0])
            .with_title("nestris-core"),
        ..Default::default()
    };
    eframe::run_native(
        "nestris-core",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

struct EventRow {
    text: String,
    severity: String,
}

/// Below this confidence a dashboard value renders grayed out.
const STALE_CONFIDENCE: f64 = 0.4;
/// Seconds of DRIFT/LOST (or very low confidence) before the capture alarm.
const ALARM_AFTER_S: f64 = 2.0;

struct App {
    settings: GuiSettings,
    show_settings: bool,
    show_stats: bool,
    source: String,
    devices: Vec<String>,
    worker: Option<WorkerHandle>,
    latest: Option<Box<GuiUpdate>>,
    events: Vec<EventRow>,
    paused: bool,
    seek_target: f64,
    seek_dragging: bool,
    /// A seek/settings restart is in flight; cleared by the next update.
    buffering: bool,
    duration_s: Option<f64>,
    live: bool,
    error: Option<String>,
    raw_tex: Option<egui::TextureHandle>,
    canon_tex: Option<egui::TextureHandle>,
    toasts: Toasts,
    session: SessionStore,
    /// When the lock first left LOCKED (for the CHECK CAPTURE alarm).
    unhealthy_since: Option<Instant>,
    /// Previous frame's game state (game-end detection for the PB tables).
    prev_game_state: Option<GameState>,
    /// Last in-game snapshot, recorded into the session on game over.
    last_ingame: Option<(OutputFrame, f64)>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let settings = GuiSettings::load();
        Self {
            source: settings.last_source.clone(),
            settings,
            show_settings: false,
            show_stats: false,
            devices: Vec::new(),
            worker: None,
            latest: None,
            events: Vec::new(),
            paused: false,
            seek_target: 0.0,
            seek_dragging: false,
            buffering: false,
            duration_s: None,
            live: false,
            error: None,
            raw_tex: None,
            canon_tex: None,
            toasts: Toasts::default(),
            session: SessionStore::load(),
            unhealthy_since: None,
            prev_game_state: None,
            last_ingame: None,
        }
    }

    fn running(&self) -> bool {
        self.worker.as_ref().is_some_and(|w| !w.join.is_finished())
    }

    fn is_replay(&self) -> bool {
        worker::is_replay_path(&self.source)
    }

    fn start(&mut self, start_s: f64) {
        self.stop();
        if self.source.is_empty() {
            self.error = Some("Choose a video file, capture device, or replay first.".into());
            return;
        }
        self.error = None;
        self.events.clear();
        self.paused = false;
        self.buffering = true;
        self.duration_s = None;
        self.live = false;
        self.latest = None;
        self.raw_tex = None;
        self.canon_tex = None;
        self.prev_game_state = None;
        self.last_ingame = None;
        self.settings.last_source = self.source.clone();
        self.settings.save();
        self.worker = Some(worker::spawn(
            self.source.clone(),
            self.settings.engine.clone(),
            self.settings.sink_options(),
            start_s,
            self.settings.speed,
        ));
    }

    fn stop(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.cmd.send(Cmd::Stop);
            let _ = worker.join.join();
        }
    }

    fn open_path(&mut self, path: std::path::PathBuf) {
        self.source = path.to_string_lossy().into_owned();
        self.start(0.0);
    }

    fn drain_updates(&mut self, ctx: &egui::Context) {
        let mut newest: Option<Box<GuiUpdate>> = None;
        if let Some(worker) = &self.worker {
            while let Ok(msg) = worker.updates.try_recv() {
                match msg {
                    WorkerMsg::Opened { duration_s, live } => {
                        self.duration_s = duration_s;
                        self.live = live;
                        self.buffering = false;
                    }
                    WorkerMsg::Update(update) => {
                        newest = Some(update);
                        self.buffering = false;
                    }
                    WorkerMsg::GameSaved(path) => {
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        self.toasts
                            .push(ToastKind::Success, format!("Recording saved: {name}"));
                    }
                    WorkerMsg::Ended => {
                        self.buffering = false;
                        self.toasts.push(ToastKind::Info, "End of stream");
                    }
                    WorkerMsg::Error(message) => {
                        self.buffering = false;
                        self.error = Some(message.clone());
                        self.toasts.push(ToastKind::Error, message);
                    }
                }
            }
        }
        // Events from every update (not just the newest) would need the
        // worker to batch them; latest-wins matches the previous behavior.
        if let Some(update) = newest {
            self.push_events(&update.output);
            self.track_game_end(&update);
            if update.raw_w == 0 {
                // Replay mode: no source video to preview.
                self.raw_tex = None;
            } else {
                let raw = egui::ColorImage::from_rgba_unmultiplied(
                    [update.raw_w, update.raw_h],
                    &update.raw_rgba,
                );
                match &mut self.raw_tex {
                    Some(tex) => tex.set(raw, egui::TextureOptions::LINEAR),
                    None => {
                        self.raw_tex =
                            Some(ctx.load_texture("raw", raw, egui::TextureOptions::LINEAR))
                    }
                }
            }
            if let Some(canon) = &update.canon_rgba {
                let img = egui::ColorImage::from_rgba_unmultiplied([256, 240], canon);
                match &mut self.canon_tex {
                    Some(tex) => tex.set(img, egui::TextureOptions::NEAREST),
                    None => {
                        self.canon_tex =
                            Some(ctx.load_texture("canon", img, egui::TextureOptions::NEAREST));
                    }
                }
            }
            if !self.seek_dragging {
                self.seek_target = update.position_s;
            }
            // Capture-health tracking for the CHECK CAPTURE alarm.
            let healthy = matches!(update.lock_state, "LOCKED" | "REPLAY")
                && update.output.confidence.overall >= 0.3;
            if healthy {
                self.unhealthy_since = None;
            } else if self.unhealthy_since.is_none() {
                self.unhealthy_since = Some(Instant::now());
            }
            self.latest = Some(update);
        }
    }

    /// Record a finished game into the session PB store when the state
    /// leaves play for game-over.
    fn track_game_end(&mut self, update: &GuiUpdate) {
        let state = update.output.game_state;
        if state == GameState::InGame {
            self.last_ingame = Some((update.output.clone(), update.position_s));
        }
        let was_playing = matches!(
            self.prev_game_state,
            Some(GameState::InGame | GameState::Paused)
        );
        if was_playing
            && state == GameState::GameOver
            && let Some((frame, _)) = self.last_ingame.take()
            && let Some(score) = frame.fields.score
        {
            let (date, time) = session::local_stamp();
            self.session.push(session::GameRecord {
                date,
                time,
                start_level: None,
                end_level: frame.fields.level,
                score,
                lines: frame.fields.lines.unwrap_or(0),
                tetris_rate: frame.stats.tetris_rate,
                duration_s: frame.stats.active_seconds.unwrap_or(0.0),
            });
            self.toasts.push(
                ToastKind::Info,
                format!("Game over — {score} points recorded"),
            );
        }
        self.prev_game_state = Some(state);
    }

    fn push_events(&mut self, output: &OutputFrame) {
        for ev in &output.events {
            let mm = (ev.ts / 60.0) as u32;
            let ss = ev.ts % 60.0;
            let extra = ev
                .new
                .as_ref()
                .map(|v| format!(" → {v}"))
                .unwrap_or_default();
            self.events.push(EventRow {
                text: format!("{mm}:{ss:04.1} {} {}{}", ev.field, ev.reason, extra),
                severity: if ev.reason == "clear_tetris" {
                    "gold".into()
                } else {
                    ev.severity.clone()
                },
            });
        }
        if self.events.len() > 300 {
            let excess = self.events.len() - 300;
            self.events.drain(..excess);
        }
    }

    fn send(&self, cmd: Cmd) {
        if let Some(worker) = &self.worker {
            let _ = worker.cmd.send(cmd);
        }
    }

    fn toggle_pause(&mut self) {
        if !self.running() || self.live {
            return;
        }
        self.paused = !self.paused;
        self.send(Cmd::Pause(self.paused));
    }

    fn seek_by(&mut self, delta_s: f64) {
        if !self.running() || self.live {
            return;
        }
        let max = self.duration_s.unwrap_or(f64::MAX);
        let target = (self.seek_target + delta_s).clamp(0.0, max);
        self.seek_target = target;
        self.buffering = !self.is_replay();
        self.send(Cmd::Seek(target));
    }

    fn step_frame(&mut self) {
        if !self.running() || self.live {
            return;
        }
        if !self.paused {
            self.paused = true;
            self.send(Cmd::Pause(true));
        }
        self.send(Cmd::StepFrame);
    }

    fn cycle_speed(&mut self, up: bool) {
        const SPEEDS: [f32; 6] = [0.25, 0.5, 1.0, 2.0, 4.0, -1.0];
        let current = SPEEDS
            .iter()
            .position(|&s| s == self.settings.speed)
            .unwrap_or(2);
        let next = if up {
            (current + 1).min(SPEEDS.len() - 1)
        } else {
            current.saturating_sub(1)
        };
        self.settings.speed = SPEEDS[next];
        self.send(Cmd::SetSpeed(self.settings.speed));
    }

    /// Global keyboard shortcuts (skipped while a text field has focus).
    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.wants_keyboard_input() {
            return;
        }
        let mut open_video = false;
        let mut fullscreen_toggle = false;
        // Read all keys in one pass.
        let (space, left, right, comma, period, up, down, key_r, key_o, f11) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Space),
                i.key_pressed(egui::Key::ArrowLeft),
                i.key_pressed(egui::Key::ArrowRight),
                i.key_pressed(egui::Key::Comma),
                i.key_pressed(egui::Key::Period),
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::R),
                i.key_pressed(egui::Key::O),
                i.key_pressed(egui::Key::F11),
            )
        });
        if space {
            self.toggle_pause();
        }
        if left {
            self.seek_by(-5.0);
        }
        if right {
            self.seek_by(5.0);
        }
        if comma {
            // One frame back = a short seek (the video pipe can't reverse).
            let fps_step = 1.0 / 30.0;
            self.seek_by(-fps_step);
            if !self.paused {
                self.paused = true;
                self.send(Cmd::Pause(true));
            }
        }
        if period {
            self.step_frame();
        }
        if up {
            self.cycle_speed(true);
        }
        if down {
            self.cycle_speed(false);
        }
        if key_r {
            self.send(Cmd::ResetLock);
        }
        if key_o {
            open_video = true;
        }
        if f11 {
            fullscreen_toggle = true;
        }
        if open_video
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("Video", &["mp4", "mkv", "avi", "mov", "webm", "ts"])
                .pick_file()
        {
            self.open_path(path);
        }
        if fullscreen_toggle {
            let is_full = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!is_full));
        }
    }

    /// Files dropped onto the window: videos and .ngf replays both open.
    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<std::path::PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if let Some(path) = dropped.into_iter().next() {
            self.open_path(path);
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_updates(ctx);
        ctx.request_repaint_after(std::time::Duration::from_millis(16));

        self.handle_shortcuts(ctx);
        self.handle_dropped_files(ctx);

        if settings_window(ctx, &mut self.show_settings, &mut self.settings) && self.running() {
            let pos = self.latest.as_ref().map(|u| u.position_s).unwrap_or(0.0);
            self.buffering = true;
            self.start(pos); // restart with the new configuration
        }

        let mut show_stats = self.show_stats;
        stats_panels::stats_window(
            ctx,
            &mut show_stats,
            self.latest.as_ref().map(|u| &u.output),
            self.latest.as_ref().map(|u| &u.ext),
            &self.session,
        );
        self.show_stats = show_stats;

        self.top_bar(ctx);
        self.side_panel(ctx);
        self.transport_bar(ctx);
        self.central(ctx);
        self.toasts.show(ctx);
    }
}

impl App {
    fn top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("nestris-core");
                ui.separator();
                if ui.button("Open video…").clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .add_filter("Video", &["mp4", "mkv", "avi", "mov", "webm", "ts"])
                        .pick_file()
                {
                    self.open_path(path);
                }
                if ui
                    .button("Open replay…")
                    .on_hover_text("Watch a recorded .ngf / .ngf.gz game")
                    .clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .add_filter("NGF replay", &["ngf", "gz", "part"])
                        .pick_file()
                {
                    self.open_path(path);
                }
                if ui
                    .button("Devices ⟳")
                    .on_hover_text("List DirectShow devices")
                    .clicked()
                {
                    self.devices = capture_ffmpeg::list_devices()
                        .map(|raw| {
                            raw.lines()
                                .filter(|l| l.contains("(video)"))
                                .filter_map(|l| l.split('"').nth(1).map(str::to_owned))
                                .collect()
                        })
                        .unwrap_or_default();
                }
                let selected = self
                    .source
                    .strip_prefix("dshow:")
                    .unwrap_or("camera…")
                    .to_owned();
                let mut pick: Option<String> = None;
                egui::ComboBox::from_id_salt("device")
                    .selected_text(selected)
                    .show_ui(ui, |ui| {
                        for device in &self.devices {
                            if ui.selectable_label(false, device).clicked() {
                                pick = Some(device.clone());
                            }
                        }
                    });
                if let Some(device) = pick {
                    self.source = format!("dshow:{device}");
                    self.start(0.0);
                }
                ui.separator();
                if self.running() {
                    if ui.button("⏹ Stop").clicked() {
                        self.stop();
                    }
                } else if ui.button("▶ Start").clicked() {
                    self.start(0.0);
                }
                if ui.button("Reset lock").clicked() {
                    self.send(Cmd::ResetLock);
                }
                if ui
                    .selectable_label(self.show_stats, "📊 Stats")
                    .on_hover_text("NestrisChamps-style statistics window")
                    .clicked()
                {
                    self.show_stats = !self.show_stats;
                }
                if ui.button("⚙ Settings").clicked() {
                    self.show_settings = !self.show_settings;
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (state, fps, recording) = self
                        .latest
                        .as_ref()
                        .map(|u| (u.lock_state, u.fps, u.recording))
                        .unwrap_or(("UNLOCKED", 0.0, false));
                    let color = match state {
                        "LOCKED" | "REPLAY" => egui::Color32::from_rgb(0x58, 0xd8, 0x54),
                        "DRIFT" => egui::Color32::from_rgb(0xfc, 0x98, 0x38),
                        _ => egui::Color32::GRAY,
                    };
                    ui.colored_label(color, state);
                    ui.label(format!("{fps:.0} fps"));
                    if recording {
                        ui.colored_label(egui::Color32::from_rgb(0xf8, 0x38, 0x00), "● REC")
                            .on_hover_text("Recording this game as .ngf.gz");
                    }
                    if self.buffering {
                        ui.spinner();
                    }
                });
            });
            if let Some(error) = &self.error {
                ui.colored_label(egui::Color32::from_rgb(0xf8, 0x38, 0x00), error);
            }
        });
    }

    fn transport_bar(&mut self, ctx: &egui::Context) {
        if self.live || !self.running() {
            return;
        }
        let Some(duration) = self
            .duration_s
            .or_else(|| self.latest.as_ref().and_then(|u| u.duration_s))
        else {
            return;
        };
        egui::TopBottomPanel::bottom("transport").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let pause_label = if self.paused { "▶" } else { "⏸" };
                if ui
                    .button(pause_label)
                    .on_hover_text("Play/pause (Space)")
                    .clicked()
                {
                    self.toggle_pause();
                }
                if ui
                    .button("|◀")
                    .on_hover_text("Back one frame (,)")
                    .clicked()
                {
                    self.seek_by(-1.0 / 30.0);
                    if !self.paused {
                        self.paused = true;
                        self.send(Cmd::Pause(true));
                    }
                }
                if ui
                    .button("▶|")
                    .on_hover_text("Step one frame (.)")
                    .clicked()
                {
                    self.step_frame();
                }
                let mut speed = self.settings.speed;
                egui::ComboBox::from_id_salt("speed")
                    .selected_text(if speed <= 0.0 {
                        "Max".to_owned()
                    } else {
                        format!("{speed}x")
                    })
                    .width(70.0)
                    .show_ui(ui, |ui| {
                        for (label, value) in [
                            ("0.25x", 0.25),
                            ("0.5x", 0.5),
                            ("1x", 1.0),
                            ("2x", 2.0),
                            ("4x", 4.0),
                            ("Max", -1.0),
                        ] {
                            ui.selectable_value(&mut speed, value, label);
                        }
                    });
                if speed != self.settings.speed {
                    self.settings.speed = speed;
                    self.send(Cmd::SetSpeed(speed));
                }

                let time_width = 150.0;
                let slider = egui::Slider::new(&mut self.seek_target, 0.0..=duration)
                    .show_value(false)
                    .trailing_fill(true);
                let response = ui.add_sized([ui.available_width() - time_width, 18.0], slider);
                // Hover time preview: map the pointer x onto the time axis.
                if let Some(pos) = response.hover_pos() {
                    let frac =
                        ((pos.x - response.rect.left()) / response.rect.width()).clamp(0.0, 1.0);
                    let t = duration * f64::from(frac);
                    response.clone().on_hover_text_at_pointer(format!(
                        "{:02}:{:04.1}",
                        (t / 60.0) as u32,
                        t % 60.0
                    ));
                }
                self.seek_dragging = response.dragged();
                if response.drag_stopped() {
                    self.buffering = !self.is_replay();
                    self.send(Cmd::Seek(self.seek_target));
                }
                ui.monospace(format!(
                    "{:02}:{:04.1} / {:02}:{:04.1}",
                    (self.seek_target / 60.0) as u32,
                    self.seek_target % 60.0,
                    (duration / 60.0) as u32,
                    duration % 60.0
                ));
            });
        });
    }

    fn side_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("side")
            .resizable(true)
            .default_width(320.0)
            .width_range(260.0..=480.0)
            .show(ctx, |ui| {
                let Some(update) = &self.latest else {
                    ui.label("Open a source to start tracking.");
                    return;
                };
                let output = &update.output;
                let fields = &output.fields;
                let conf = &output.confidence;

                // Capture alarm: sustained lock loss or low confidence.
                if self
                    .unhealthy_since
                    .is_some_and(|t| t.elapsed().as_secs_f64() > ALARM_AFTER_S)
                {
                    ui.vertical_centered(|ui| {
                        ui.colored_label(
                            egui::Color32::from_rgb(0xf8, 0x38, 0x00),
                            egui::RichText::new("⚠ CHECK CAPTURE").heading(),
                        );
                    });
                    ui.separator();
                }

                let state_text = format!("{:?}", output.game_state)
                    .to_uppercase()
                    .replace("GAMEOVER", "GAME OVER");
                ui.vertical_centered(|ui| {
                    ui.heading(state_text);
                });
                ui.separator();

                egui::Grid::new("dashboard").num_columns(2).show(ui, |ui| {
                    // Values gray out when their fused confidence is stale.
                    let dash = |ui: &mut egui::Ui, label: &str, value: String, conf: f64| {
                        ui.label(label);
                        if conf < STALE_CONFIDENCE {
                            ui.add_enabled(false, egui::Label::new(value));
                        } else {
                            ui.strong(value);
                        }
                        ui.end_row();
                    };
                    let opt = |v: Option<i64>| v.map_or("—".into(), |v| v.to_string());
                    dash(ui, "Score", opt(fields.score), conf.score);
                    dash(ui, "Lines", opt(fields.lines), conf.lines);
                    dash(ui, "Level", opt(fields.level), conf.level);
                    dash(
                        ui,
                        "Next",
                        fields
                            .next_piece
                            .map_or("—".into(), |p| p.letter().to_owned()),
                        conf.next_piece,
                    );
                    dash(ui, "Pieces", output.stats.pieces.to_string(), 1.0);
                    dash(
                        ui,
                        "Tetris rate",
                        output
                            .stats
                            .tetris_rate
                            .map_or("—".into(), |r| format!("{:.0}%", r * 100.0)),
                        1.0,
                    );
                    dash(
                        ui,
                        "PPS",
                        output.stats.pps.map_or("—".into(), |v| format!("{v:.2}")),
                        1.0,
                    );
                    dash(ui, "Burn", output.stats.burn.to_string(), 1.0);
                    dash(ui, "Drought", output.stats.drought.to_string(), 1.0);
                    dash(
                        ui,
                        "Clears",
                        format!(
                            "{}/{}/{}/{}",
                            output.stats.clears.single,
                            output.stats.clears.double,
                            output.stats.clears.triple,
                            output.stats.clears.tetris
                        ),
                        1.0,
                    );
                });
                ui.separator();
                ui.label("Events");
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for row in &self.events {
                            let color = match row.severity.as_str() {
                                "gold" => egui::Color32::GOLD,
                                "info" => egui::Color32::from_rgb(0x3c, 0xbc, 0xfc),
                                "warn" => egui::Color32::from_rgb(0xfc, 0x98, 0x38),
                                _ => egui::Color32::from_rgb(0xf8, 0x38, 0x00),
                            };
                            ui.colored_label(color, &row.text);
                        }
                    });
            });
    }

    fn central(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            // Responsive: canonical + playfield panes get fixed aspect
            // slices of the width; the raw preview takes the rest. Below a
            // threshold the panes wrap under the preview.
            let total = ui.available_width();
            let narrow = total < 900.0;
            let panes_width = if narrow { 0.0 } else { 336.0 + 176.0 };

            let render_panes = |ui: &mut egui::Ui| {
                ui.vertical(|ui| {
                    ui.label("Canonical");
                    if let Some(tex) = &self.canon_tex {
                        ui.add(egui::Image::new(tex).fit_to_exact_size(egui::vec2(320.0, 300.0)));
                    } else {
                        ui.allocate_space(egui::vec2(320.0, 300.0));
                    }
                });
                ui.vertical(|ui| {
                    ui.label("Tracked field");
                    self.playfield(ui);
                });
            };

            ui.horizontal_top(|ui| {
                // Raw preview + lock overlay.
                if let Some(tex) = &self.raw_tex {
                    let avail = (ui.available_width() - panes_width).max(200.0);
                    let size = tex.size_vec2();
                    let scale = (avail / size.x).clamp(0.1, 2.0);
                    let response = ui.add(egui::Image::new(tex).fit_to_exact_size(size * scale));
                    if let Some(update) = &self.latest
                        && let Some(quad) = update.lock_quad
                    {
                        let rect = response.rect;
                        let painter = ui.painter_at(rect);
                        let map = |p: (f32, f32)| rect.min + egui::vec2(p.0 * scale, p.1 * scale);
                        let pts = [map(quad[0]), map(quad[1]), map(quad[2]), map(quad[3])];
                        let stroke =
                            egui::Stroke::new(2.0, egui::Color32::from_rgb(0x3c, 0xbc, 0xfc));
                        for i in 0..4 {
                            painter.line_segment([pts[i], pts[(i + 1) % 4]], stroke);
                        }
                    }
                } else if self.is_replay() && self.latest.is_some() {
                    ui.vertical(|ui| {
                        ui.add_space(40.0);
                        ui.heading("▶ NGF Replay");
                        ui.label("Recorded game playback — no source video.");
                    });
                } else {
                    ui.allocate_space(egui::vec2(320.0, 240.0));
                }

                if !narrow {
                    render_panes(ui);
                }
            });
            if narrow {
                ui.separator();
                ui.horizontal_top(render_panes);
            }
        });
    }

    fn playfield(&self, ui: &mut egui::Ui) {
        let (response, painter) =
            ui.allocate_painter(egui::vec2(160.0, 320.0), egui::Sense::hover());
        let rect = response.rect;
        painter.rect_filled(rect, 4.0, egui::Color32::BLACK);
        let Some(update) = &self.latest else { return };
        let fields = &update.output.fields;
        let Some(grid) = &fields.playfield else {
            return;
        };
        let cw = rect.width() / 10.0;
        let ch = rect.height() / 20.0;
        let level = fields.level;
        let piece_cells: std::collections::HashSet<(u32, u32)> = fields
            .current_piece_cells
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .copied()
            .collect();
        let cell_rect = |r: usize, c: usize| {
            egui::Rect::from_min_size(
                rect.min + egui::vec2(c as f32 * cw + 1.0, r as f32 * ch + 1.0),
                egui::vec2(cw - 2.0, ch - 2.0),
            )
        };
        for (r, row) in grid.iter().enumerate() {
            for (c, &id) in row.iter().enumerate() {
                if id == 0 || piece_cells.contains(&(r as u32, c as u32)) {
                    continue;
                }
                let (cr, cg, cb) = nes_palette::cell_color(level, id);
                painter.rect_filled(cell_rect(r, c), 1.0, egui::Color32::from_rgb(cr, cg, cb));
            }
        }
        // Falling piece in its guideline color, on top.
        if let (Some(piece), Some(cells)) = (fields.current_piece, &fields.current_piece_cells)
            && piece != Piece::None
        {
            let (cr, cg, cb) = nes_palette::piece_color(Some(piece));
            for &(r, c) in cells {
                painter.rect_filled(
                    cell_rect(r as usize, c as usize),
                    1.0,
                    egui::Color32::from_rgb(cr, cg, cb),
                );
            }
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.stop();
        self.settings.save();
    }
}

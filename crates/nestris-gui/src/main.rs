//! Native desktop GUI (egui): the Rust counterpart of the Python PySide6
//! app. Source picker (file / DirectShow device), live raw preview with the
//! detected-playfield overlay, canonical preview, tracked-field rendering in
//! the NES level palette, dashboard tiles, event stream, transport bar
//! (pause / speed / seek), and a full settings dialog persisted as TOML.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod settings_ui;
mod worker;

use eframe::egui;
use nestris_engine::enums::Piece;
use nestris_engine::nes_palette;
use nestris_engine::output::OutputFrame;
use nestris_host::capture_ffmpeg;

use crate::settings_ui::{GuiSettings, settings_window};
use crate::worker::{Cmd, GuiUpdate, WorkerHandle};

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1480.0, 900.0])
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

struct App {
    settings: GuiSettings,
    show_settings: bool,
    source: String,
    devices: Vec<String>,
    worker: Option<WorkerHandle>,
    latest: Option<Box<GuiUpdate>>,
    events: Vec<EventRow>,
    paused: bool,
    seek_target: f64,
    seek_dragging: bool,
    error: Option<String>,
    raw_tex: Option<egui::TextureHandle>,
    canon_tex: Option<egui::TextureHandle>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let settings = GuiSettings::load();
        Self {
            source: settings.last_source.clone(),
            settings,
            show_settings: false,
            devices: Vec::new(),
            worker: None,
            latest: None,
            events: Vec::new(),
            paused: false,
            seek_target: 0.0,
            seek_dragging: false,
            error: None,
            raw_tex: None,
            canon_tex: None,
        }
    }

    fn running(&self) -> bool {
        self.worker.as_ref().is_some_and(|w| !w.join.is_finished())
    }

    fn start(&mut self, start_s: f64) {
        self.stop();
        if self.source.is_empty() {
            self.error = Some("Choose a video file or capture device first.".into());
            return;
        }
        self.error = None;
        self.events.clear();
        self.paused = false;
        self.latest = None;
        self.raw_tex = None;
        self.canon_tex = None;
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

    fn drain_updates(&mut self, ctx: &egui::Context) {
        let mut pending: Vec<Box<GuiUpdate>> = Vec::new();
        if let Some(worker) = &self.worker {
            while let Ok(update) = worker.updates.try_recv() {
                pending.push(update);
            }
        }
        let mut newest: Option<Box<GuiUpdate>> = None;
        for update in pending {
            self.push_events(&update.output);
            newest = Some(update);
        }
        if let Some(update) = newest {
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
            self.latest = Some(update);
        }
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
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_updates(ctx);
        ctx.request_repaint_after(std::time::Duration::from_millis(16));

        if settings_window(ctx, &mut self.show_settings, &mut self.settings) && self.running() {
            let pos = self.latest.as_ref().map(|u| u.position_s).unwrap_or(0.0);
            self.start(pos); // restart with the new configuration
        }

        self.top_bar(ctx);
        self.side_panel(ctx);
        self.transport_bar(ctx);
        self.central(ctx);
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
                    self.source = path.to_string_lossy().into_owned();
                    self.start(0.0);
                }
                if ui
                    .button("Open replay…")
                    .on_hover_text("Watch a recorded .ngf / .ngf.gz game")
                    .clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .add_filter("NGF replay", &["ngf", "gz", "part"])
                        .pick_file()
                {
                    self.source = path.to_string_lossy().into_owned();
                    self.start(0.0);
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
                if ui.button("⚙ Settings").clicked() {
                    self.show_settings = !self.show_settings;
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (state, fps) = self
                        .latest
                        .as_ref()
                        .map(|u| (u.lock_state, u.fps))
                        .unwrap_or(("UNLOCKED", 0.0));
                    let color = match state {
                        "LOCKED" => egui::Color32::from_rgb(0x58, 0xd8, 0x54),
                        "DRIFT" => egui::Color32::from_rgb(0xfc, 0x98, 0x38),
                        _ => egui::Color32::GRAY,
                    };
                    ui.colored_label(color, state);
                    ui.label(format!("{fps:.0} fps"));
                });
            });
            if let Some(error) = &self.error {
                ui.colored_label(egui::Color32::from_rgb(0xf8, 0x38, 0x00), error);
            }
        });
    }

    fn transport_bar(&mut self, ctx: &egui::Context) {
        let Some(update) = &self.latest else { return };
        let Some(duration) = update.duration_s else {
            return;
        };
        let is_live = self.source.starts_with("dshow:");
        if is_live || !self.running() {
            return;
        }
        egui::TopBottomPanel::bottom("transport").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let pause_label = if self.paused { "▶" } else { "⏸" };
                if ui.button(pause_label).clicked() {
                    self.paused = !self.paused;
                    self.send(Cmd::Pause(self.paused));
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
                let slider = egui::Slider::new(&mut self.seek_target, 0.0..=duration)
                    .show_value(false)
                    .trailing_fill(true);
                let response = ui.add_sized([ui.available_width() - 90.0, 18.0], slider);
                self.seek_dragging = response.dragged();
                if response.drag_stopped() {
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
            .default_width(320.0)
            .show(ctx, |ui| {
                let Some(update) = &self.latest else {
                    ui.label("Open a source to start tracking.");
                    return;
                };
                let output = &update.output;
                let fields = &output.fields;

                let state_text = format!("{:?}", output.game_state)
                    .to_uppercase()
                    .replace("GAMEOVER", "GAME OVER");
                ui.vertical_centered(|ui| {
                    ui.heading(state_text);
                });
                ui.separator();

                egui::Grid::new("dashboard").num_columns(2).show(ui, |ui| {
                    let dash = |ui: &mut egui::Ui, label: &str, value: String| {
                        ui.label(label);
                        ui.strong(value);
                        ui.end_row();
                    };
                    let opt = |v: Option<i64>| v.map_or("—".into(), |v| v.to_string());
                    dash(ui, "Score", opt(fields.score));
                    dash(ui, "Lines", opt(fields.lines));
                    dash(ui, "Level", opt(fields.level));
                    dash(
                        ui,
                        "Next",
                        fields
                            .next_piece
                            .map_or("—".into(), |p| p.letter().to_owned()),
                    );
                    dash(ui, "Pieces", output.stats.pieces.to_string());
                    dash(
                        ui,
                        "Tetris rate",
                        output
                            .stats
                            .tetris_rate
                            .map_or("—".into(), |r| format!("{:.0}%", r * 100.0)),
                    );
                    dash(
                        ui,
                        "PPS",
                        output.stats.pps.map_or("—".into(), |v| format!("{v:.2}")),
                    );
                    dash(ui, "Burn", output.stats.burn.to_string());
                    dash(ui, "Drought", output.stats.drought.to_string());
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
            ui.horizontal_top(|ui| {
                // Raw preview + lock overlay.
                if let Some(tex) = &self.raw_tex {
                    let avail = ui.available_width() - 560.0;
                    let size = tex.size_vec2();
                    let scale = (avail / size.x).clamp(0.1, 1.6);
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
                } else {
                    ui.allocate_space(egui::vec2(480.0, 360.0));
                }

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
            });
        });
    }

    fn playfield(&self, ui: &mut egui::Ui) {
        let (response, painter) =
            ui.allocate_painter(egui::vec2(200.0, 400.0), egui::Sense::hover());
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

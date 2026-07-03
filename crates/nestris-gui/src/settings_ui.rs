//! The settings window: every EngineConfig knob (mirroring the Python
//! settings dialog) plus host output sinks, persisted as TOML in the
//! per-user config directory.

use std::path::PathBuf;

use nestris_engine::config::EngineConfig;
use serde::{Deserialize, Serialize};

use crate::worker::SinkOptions;

/// Bumped when new defaults should be applied to settings files written by
/// older versions (see [`GuiSettings::load`]).
const SETTINGS_VERSION: u32 = 2;

/// Everything the GUI persists between sessions.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GuiSettings {
    pub settings_version: u32,
    pub engine: EngineConfig,
    pub ws_enabled: bool,
    pub ws_addr: String,
    pub jsonl_enabled: bool,
    pub jsonl_path: String,
    /// Record every detected game as .ngf.gz.
    pub record_enabled: bool,
    /// Recording directory; empty = Documents\nestris-recordings.
    pub record_dir: String,
    pub last_source: String,
    pub speed: f32,
}

impl Default for GuiSettings {
    fn default() -> Self {
        let mut engine = EngineConfig::default();
        // GUI default: continuous geometry tracking on. The engine-level
        // default stays off (oracle parity for CLI verification runs).
        engine.tracking.enabled = true;
        Self {
            settings_version: SETTINGS_VERSION,
            engine,
            ws_enabled: false,
            ws_addr: String::new(),
            jsonl_enabled: false,
            jsonl_path: String::new(),
            record_enabled: true,
            record_dir: String::new(),
            last_source: String::new(),
            speed: 1.0,
        }
    }
}

impl GuiSettings {
    pub fn load() -> GuiSettings {
        let mut settings: GuiSettings = std::fs::read_to_string(settings_path())
            .ok()
            .and_then(|raw| toml::from_str(&raw).ok())
            .unwrap_or_default();
        if settings.ws_addr.is_empty() {
            settings.ws_addr = "127.0.0.1:8765".into();
        }
        if settings.speed <= 0.0 && settings.speed != -1.0 {
            settings.speed = 1.0;
        }
        // One-time migration for settings files from before these features
        // existed: enable their GUI defaults.
        if settings.settings_version < 2 {
            settings.engine.tracking.enabled = true;
            settings.record_enabled = true;
            settings.settings_version = SETTINGS_VERSION;
        }
        settings
    }

    pub fn save(&self) {
        let path = settings_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(raw) = toml::to_string_pretty(self) {
            let _ = std::fs::write(path, raw);
        }
    }

    pub fn sink_options(&self) -> SinkOptions {
        SinkOptions {
            jsonl_path: (self.jsonl_enabled && !self.jsonl_path.is_empty())
                .then(|| PathBuf::from(&self.jsonl_path)),
            ws_addr: self.ws_enabled.then(|| self.ws_addr.clone()),
            record: self.record_enabled,
            record_dir: (!self.record_dir.is_empty()).then(|| PathBuf::from(&self.record_dir)),
        }
    }
}

fn settings_path() -> PathBuf {
    let base = std::env::var("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    base.join("nestris-core").join("gui-settings.toml")
}

/// Render the settings form. Returns `true` when Apply was clicked.
pub fn settings_window(ctx: &egui::Context, open: &mut bool, settings: &mut GuiSettings) -> bool {
    let mut apply = false;
    egui::Window::new("Settings")
        .open(open)
        .default_width(420.0)
        .vscroll(true)
        .show(ctx, |ui| {
            let e = &mut settings.engine;

            egui::CollapsingHeader::new("Calibration")
                .default_open(true)
                .show(ui, |ui| {
                    slider_f64(
                        ui,
                        &mut e.calibration.acquire_threshold,
                        0.0..=1.0,
                        "Acquire threshold",
                    );
                    slider_f64(
                        ui,
                        &mut e.calibration.drift_threshold,
                        0.0..=1.0,
                        "Drift threshold",
                    );
                    drag_u32(ui, &mut e.calibration.lost_frames, "Lost after weak frames");
                    drag_u32(
                        ui,
                        &mut e.calibration.acquire_frames,
                        "Frames to confirm acquire",
                    );
                    slider_f64(
                        ui,
                        &mut e.calibration.smooth_alpha,
                        0.0..=1.0,
                        "Geometry smoothing (EMA)",
                    );
                    slider_f64(
                        ui,
                        &mut e.calibration.adopt_margin,
                        0.0..=0.5,
                        "Re-solve adopt margin",
                    );
                    let mut undistort = e.calibration.undistort == "auto";
                    ui.checkbox(&mut undistort, "Barrel undistortion (auto)");
                    e.calibration.undistort = if undistort {
                        "auto".into()
                    } else {
                        "off".into()
                    };
                    ui.checkbox(
                        &mut e.calibration.background_recalibration,
                        "Background recalibration",
                    );
                    ui.checkbox(
                        &mut e.calibration.menu_drift_hold,
                        "Hold lock through menus",
                    );
                });

            egui::CollapsingHeader::new("Fusion").show(ui, |ui| {
                drag_usize(ui, &mut e.fusion.vote_window, "Vote window (frames)");
                slider_f64(
                    ui,
                    &mut e.fusion.confidence_decay,
                    0.5..=1.0,
                    "Confidence decay",
                );
                slider_f64(
                    ui,
                    &mut e.fusion.min_report_confidence,
                    0.0..=1.0,
                    "Min report confidence",
                );
                ui.checkbox(
                    &mut e.fusion.enforce_monotonic,
                    "Enforce monotonic score/lines/level",
                );
                drag_u32(
                    ui,
                    &mut e.fusion.new_game_menu_frames,
                    "Menu frames to arm new game",
                );
            });

            egui::CollapsingHeader::new("Plausibility").show(ui, |ui| {
                ui.checkbox(&mut e.plausibility.enabled, "Enabled (NES-rules guard)");
                drag_i64(ui, &mut e.plausibility.max_score_jump, "Max score jump");
                drag_i64(
                    ui,
                    &mut e.plausibility.max_lines_step,
                    "Max lines step (contiguous)",
                );
                drag_i64(
                    ui,
                    &mut e.plausibility.max_lines_skip,
                    "Max lines skip (across gaps)",
                );
                drag_i64(ui, &mut e.plausibility.level_tolerance, "Level tolerance");
                drag_u32(
                    ui,
                    &mut e.plausibility.confirm_frames,
                    "Self-heal after frames",
                );
            });

            egui::CollapsingHeader::new("Recognition").show(ui, |ui| {
                egui::ComboBox::from_label("Score base")
                    .selected_text(e.recognition.score_base.clone())
                    .show_ui(ui, |ui| {
                        for base in ["auto", "dec", "hex"] {
                            ui.selectable_value(&mut e.recognition.score_base, base.into(), base);
                        }
                    });
                drag_u32(
                    ui,
                    &mut e.recognition.score_base_latch_frames,
                    "Base latch frames",
                );
                ui.checkbox(&mut e.recognition.read_statistics, "Read STATISTICS rail");
                drag_u32(
                    ui,
                    &mut e.recognition.statistics_every_n,
                    "STATISTICS every N frames",
                );
                ui.checkbox(&mut e.recognition.read_current_piece, "Track current piece");
                ui.checkbox(
                    &mut e.recognition.freeze_on_clear_animation,
                    "Freeze during clear animation",
                );
                ui.checkbox(
                    &mut e.recognition.playfield_stabilizer,
                    "Playfield stabilizer",
                );
            });

            egui::CollapsingHeader::new("Tracking").show(ui, |ui| {
                ui.checkbox(
                    &mut e.tracking.enabled,
                    "Continuous geometry tracking (handheld footage)",
                )
                .on_hover_text(
                    "Follows small per-frame camera motion while locked. \
                     Idles on stable capture-card sources.",
                );
                ui.add_enabled_ui(e.tracking.enabled, |ui| {
                    let mut radius = e.tracking.search_radius_px as i32;
                    ui.add(
                        egui::Slider::new(&mut radius, 4..=16).text("Label search radius (px)"),
                    );
                    e.tracking.search_radius_px = radius as u32;
                    slider_f64(
                        ui,
                        &mut e.tracking.damping,
                        0.1..=1.0,
                        "Correction damping",
                    );
                });
            });

            egui::CollapsingHeader::new("Output").show(ui, |ui| {
                ui.checkbox(&mut settings.record_enabled, "Record games (.ngf.gz)")
                    .on_hover_text(
                        "Automatically saves one NestrisChamps-format recording per game",
                    );
                ui.add_enabled_ui(settings.record_enabled, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut settings.record_dir)
                                .hint_text("Documents\\nestris-recordings"),
                        );
                        if ui.button("…").clicked()
                            && let Some(dir) = rfd::FileDialog::new().pick_folder()
                        {
                            settings.record_dir = dir.to_string_lossy().into_owned();
                        }
                    });
                });
                ui.separator();
                ui.checkbox(&mut settings.ws_enabled, "WebSocket broadcast");
                ui.add_enabled(
                    settings.ws_enabled,
                    egui::TextEdit::singleline(&mut settings.ws_addr).hint_text("127.0.0.1:8765"),
                );
                ui.checkbox(&mut settings.jsonl_enabled, "JSONL file");
                ui.horizontal(|ui| {
                    ui.add_enabled(
                        settings.jsonl_enabled,
                        egui::TextEdit::singleline(&mut settings.jsonl_path).hint_text("out.jsonl"),
                    );
                    if settings.jsonl_enabled
                        && ui.button("…").clicked()
                        && let Some(path) = rfd::FileDialog::new()
                            .add_filter("JSONL", &["jsonl"])
                            .save_file()
                    {
                        settings.jsonl_path = path.to_string_lossy().into_owned();
                    }
                });
            });

            ui.separator();
            ui.horizontal(|ui| {
                if ui
                    .button("Apply & save")
                    .on_hover_text("Restarts the pipeline with the new configuration")
                    .clicked()
                {
                    settings.save();
                    apply = true;
                }
                if ui.button("Reset to defaults").clicked() {
                    settings.engine = EngineConfig::default();
                }
            });
        });
    apply
}

fn slider_f64(
    ui: &mut egui::Ui,
    value: &mut f64,
    range: std::ops::RangeInclusive<f64>,
    label: &str,
) {
    ui.add(egui::Slider::new(value, range).text(label));
}

fn drag_u32(ui: &mut egui::Ui, value: &mut u32, label: &str) {
    ui.horizontal(|ui| {
        ui.add(egui::DragValue::new(value));
        ui.label(label);
    });
}

fn drag_i64(ui: &mut egui::Ui, value: &mut i64, label: &str) {
    ui.horizontal(|ui| {
        ui.add(egui::DragValue::new(value));
        ui.label(label);
    });
}

fn drag_usize(ui: &mut egui::Ui, value: &mut usize, label: &str) {
    ui.horizontal(|ui| {
        ui.add(egui::DragValue::new(value));
        ui.label(label);
    });
}

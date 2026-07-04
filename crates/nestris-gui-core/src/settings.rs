//! Persisted GUI settings: every EngineConfig knob (mirroring the Python
//! settings dialog) plus host output sinks, stored as TOML in the per-user
//! config directory. Each frontend passes its own file name so the egui
//! and Qt apps never clobber each other's settings.

use std::path::PathBuf;

use nestris_engine::config::EngineConfig;
use serde::{Deserialize, Serialize};

use crate::worker::SinkOptions;

/// Bumped when new defaults should be applied to settings files written by
/// older versions (see [`GuiSettings::load_from`]).
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
    pub fn load_from(file_name: &str) -> GuiSettings {
        let mut settings: GuiSettings = std::fs::read_to_string(settings_path(file_name))
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

    pub fn save_to(&self, file_name: &str) {
        let path = settings_path(file_name);
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

fn settings_path(file_name: &str) -> PathBuf {
    crate::config_dir().join(file_name)
}

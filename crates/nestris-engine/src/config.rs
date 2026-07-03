//! Engine configuration mirroring the Python `config.py` structures and
//! default values exactly (the wire/tuning contract).

use serde::{Deserialize, Serialize};

use crate::enums::Region;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct CalibrationConfig {
    pub auto: bool,
    pub revalidate_every_n: u32,
    pub acquire_threshold: f64,
    pub drift_threshold: f64,
    pub lost_frames: u32,
    pub acquire_frames: u32,
    pub smooth_alpha: f64,
    /// `"auto"` engages barrel undistortion only when bow is detected; `"off"`.
    pub undistort: String,
    pub adopt_margin: f64,
    pub background_recalibration: bool,
    pub menu_drift_hold: bool,
}

impl Default for CalibrationConfig {
    fn default() -> Self {
        Self {
            auto: true,
            revalidate_every_n: 3,
            acquire_threshold: 0.55,
            drift_threshold: 0.40,
            lost_frames: 12,
            acquire_frames: 2,
            smooth_alpha: 0.5,
            undistort: "auto".into(),
            adopt_margin: 0.05,
            background_recalibration: true,
            menu_drift_hold: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct FusionConfig {
    pub vote_window: usize,
    pub confidence_decay: f64,
    pub min_report_confidence: f64,
    pub enforce_monotonic: bool,
    pub new_game_menu_frames: u32,
}

impl Default for FusionConfig {
    fn default() -> Self {
        Self {
            vote_window: 5,
            confidence_decay: 0.9,
            min_report_confidence: 0.4,
            enforce_monotonic: true,
            new_game_menu_frames: 10,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PlausibilityConfig {
    pub enabled: bool,
    pub max_score_jump: i64,
    pub max_lines_step: i64,
    pub max_lines_skip: i64,
    pub level_tolerance: i64,
    pub confirm_frames: u32,
    pub scoring: String,
}

impl Default for PlausibilityConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_score_jump: 60_000,
            max_lines_step: 4,
            max_lines_skip: 8,
            level_tolerance: 1,
            confirm_frames: 6,
            scoring: "nes_ntsc".into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RecognitionConfig {
    /// `"dec"`, `"hex"`, or `"auto"`.
    pub score_base: String,
    pub score_base_latch_frames: u32,
    pub read_statistics: bool,
    pub statistics_every_n: u32,
    pub read_current_piece: bool,
    pub freeze_on_clear_animation: bool,
    pub playfield_stabilizer: bool,
}

impl Default for RecognitionConfig {
    fn default() -> Self {
        Self {
            score_base: "auto".into(),
            score_base_latch_frames: 60,
            read_statistics: true,
            statistics_every_n: 6,
            read_current_piece: true,
            freeze_on_clear_animation: true,
            playfield_stabilizer: true,
        }
    }
}

/// Top-level engine configuration (host I/O settings live with the hosts).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineConfig {
    pub region: Option<Region>,
    pub calibration: CalibrationConfig,
    pub fusion: FusionConfig,
    pub plausibility: PlausibilityConfig,
    pub recognition: RecognitionConfig,
}

impl EngineConfig {
    pub fn region(&self) -> Region {
        self.region.unwrap_or(Region::Ntsc)
    }
}

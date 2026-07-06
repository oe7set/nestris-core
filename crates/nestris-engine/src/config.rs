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
    /// Run acquisition solves on the host's background solver instead of
    /// inline on the pipeline thread (requires `background_recalibration`).
    /// Default off: the oracle-verified inline acquisition stays bit-identical.
    pub background_acquisition: bool,
    /// Downscale the frame to this width for playfield-candidate detection
    /// (labels/RANSAC/validation stay at full resolution). `0` = off
    /// (bit-identical oracle path); GUIs/web use `640`.
    pub acquire_downscale_width: u32,
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
            background_acquisition: false,
            acquire_downscale_width: 0,
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
    /// Temporal per-cell color voting over the last few reads instead of
    /// "hold previous id when ambiguous". Reduces color flicker.
    pub color_voting: bool,
    /// Weight of the hue-angle term in accent color assignment, `0.0..=1.0`.
    /// `0.0` keeps the pure CIELAB nearest-target behavior.
    pub color_hue_weight: f64,
    /// Scale the ambiguity threshold by how separable the level's palette
    /// actually is, flagging ambiguity earlier on close accent pairs.
    pub adaptive_ambiguity: bool,
    /// Estimate per-channel gains from white-classified cells and re-assign
    /// colors once with rebalanced targets.
    pub white_balance: bool,
    /// Force all cells of the falling piece to its majority color (a piece
    /// is a single color by construction).
    pub piece_color_uniform: bool,
    /// Predict the post-clear board when a clear animation starts and
    /// validate the first post-animation reading against it.
    pub clear_prediction: bool,
    /// When a predicted clear crosses a level-up boundary, hint the next
    /// level's palette to the playfield reader until fusion catches up.
    pub level_hint_on_clear: bool,
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
            color_voting: false,
            color_hue_weight: 0.0,
            adaptive_ambiguity: false,
            white_balance: false,
            piece_color_uniform: false,
            clear_prediction: false,
            level_hint_on_clear: false,
        }
    }
}

/// Continuous per-frame geometry micro-tracking for unstable (handheld)
/// sources. Default off: the oracle-verified pipeline is bit-identical with
/// tracking disabled, and stable capture-card sources don't need it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct TrackingConfig {
    pub enabled: bool,
    /// Search window half-size around each HUD label, in canonical pixels.
    pub search_radius_px: u32,
    /// Minimum NCC peak score for a label match to count.
    pub min_label_score: f64,
    /// Mean correction below this is ignored entirely (zero-cost idle path).
    pub deadband_px: f64,
    /// Blend factor toward the fitted correction (`0.0` = ignore, `1.0` = full).
    pub damping: f64,
    /// Label offsets larger than this are treated as mismatches, not motion.
    pub max_correction_px: f64,
    /// Consecutive tracker misses before escalating to the drift path.
    pub miss_escalate: u32,
    /// Sustained motion (EMA of corrections) above this relaxes the
    /// never-regress adoption margin for background solves.
    pub motion_adopt_threshold_px: f64,
    /// Background solve pacing while the tracker reports urgency (seconds).
    pub drift_solve_interval_s: f64,
}

impl Default for TrackingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            search_radius_px: 8,
            min_label_score: 0.4,
            deadband_px: 0.35,
            damping: 0.6,
            max_correction_px: 12.0,
            miss_escalate: 4,
            motion_adopt_threshold_px: 1.0,
            drift_solve_interval_s: 0.15,
        }
    }
}

/// Output-shaping options. Default off so the serialized `OutputFrame`
/// stays byte-identical to the verified schema.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputConfig {
    /// Attach the `ExtendedStats` block (dashboards, pace, board metrics)
    /// to every output frame.
    pub extended_stats: bool,
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
    pub tracking: TrackingConfig,
    pub output: OutputConfig,
}

impl EngineConfig {
    pub fn region(&self) -> Region {
        self.region.unwrap_or(Region::Ntsc)
    }

    /// Preset for handheld/phone footage: continuous geometry tracking on,
    /// faster background solves. Everything else stays at defaults.
    pub fn apply_handheld_preset(&mut self) {
        self.tracking.enabled = true;
    }
}

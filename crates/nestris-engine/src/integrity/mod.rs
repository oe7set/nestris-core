//! Per-game integrity checks for tournament hosts: cheat detection (score
//! gains no legal NES scoring event explains) and an end-of-game validation
//! report over the emitted output stream.
//!
//! Everything here is a pure consumer of [`OutputFrame`]s. It is *not* wired
//! into [`crate::processor::FrameProcessor::process`], so the verified
//! schema-v4 output path stays byte-identical; hosts (the station daemon,
//! GUIs) run it alongside the processor.
//!
//! [`OutputFrame`]: crate::output::OutputFrame

pub mod cheat;
pub mod validate;

use serde::{Deserialize, Serialize};

/// Tuning for [`cheat::CheatDetector`] and [`validate::GameValidator`].
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct IntegrityConfig {
    /// Points one cheat input awards (the Select trick adds 10 000).
    pub cheat_points: i64,
    /// Tolerance around each multiple of `cheat_points`.
    pub cheat_slack: i64,
    /// In-game frames a new SCORE/LINES value must hold before it counts
    /// (filters OCR glitches that survived fusion).
    pub confirm_frames: u32,
    /// Frames a score gain waits for a matching LINES change (SCORE and
    /// LINES settle on different frames in the OCR).
    pub pair_window_frames: u32,
    /// Soft-drop (push-down) points allowed on top of a scoring event.
    pub softdrop_slack: i64,
    /// Unexplained gains above this (and not a cheat) raise an anomaly.
    pub anomaly_threshold: i64,
    /// Upper bound of push-down points per piece, for the game-level
    /// score reconciliation.
    pub softdrop_per_piece: i64,
    /// Allowed unexplained points in the game-level reconciliation.
    pub reconcile_tolerance: i64,
    /// Allowed difference between counted clears and the LINES counter.
    pub lines_tolerance: i64,
    /// `confidence.overall` below this marks an in-game frame low-confidence.
    pub low_confidence: f64,
    /// Low-confidence frame ratio for a warning / an error.
    pub low_confidence_warn_ratio: f64,
    pub low_confidence_error_ratio: f64,
    /// Seconds of lost signal during a game for a warning / an error.
    pub signal_lost_warn_s: f64,
    pub signal_lost_error_s: f64,
    /// Plausibility rejections per game before a warning.
    pub plausibility_warn_count: u32,
}

impl Default for IntegrityConfig {
    fn default() -> Self {
        Self {
            cheat_points: 10_000,
            cheat_slack: 250,
            confirm_frames: 10,
            pair_window_frames: 30,
            softdrop_slack: 200,
            anomaly_threshold: 2_000,
            softdrop_per_piece: 40,
            reconcile_tolerance: 1_000,
            lines_tolerance: 1,
            low_confidence: 0.5,
            low_confidence_warn_ratio: 0.10,
            low_confidence_error_ratio: 0.30,
            signal_lost_warn_s: 2.0,
            signal_lost_error_s: 10.0,
            plausibility_warn_count: 20,
        }
    }
}

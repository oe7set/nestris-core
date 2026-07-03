//! Per-frame calibration lock state machine (port of `geometry/lock.py`).
//!
//! The Python background-recalibrator *thread* becomes a sans-io protocol:
//! when `background` mode is on, [`CalibrationLock::wants_background_solve`]
//! tells the host a solve would help, the host runs [`estimate_geometry`]
//! wherever it likes (thread, Web Worker, inline) and hands the result back
//! via [`CalibrationLock::offer_solution`]; adoption follows the same
//! never-regress hysteresis as the Python `_poll_recalibrator`.

use std::sync::Arc;

use nestris_vision::Image;
use nestris_vision::homography::{Mat3, mat3_inv, project};
use nestris_vision::undistort::UndistortMap;

use crate::config::CalibrationConfig;
use crate::geometry_cal::calibration::{GeometryResult, Rectifier, estimate_geometry};
use crate::layout::{LayoutTable, get_layout};

const REVALIDATE_MIN_DARK: f64 = 0.12;
const SMALL_CHANGE_SHIFT_PX: f64 = 24.0;

/// Calibration lock lifecycle state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockState {
    Unlocked,
    Acquiring,
    Locked,
    Drift,
    Lost,
}

impl LockState {
    pub fn name(&self) -> &'static str {
        match self {
            LockState::Unlocked => "UNLOCKED",
            LockState::Acquiring => "ACQUIRING",
            LockState::Locked => "LOCKED",
            LockState::Drift => "DRIFT",
            LockState::Lost => "LOST",
        }
    }
}

/// Result of one lock update phase.
pub struct LockStatus {
    pub state: LockState,
    pub usable: bool,
    pub geometry_confidence: f64,
}

/// Stateful per-frame calibration tracker.
pub struct CalibrationLock {
    cfg: CalibrationConfig,
    layout: &'static LayoutTable,
    /// Host-driven background solves (the recalibrator protocol) vs inline
    /// periodic solves (the deterministic oracle-parity mode).
    background: bool,
    state: LockState,
    homography: Option<Mat3>,
    undistort: Option<Arc<UndistortMap>>,
    undistort_tried: bool,
    rectifier: Option<Rectifier>,
    confidence: f64,
    frame_index: u64,
    acquire_streak: u32,
    drift_streak: u32,
    pending_confirm: bool,
    pending_score: Option<f64>,
    /// Latest candidate offered by the host's background solver.
    offered: Option<GeometryResult>,
    /// Whether the current frame is one the host may snapshot for a solve.
    wants_solve: bool,
}

impl CalibrationLock {
    pub fn new(cfg: CalibrationConfig) -> Self {
        let background = cfg.background_recalibration;
        Self {
            cfg,
            layout: get_layout(),
            background,
            state: LockState::Unlocked,
            homography: None,
            undistort: None,
            undistort_tried: false,
            rectifier: None,
            confidence: 0.0,
            frame_index: 0,
            acquire_streak: 0,
            drift_streak: 0,
            pending_confirm: false,
            pending_score: None,
            offered: None,
            wants_solve: false,
        }
    }

    pub fn state(&self) -> LockState {
        self.state
    }

    pub fn rectifier(&self) -> Option<&Rectifier> {
        let usable = matches!(self.state, LockState::Locked | LockState::Drift);
        if usable {
            self.rectifier.as_ref()
        } else {
            None
        }
    }

    pub fn confidence(&self) -> f64 {
        self.confidence
    }

    /// Whether the host should snapshot the current frame for an off-thread
    /// solve (background mode, tracking, on-gameplay frames only).
    pub fn wants_background_solve(&self) -> bool {
        self.wants_solve
    }

    /// The current confidence a background solve must beat (with margin).
    pub fn current_score(&self) -> f64 {
        self.confidence
    }

    /// The undistort map background solves should reuse (skips estimation).
    pub fn undistort_map(&self) -> Option<Arc<UndistortMap>> {
        self.undistort.clone()
    }

    /// Hand a host-computed background solve to the lock; adopted next frame
    /// under the never-regress rule.
    pub fn offer_solution(&mut self, result: GeometryResult) {
        self.offered = Some(result);
    }

    pub fn reset(&mut self) {
        self.state = LockState::Unlocked;
        self.homography = None;
        self.undistort = None;
        self.undistort_tried = false;
        self.rectifier = None;
        self.confidence = 0.0;
        self.acquire_streak = 0;
        self.drift_streak = 0;
        self.pending_confirm = false;
        self.pending_score = None;
        self.offered = None;
        self.wants_solve = false;
    }

    fn try_undistort(&self) -> bool {
        self.cfg.undistort != "off" && self.undistort.is_none() && !self.undistort_tried
    }

    /// Phase 1 of a frame update: acquisition / geometry adoption.
    pub fn prepare(&mut self, image: &Image, hold_drift: bool) -> LockStatus {
        self.frame_index += 1;
        self.pending_confirm = false;
        self.pending_score = None;
        self.wants_solve = false;
        if matches!(
            self.state,
            LockState::Unlocked | LockState::Lost | LockState::Acquiring
        ) {
            return self.acquire(image);
        }
        self.track_prepare(image, hold_drift)
    }

    /// Phase 2: the deferred cheap revalidation on the caller's canonical luma.
    pub fn confirm(&mut self, canon_gray: Option<&Image>, hold_drift: bool) -> LockStatus {
        if !self.pending_confirm {
            return self.status();
        }
        self.pending_confirm = false;
        if canon_gray
            .map(|g| self.dark_fraction_ok(g))
            .unwrap_or(false)
        {
            self.state = LockState::Locked;
            self.drift_streak = 0;
        } else {
            let score = self.pending_score.unwrap_or(self.confidence);
            self.on_weak(score, hold_drift);
        }
        self.pending_score = None;
        self.status()
    }

    fn acquire(&mut self, image: &Image) -> LockStatus {
        self.state = LockState::Acquiring;
        let result = estimate_geometry(
            image,
            self.layout,
            self.undistort.clone(),
            self.try_undistort(),
            self.frame_index,
        );
        if self.cfg.undistort != "off" {
            self.undistort_tried = true;
        }
        if result.ok() && result.confidence >= self.cfg.acquire_threshold {
            self.acquire_streak += 1;
            self.adopt(&result);
            if self.acquire_streak >= self.cfg.acquire_frames {
                self.state = LockState::Locked;
                self.drift_streak = 0;
            }
        } else {
            self.acquire_streak = 0;
            self.confidence = if result.ok() { result.confidence } else { 0.0 };
        }
        self.status()
    }

    fn track_prepare(&mut self, image: &Image, hold_drift: bool) -> LockStatus {
        if self.background {
            if !hold_drift {
                // The host may snapshot this frame for an off-thread solve;
                // adopt any offered candidate under the hysteresis rule.
                self.wants_solve = true;
                if let Some(candidate) = self.offered.take()
                    && candidate.ok()
                    && self.should_adopt(candidate.confidence)
                {
                    self.adopt(&candidate);
                }
            }
            self.pending_confirm = true;
            return self.status();
        }

        let do_full = self
            .frame_index
            .is_multiple_of(self.cfg.revalidate_every_n.max(1) as u64);
        if do_full && !hold_drift {
            let result = estimate_geometry(
                image,
                self.layout,
                self.undistort.clone(),
                self.try_undistort(),
                self.frame_index,
            );
            if self.cfg.undistort != "off" {
                self.undistort_tried = true;
            }
            let score = if result.ok() { result.confidence } else { 0.0 };
            if result.ok() && self.should_adopt(score) {
                self.adopt(&result);
                self.state = LockState::Locked;
                self.drift_streak = 0;
                return self.status();
            }
            self.pending_confirm = true;
            self.pending_score = Some(score);
            return self.status();
        }

        self.pending_confirm = true;
        self.status()
    }

    fn should_adopt(&self, score: f64) -> bool {
        score >= self.cfg.drift_threshold && score > self.confidence + self.cfg.adopt_margin
    }

    fn on_weak(&mut self, score: f64, hold: bool) {
        if hold {
            self.state = LockState::Locked;
            return;
        }
        self.confidence = score;
        self.drift_streak += 1;
        self.state = LockState::Drift;
        if self.drift_streak >= self.cfg.lost_frames {
            self.reset();
            self.state = LockState::Lost;
        }
    }

    fn dark_fraction_ok(&self, canon_gray: &Image) -> bool {
        let (x0, y0, x1, y1) = self.layout.playfield.to_bounds();
        let (x1, y1) = (x1.min(canon_gray.width), y1.min(canon_gray.height));
        if x1 <= x0 || y1 <= y0 {
            return false;
        }
        let mut dark = 0usize;
        let mut total = 0usize;
        for y in y0..y1 {
            for x in x0..x1 {
                total += 1;
                if canon_gray.data[y * canon_gray.width + x] < 64 {
                    dark += 1;
                }
            }
        }
        dark as f64 / total as f64 >= REVALIDATE_MIN_DARK
    }

    fn adopt(&mut self, result: &GeometryResult) {
        let mut new_h = result.homography.expect("adopt requires a homography");
        if let Some(prev) = &self.homography {
            let shift = corner_shift(prev, &new_h);
            if let Some(shift) = shift
                && shift < SMALL_CHANGE_SHIFT_PX
            {
                let a = self.cfg.smooth_alpha;
                for (n, p) in new_h.iter_mut().zip(prev.iter()) {
                    *n = a * *n + (1.0 - a) * p;
                }
            }
        }
        self.homography = Some(new_h);
        if result.undistort.is_some() {
            self.undistort = result.undistort.clone();
        }
        self.rectifier = Rectifier::new(new_h, self.undistort.clone());
        self.confidence = result.confidence;
    }

    fn status(&self) -> LockStatus {
        let usable = matches!(self.state, LockState::Locked | LockState::Drift);
        LockStatus {
            state: self.state,
            usable,
            geometry_confidence: if usable { self.confidence } else { 0.0 },
        }
    }
}

/// Mean source-px movement of the canonical corners between geometries.
fn corner_shift(prev: &Mat3, new: &Mat3) -> Option<f64> {
    let prev_inv = mat3_inv(prev)?;
    let new_inv = mat3_inv(new)?;
    let corners = [(0.0, 0.0), (256.0, 0.0), (256.0, 240.0), (0.0, 240.0)];
    let mut total = 0.0;
    for (x, y) in corners {
        let (px, py) = project(&prev_inv, x, y);
        let (nx, ny) = project(&new_inv, x, y);
        total += (px - nx).hypot(py - ny);
    }
    Some(total / 4.0)
}

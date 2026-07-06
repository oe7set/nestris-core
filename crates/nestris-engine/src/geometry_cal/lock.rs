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
use nestris_vision::homography::{Mat3, mat3_inv, mat3_mul, project};
use nestris_vision::undistort::UndistortMap;

use crate::config::{CalibrationConfig, TrackingConfig};
use crate::geometry_cal::calibration::{
    GeometryResult, Rectifier, SolveOptions, estimate_geometry_with,
};
use crate::geometry_cal::tracker::LocalTracker;
use crate::layout::{LayoutTable, get_layout};

const REVALIDATE_MIN_DARK: f64 = 0.12;
const SMALL_CHANGE_SHIFT_PX: f64 = 24.0;
/// EMA factor for the tracker's sustained-motion estimate.
const MOTION_EMA_ALPHA: f64 = 0.3;
/// Blend rate of confidence toward the observed label score under motion.
const CONFIDENCE_BLEND: f64 = 0.05;

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
    /// Continuous micro-tracker for unstable sources (None = disabled).
    tracker: Option<LocalTracker>,
    tracking_cfg: TrackingConfig,
    /// EMA of the tracker's measured per-frame motion (canonical px).
    motion_ema: f64,
    /// Consecutive frames the tracker failed to find enough labels.
    miss_streak: u32,
}

impl CalibrationLock {
    pub fn new(cfg: CalibrationConfig) -> Self {
        Self::new_with_tracking(cfg, TrackingConfig::default())
    }

    pub fn new_with_tracking(cfg: CalibrationConfig, tracking: TrackingConfig) -> Self {
        let background = cfg.background_recalibration;
        let layout = get_layout();
        let tracker = tracking
            .enabled
            .then(|| LocalTracker::new(tracking.clone(), layout));
        Self {
            cfg,
            layout,
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
            tracker,
            tracking_cfg: tracking,
            motion_ema: 0.0,
            miss_streak: 0,
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

    /// The [`SolveOptions`] hosts should pass to off-thread solves so they
    /// match the lock's own inline solves.
    pub fn solve_options(&self) -> SolveOptions {
        SolveOptions {
            downscale_width: self.cfg.acquire_downscale_width,
        }
    }

    /// Whether an off-thread *acquisition* solve should attempt radial
    /// distortion estimation — mirrors the inline once-only guard so
    /// back-to-back acquisition solves don't repeat the expensive probe.
    pub fn try_undistort_hint(&self) -> bool {
        self.try_undistort()
    }

    /// Hand a host-computed background solve to the lock; adopted next frame
    /// under the never-regress rule. A failed solve never replaces a pending
    /// ok one (it still must arrive so background acquisition can reset its
    /// streak on scene changes).
    pub fn offer_solution(&mut self, result: GeometryResult) {
        if result.ok() || self.offered.as_ref().is_none_or(|o| !o.ok()) {
            self.offered = Some(result);
        }
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
        self.motion_ema = 0.0;
        self.miss_streak = 0;
        if let Some(tracker) = &mut self.tracker {
            tracker.reset_motion();
        }
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
            if self.background && self.cfg.background_acquisition {
                return self.acquire_background();
            }
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
            if !hold_drift && let Some(gray) = canon_gray {
                self.track_micro_motion(gray, hold_drift);
            }
        } else {
            let score = self.pending_score.unwrap_or(self.confidence);
            self.on_weak(score, hold_drift);
        }
        self.pending_score = None;
        self.status()
    }

    /// Run the continuous micro-tracker (when enabled) on the rectified luma
    /// and fold its correction into the geometry for the next frame.
    fn track_micro_motion(&mut self, canon_gray: &Image, hold_drift: bool) {
        let Some(tracker) = &mut self.tracker else {
            return;
        };
        let outcome = tracker.step(canon_gray);
        if outcome.matched < 2 {
            // Labels unreadable at their expected spots: either the camera
            // jumped beyond the search radius or content changed. Escalate
            // to the drift path (fast background solves) after a few misses.
            self.miss_streak += 1;
            if self.miss_streak >= self.tracking_cfg.miss_escalate {
                self.on_weak(self.confidence, hold_drift);
            }
            return;
        }
        self.miss_streak = 0;
        self.motion_ema =
            MOTION_EMA_ALPHA * outcome.mean_shift + (1.0 - MOTION_EMA_ALPHA) * self.motion_ema;

        if let Some(correction) = outcome.correction
            && let Some(h) = self.homography
        {
            let new_h = mat3_mul(&correction, &h);
            if let Some(rectifier) = Rectifier::new(new_h, self.undistort.clone()) {
                self.homography = Some(new_h);
                self.rectifier = Some(rectifier);
            }
        }

        // Under sustained motion the original solve's confidence goes stale;
        // drift it toward the live label score so `should_adopt` stays honest
        // and fresher background solves can win.
        if self.motion_ema > self.tracking_cfg.motion_adopt_threshold_px {
            self.confidence += (outcome.mean_score - self.confidence) * CONFIDENCE_BLEND;
        }
    }

    /// Whether the host should pace background solves at the fast (drift)
    /// interval: the tracker reports misses or sustained motion, or the lock
    /// is already drifting. `None` = normal pacing.
    pub fn solve_interval_hint(&self) -> Option<f64> {
        // While acquiring off-thread there is no OCR to compete with and the
        // snapshot slot is newest-wins: solve back-to-back for minimal
        // time-to-lock.
        if self.cfg.background_acquisition
            && matches!(
                self.state,
                LockState::Unlocked | LockState::Acquiring | LockState::Lost
            )
        {
            return Some(0.0);
        }
        self.tracker.as_ref()?;
        let urgent = self.state == LockState::Drift
            || self.miss_streak > 0
            || self.motion_ema > self.tracking_cfg.motion_adopt_threshold_px;
        urgent.then_some(self.tracking_cfg.drift_solve_interval_s)
    }

    /// Acquisition via the host's background solver: the pipeline thread
    /// keeps flowing frames while the host runs the expensive solve
    /// off-thread and hands results back through [`Self::offer_solution`].
    /// Adoption semantics mirror the inline [`Self::acquire`] exactly, with
    /// "consecutive frames" replaced by "consecutive solve results".
    fn acquire_background(&mut self) -> LockStatus {
        self.state = LockState::Acquiring;
        self.wants_solve = true;
        if let Some(result) = self.offered.take() {
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
        }
        self.status()
    }

    fn acquire(&mut self, image: &Image) -> LockStatus {
        self.state = LockState::Acquiring;
        let result = estimate_geometry_with(
            image,
            self.layout,
            self.undistort.clone(),
            self.try_undistort(),
            self.frame_index,
            &self.solve_options(),
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
            let result = estimate_geometry_with(
                image,
                self.layout,
                self.undistort.clone(),
                self.try_undistort(),
                self.frame_index,
                &self.solve_options(),
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
        // Under sustained tracked motion the never-regress margin (designed
        // for static capture-card sources) yields to fresher solves.
        let margin = if self.tracker.is_some()
            && self.motion_ema > self.tracking_cfg.motion_adopt_threshold_px
        {
            0.0
        } else {
            self.cfg.adopt_margin
        };
        score >= self.cfg.drift_threshold && score > self.confidence + margin
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
            } else if let Some(tracker) = &mut self.tracker {
                // Geometry replaced outright: the tracker's motion history
                // refers to the old frame of reference.
                tracker.reset_motion();
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

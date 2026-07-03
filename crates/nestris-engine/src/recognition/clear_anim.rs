//! Line-clear animation detection (port of `ClearAnimationDetector` in
//! `recognition/playfield.py`).

use crate::layout::PLAYFIELD_COLS;
use crate::recognition::playfield::Grid;

const FLASH_RATIO: f64 = 1.5;
const FLASH_MIN_BASE: f64 = 20.0;
const LUMA_EMA_ALPHA: f64 = 0.1;
const ANIM_STABLE_FRAMES: u32 = 2;
const ANIM_MAX_FRAMES: u32 = 20;

/// Detects the NES line-clear animation so playfield reads can be frozen.
pub struct ClearAnimationDetector {
    prev_occupancy: Option<Grid<bool>>,
    luma_ema: Option<f64>,
    animating: bool,
    anim_frames: u32,
    stable_frames: u32,
    flash: bool,
}

impl Default for ClearAnimationDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl ClearAnimationDetector {
    pub fn new() -> Self {
        Self {
            prev_occupancy: None,
            luma_ema: None,
            animating: false,
            anim_frames: 0,
            stable_frames: 0,
            flash: false,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn animating(&self) -> bool {
        self.animating
    }

    /// Whether this frame's luma is strobing (the tetris flash).
    pub fn flash_active(&self) -> bool {
        self.animating && self.flash
    }

    /// Fold one frame in; returns `true` while the animation runs.
    pub fn update(&mut self, occupancy: &Grid<bool>, frame_luma_mean: f64) -> bool {
        self.flash = match self.luma_ema {
            Some(ema) => ema > FLASH_MIN_BASE && frame_luma_mean > ema * FLASH_RATIO,
            None => false,
        };
        if self.animating {
            self.anim_frames += 1;
            if self.prev_occupancy.as_ref() == Some(occupancy) {
                self.stable_frames += 1;
            } else {
                self.stable_frames = 0;
            }
            if self.stable_frames >= ANIM_STABLE_FRAMES || self.anim_frames >= ANIM_MAX_FRAMES {
                self.animating = false;
            }
        } else {
            let mut retracting = false;
            if let Some(prev) = &self.prev_occupancy {
                for (prow, nrow) in prev.iter().zip(occupancy.iter()) {
                    let was_full = prow.iter().filter(|&&v| v).count() == PLAYFIELD_COLS;
                    let now_partial = nrow.iter().filter(|&&v| v).count() < PLAYFIELD_COLS;
                    if was_full && now_partial {
                        retracting = true;
                        break;
                    }
                }
            }
            if self.flash || retracting {
                self.animating = true;
                self.anim_frames = 0;
                self.stable_frames = 0;
            }
        }
        if !self.animating {
            self.luma_ema = Some(match self.luma_ema {
                None => frame_luma_mean,
                Some(ema) => (1.0 - LUMA_EMA_ALPHA) * ema + LUMA_EMA_ALPHA * frame_luma_mean,
            });
        }
        self.prev_occupancy = Some(*occupancy);
        self.animating
    }
}

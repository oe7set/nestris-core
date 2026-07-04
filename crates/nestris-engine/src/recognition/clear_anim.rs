//! Line-clear animation detection (port of `ClearAnimationDetector` in
//! `recognition/playfield.py`), extended with post-clear board prediction
//! and game-over curtain detection.
//!
//! The extensions are passive unless the processor opts in
//! (`recognition.clear_prediction`): with the flag off, `update` behaves
//! exactly like the verified oracle port.

use crate::layout::{PLAYFIELD_COLS, PLAYFIELD_ROWS};
use crate::recognition::playfield::Grid;

const FLASH_RATIO: f64 = 1.5;
const FLASH_MIN_BASE: f64 = 20.0;
const LUMA_EMA_ALPHA: f64 = 0.1;
const ANIM_STABLE_FRAMES: u32 = 2;
const ANIM_MAX_FRAMES: u32 = 20;
/// Consecutive frames of top-down fill growth before the curtain is called.
const CURTAIN_STREAK: u32 = 2;

/// Board prediction for a finished clear animation.
#[derive(Clone, Debug)]
pub struct ClearPrediction {
    /// Expected occupancy after the full rows collapse.
    pub occupancy: Grid<bool>,
    /// How many rows were full when the animation started.
    pub cleared_rows: usize,
}

/// Detects the NES line-clear animation so playfield reads can be frozen.
pub struct ClearAnimationDetector {
    prev_occupancy: Option<Grid<bool>>,
    luma_ema: Option<f64>,
    animating: bool,
    anim_frames: u32,
    stable_frames: u32,
    flash: bool,
    /// Prediction captured when an animation starts, handed out at its end.
    prediction: Option<ClearPrediction>,
    finished_prediction: Option<ClearPrediction>,
    /// Game-over curtain tracking (top-down full-row fill).
    curtain_prefix: usize,
    curtain_streak: u32,
    curtain: bool,
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
            prediction: None,
            finished_prediction: None,
            curtain_prefix: 0,
            curtain_streak: 0,
            curtain: false,
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

    /// The game-over curtain is sweeping down (rows filling top-to-bottom).
    pub fn curtain_active(&self) -> bool {
        self.curtain
    }

    /// The prediction of an animation that ended this frame, if any.
    pub fn take_finished_prediction(&mut self) -> Option<ClearPrediction> {
        self.finished_prediction.take()
    }

    /// Fold one frame in; returns `true` while the animation runs.
    /// `suppress_entry` blocks *starting* a new animation (used while
    /// paused or during the game-over curtain); a running animation still
    /// completes normally.
    pub fn update(
        &mut self,
        occupancy: &Grid<bool>,
        frame_luma_mean: f64,
        suppress_entry: bool,
    ) -> bool {
        self.track_curtain(occupancy);
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
                self.finished_prediction = self.prediction.take();
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
            if (self.flash || retracting) && !suppress_entry {
                self.animating = true;
                self.anim_frames = 0;
                self.stable_frames = 0;
                self.prediction = self.prev_occupancy.as_ref().and_then(predict_post_clear);
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

    /// Track the game-over curtain: the number of fully-occupied rows from
    /// the top grows monotonically over consecutive frames. (Line clears
    /// never fill top rows, so this pattern is unique to the curtain.)
    fn track_curtain(&mut self, occupancy: &Grid<bool>) {
        let prefix = occupancy
            .iter()
            .take_while(|row| row.iter().all(|&v| v))
            .count();
        if prefix > self.curtain_prefix && prefix >= 2 {
            self.curtain_streak += 1;
        } else if prefix < self.curtain_prefix {
            // Curtain lifted (new game) or it never was one.
            self.curtain_streak = 0;
            self.curtain = false;
        }
        if self.curtain_streak >= CURTAIN_STREAK {
            self.curtain = true;
        }
        self.curtain_prefix = prefix;
    }
}

/// Delete the full rows of `board` and shift everything above them down.
/// `None` when no row is full (a flash-only animation predicts nothing).
fn predict_post_clear(board: &Grid<bool>) -> Option<ClearPrediction> {
    let full: Vec<usize> = (0..PLAYFIELD_ROWS)
        .filter(|&r| board[r].iter().all(|&v| v))
        .collect();
    if full.is_empty() {
        return None;
    }
    let mut out: Grid<bool> = [[false; PLAYFIELD_COLS]; PLAYFIELD_ROWS];
    let mut dst = PLAYFIELD_ROWS;
    for src in (0..PLAYFIELD_ROWS).rev() {
        if full.contains(&src) {
            continue;
        }
        dst -= 1;
        out[dst] = board[src];
    }
    Some(ClearPrediction {
        occupancy: out,
        cleared_rows: full.len(),
    })
}

/// Cells that differ between a prediction and an observed board.
pub fn prediction_mismatches(prediction: &Grid<bool>, observed: &Grid<bool>) -> usize {
    prediction
        .iter()
        .zip(observed.iter())
        .flat_map(|(p, o)| p.iter().zip(o.iter()))
        .filter(|(p, o)| p != o)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty() -> Grid<bool> {
        [[false; PLAYFIELD_COLS]; PLAYFIELD_ROWS]
    }

    fn with_full_rows(rows: &[usize], extra: &[(usize, usize)]) -> Grid<bool> {
        let mut g = empty();
        for &r in rows {
            g[r] = [true; PLAYFIELD_COLS];
        }
        for &(r, c) in extra {
            g[r][c] = true;
        }
        g
    }

    #[test]
    fn predicts_single_clear_shift() {
        // Row 19 full; a lone cell at (18, 0) should land on row 19.
        let board = with_full_rows(&[19], &[(18, 0)]);
        let p = predict_post_clear(&board).unwrap();
        assert_eq!(p.cleared_rows, 1);
        assert!(p.occupancy[19][0]);
        assert!(!p.occupancy[18][0]);
    }

    #[test]
    fn predicts_tetris_shift() {
        // Rows 16-19 full, stack cell at (15, 3) drops four rows.
        let board = with_full_rows(&[16, 17, 18, 19], &[(15, 3)]);
        let p = predict_post_clear(&board).unwrap();
        assert_eq!(p.cleared_rows, 4);
        assert!(p.occupancy[19][3]);
        assert_eq!(
            p.occupancy.iter().flatten().filter(|&&v| v).count(),
            1,
            "only the shifted cell remains"
        );
    }

    #[test]
    fn retraction_starts_animation_and_yields_prediction() {
        let mut det = ClearAnimationDetector::new();
        let full = with_full_rows(&[19], &[(18, 0)]);
        det.update(&full, 40.0, false);
        // Middle-out retraction: row 19 loses its center columns.
        let mut retract = full;
        retract[19][4] = false;
        retract[19][5] = false;
        assert!(det.update(&retract, 40.0, false), "animation starts");
        // Post-clear board arrives and stays stable.
        let done = with_full_rows(&[], &[(19, 0)]);
        det.update(&done, 40.0, false);
        det.update(&done, 40.0, false);
        assert!(det.update(&done, 40.0, false) == false || !det.animating());
        let p = det
            .take_finished_prediction()
            .expect("prediction handed out");
        assert_eq!(p.cleared_rows, 1);
        assert_eq!(prediction_mismatches(&p.occupancy, &done), 0);
    }

    #[test]
    fn suppress_entry_blocks_new_animation() {
        let mut det = ClearAnimationDetector::new();
        let full = with_full_rows(&[19], &[]);
        det.update(&full, 40.0, false);
        let mut retract = full;
        retract[19][4] = false;
        assert!(!det.update(&retract, 40.0, true), "entry suppressed");
        assert!(!det.animating());
    }

    #[test]
    fn curtain_detected_on_top_down_fill() {
        let mut det = ClearAnimationDetector::new();
        det.update(&empty(), 40.0, false);
        assert!(!det.curtain_active());
        det.update(&with_full_rows(&[0, 1], &[]), 40.0, false);
        det.update(&with_full_rows(&[0, 1, 2, 3], &[]), 40.0, false);
        det.update(&with_full_rows(&[0, 1, 2, 3, 4, 5], &[]), 40.0, false);
        assert!(
            det.curtain_active(),
            "monotone top-down fill is the curtain"
        );
        // A new game clears the board: curtain lifts.
        det.update(&empty(), 40.0, false);
        assert!(!det.curtain_active());
    }

    #[test]
    fn normal_clears_never_look_like_the_curtain() {
        let mut det = ClearAnimationDetector::new();
        det.update(&with_full_rows(&[19], &[(18, 2)]), 40.0, false);
        det.update(&with_full_rows(&[18, 19], &[(17, 2)]), 40.0, false);
        det.update(&with_full_rows(&[17, 18, 19], &[]), 40.0, false);
        assert!(!det.curtain_active(), "bottom stacking is not a curtain");
    }

    #[test]
    fn flash_only_animation_predicts_nothing() {
        let mut det = ClearAnimationDetector::new();
        let board = with_full_rows(&[], &[(19, 0)]);
        for _ in 0..12 {
            det.update(&board, 30.0, false);
        }
        // Sudden luma spike without any full row: flash-triggered animation.
        assert!(det.update(&board, 90.0, false));
        det.update(&board, 30.0, false);
        det.update(&board, 30.0, false);
        det.update(&board, 30.0, false);
        assert!(det.take_finished_prediction().is_none());
    }
}

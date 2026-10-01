//! Screen / game-state classification.
//!
//! Two modes (`screen.mode`):
//! - `signature` (default): menus, the blanked pause and the ending are
//!   recognized by their NES-tile layout ([`super::screen_sig`]); gameplay
//!   frames then go through the HUD/playfield checks below (curtain, normal
//!   pause). Without a geometry lock the last good geometry, else an
//!   estimated frame box, aligns the raw frame.
//! - `legacy`: exact port of the Python `state/screen_classifier.py`
//!   (pixel-fraction heuristics), kept for oracle parity.

use std::collections::BTreeMap;

use nestris_vision::{Image, color, ncc, resize};

use super::screen_sig::{ScreenKind, ScreenMatch, SignatureMatcher, TileGrid, content_box};
use crate::config::ScreenConfig;
use crate::enums::GameState;
use crate::layout::{LayoutTable, get_layout};
use crate::palette::to_luma;
use crate::templates::label_templates;

const NO_SIGNAL_LUMA: f64 = 6.0;
const HUD_INK_LUMA: f64 = 12.0;
const HUD_LABEL_SCORE: f64 = 0.5;
const HUD_LABEL_PRESENT: f64 = 0.55;
const HUD_LABEL_SEARCH_PAD: usize = 3;
const CURTAIN_FILL: f64 = 0.75;
const WELL_DARK_MIN: f64 = 0.12;

const PAUSE_FIELD_MAX: f64 = 0.15;
const PAUSE_TEXT_BAND_MIN: f64 = 0.01;
const PAUSE_TEXT_BAND_MAX: f64 = 0.45;
const PAUSE_MIN_RECENT_FILL: f64 = 0.15;
const FILL_EMA_ALPHA: f64 = 0.05;

const BLANK_PAUSE_MAX: f64 = 24.0;
const PAUSE_TEXT_LUMA: u8 = 50;
const PAUSE_TEXT_MIN_FRAC: f64 = 0.004;
const PAUSE_TEXT_MAX_FRAC: f64 = 0.15;

const MENU_DARK_MEAN: f64 = 28.0;
const LOGO_FRACTION: f64 = 0.18;
const CHROME_FRACTION: f64 = 0.32;
const LOWER_DARK_FRACTION: f64 = 0.5;
const MENU_SIGNAL_W: usize = 160;
const MENU_SIGNAL_H: usize = 120;
const MENU_CONFIRM_FRAMES: u32 = 10;
/// Signature mode: minimum gameplay-layout correlation for the HUD checks to
/// run on a frame no signature matched confidently.
const GAMEPLAY_LAYOUT_MIN: f64 = 0.6;
/// Signature mode: the hidden-playfield pause leaves the well empty outside
/// the text band.
const PAUSE_OUTSIDE_BAND_MAX: f64 = 0.01;
/// Signature mode: game-over curtain = this many full rows from the top.
const CURTAIN_MIN_ROWS: usize = 3;
const CURTAIN_CELL_LUMA: f64 = 40.0;
const CURTAIN_CELL_STD: f64 = 25.0;

fn is_pause_state(state: Option<GameState>) -> bool {
    matches!(state, Some(GameState::InGame) | Some(GameState::Paused))
}

fn is_curtain_prev(state: Option<GameState>) -> bool {
    matches!(
        state,
        Some(GameState::InGame) | Some(GameState::Paused) | Some(GameState::GameOver)
    )
}

/// A classification outcome with confidence and supporting signals.
#[derive(Clone, Debug)]
pub struct ClassificationResult {
    pub state: GameState,
    pub confidence: f64,
    pub signals: BTreeMap<&'static str, f64>,
}

fn structural_well(signals: &BTreeMap<&'static str, f64>) -> bool {
    signals.get("well_dark").copied().unwrap_or(0.0) >= 0.5
        && signals.get("pf_fill").copied().unwrap_or(1.0) < 0.5
        && signals.get("bottom_fill").copied().unwrap_or(0.0)
            >= signals.get("top_fill").copied().unwrap_or(1.0)
}

/// Classifies the screen state from raw + (optionally) canonical frames.
pub struct ScreenClassifier {
    layout: &'static LayoutTable,
    cfg: ScreenConfig,
    menu_streak: u32,
    fill_ema: f64,
    /// No-geometry path: the detected content box and the NES frame box
    /// refined from it by maximizing the signature score.
    raw_box: Option<FrameBox>,
    frame_box: Option<FrameBox>,
}

impl Default for ScreenClassifier {
    fn default() -> Self {
        Self::new()
    }
}

impl ScreenClassifier {
    pub fn new() -> Self {
        Self::with_config(ScreenConfig::default())
    }

    pub fn with_config(cfg: ScreenConfig) -> Self {
        Self {
            layout: get_layout(),
            cfg,
            menu_streak: 0,
            fill_ema: 0.0,
            raw_box: None,
            frame_box: None,
        }
    }

    pub fn reset(&mut self) {
        self.menu_streak = 0;
        self.fill_ema = 0.0;
    }

    /// Classify one frame from raw + (when locked) canonical evidence.
    /// `aligned_gray` is the canonical luma rectified with the last good
    /// geometry while the lock is not usable (signature mode only).
    pub fn classify_frame(
        &mut self,
        source_bgr: &Image,
        canon: Option<&Image>,
        prev_state: Option<GameState>,
        canon_gray: Option<&Image>,
        aligned_gray: Option<&Image>,
    ) -> ClassificationResult {
        if self.cfg.legacy() {
            return self.classify_legacy(source_bgr, canon, prev_state, canon_gray);
        }
        self.classify_signature(source_bgr, canon, prev_state, canon_gray, aligned_gray)
    }

    fn confident(&self, m: Option<ScreenMatch>) -> Option<ScreenMatch> {
        m.filter(|m| {
            m.score as f64 >= self.cfg.match_threshold && m.margin as f64 >= self.cfg.match_margin
        })
    }

    fn classify_signature(
        &mut self,
        source_bgr: &Image,
        canon: Option<&Image>,
        prev_state: Option<GameState>,
        canon_gray: Option<&Image>,
        aligned_gray: Option<&Image>,
    ) -> ClassificationResult {
        let matcher = SignatureMatcher::builtin();
        let mut signals = BTreeMap::new();

        let gray_owned;
        let locked_gray = match (canon_gray, canon) {
            (Some(g), _) => Some(g),
            (None, Some(c)) => {
                gray_owned = to_luma(c);
                Some(&gray_owned)
            }
            _ => None,
        };

        // 1. Aligned evidence: the locked canonical frame, else the frame
        //    rectified with the last good geometry.
        if let Some(gray) = locked_gray.or(aligned_gray) {
            let locked = locked_gray.is_some();
            let grid = TileGrid::from_gray(gray);
            let best = matcher.best(&grid);
            signals.insert("canon_mean", grid.mean() as f64);
            if let Some(b) = best {
                signals.insert("sig_score", b.score as f64);
                signals.insert("sig_margin", b.margin as f64);
            }
            if let Some(m) = self.confident(best) {
                return match m.kind {
                    ScreenKind::InGame => match canon {
                        // HUD visible: curtain / normal pause / in-game.
                        Some(canon) => {
                            self.menu_streak = 0;
                            self.classify_gameplay(
                                canon,
                                prev_state,
                                &mut signals,
                                canon_gray,
                                true,
                            )
                            .unwrap_or(ClassificationResult {
                                state: GameState::InGame,
                                confidence: 0.6,
                                signals,
                            })
                        }
                        // Gameplay without a usable lock: nothing to read
                        // until the lock re-acquires.
                        None => self.unknown(signals),
                    },
                    ScreenKind::Pause => {
                        self.menu_streak = 0;
                        let confidence = if is_pause_state(prev_state) { 0.9 } else { 0.6 };
                        ClassificationResult {
                            state: GameState::Paused,
                            confidence,
                            signals,
                        }
                    }
                    ScreenKind::Boot => self.unknown(signals),
                    kind => {
                        let confidence = if locked { 0.9 } else { 0.8 };
                        self.menu_result(kind.state(), confidence, prev_state, signals)
                    }
                };
            }
            // Weak evidence: only a frame that still resembles the gameplay
            // layout may take the HUD checks (a transition, heavy noise);
            // anything else means the geometry is stale for this screen and
            // the frame box below decides.
            let ingame = matcher.score_of(&grid, ScreenKind::InGame).unwrap_or(0.0) as f64;
            signals.insert("sig_ingame", ingame);
            if let Some(canon) = canon
                && ingame >= GAMEPLAY_LAYOUT_MIN
            {
                if let Some(gameplay) =
                    self.classify_gameplay(canon, prev_state, &mut signals, canon_gray, false)
                    && gameplay.state != GameState::NoSignal
                {
                    self.menu_streak = 0;
                    return gameplay;
                }
                return self.unknown(signals);
            }
        }

        // 2. No geometry: estimated frame box, pooled grid. A black or
        //    uniform frame (console off/resetting, capture idle) has no
        //    picture.
        let (mean, std) = raw_stats(source_bgr);
        signals.insert("overall_mean", mean);
        signals.insert("overall_std", std);
        if mean < NO_SIGNAL_LUMA || std < FLAT_FRAME_STD {
            self.menu_streak = 0;
            return ClassificationResult {
                state: GameState::NoSignal,
                confidence: 1.0,
                signals,
            };
        }
        if let Some(b) = content_box(source_bgr) {
            let moved = self
                .raw_box
                .is_none_or(|r| box_distance(r, b) > BOX_MOVE_PX);
            if moved && let Some(refined) = refine_box(matcher, source_bgr, b) {
                self.raw_box = Some(b);
                self.frame_box = Some(refined);
            }
        }
        let Some((x, y, w, h)) = self.frame_box else {
            return self.unknown(signals);
        };
        let grid = TileGrid::from_bgr_region(source_bgr, x, y, w, h);
        let best = matcher.best_pooled(&grid);
        if let Some(b) = best {
            signals.insert("sig_score", b.score as f64);
            signals.insert("sig_margin", b.margin as f64);
        }
        // Without geometry only the menus count: the ending and gameplay
        // always follow a locked game, and a near-flat frame (console reset
        // gray, fade) must not pass for a menu.
        let contrast = grid_std(&grid.luma);
        signals.insert("sig_contrast", contrast);
        match self.confident(best).map(|m| m.kind) {
            Some(
                kind @ (ScreenKind::Title
                | ScreenKind::TypeSelect
                | ScreenKind::LevelSelect
                | ScreenKind::HighscoreEntry),
            ) if contrast >= MENU_MIN_CONTRAST => {
                self.menu_result(kind.state(), 0.6, prev_state, signals)
            }
            Some(ScreenKind::Pause) if is_pause_state(prev_state) => ClassificationResult {
                state: GameState::Paused,
                confidence: 0.6,
                signals,
            },
            _ => self.unknown(signals),
        }
    }

    fn unknown(&mut self, signals: BTreeMap<&'static str, f64>) -> ClassificationResult {
        self.menu_streak = 0;
        ClassificationResult {
            state: GameState::Unknown,
            confidence: 0.3,
            signals,
        }
    }

    /// A recognized menu screen; leaving a running game needs
    /// `menu_confirm_frames` consecutive matches.
    fn menu_result(
        &mut self,
        state: GameState,
        confidence: f64,
        prev_state: Option<GameState>,
        mut signals: BTreeMap<&'static str, f64>,
    ) -> ClassificationResult {
        if is_pause_state(prev_state) {
            self.menu_streak += 1;
            signals.insert("menu_streak", self.menu_streak as f64);
            if self.menu_streak < self.cfg.menu_confirm_frames {
                return ClassificationResult {
                    state: GameState::Unknown,
                    confidence: 0.3,
                    signals,
                };
            }
        } else {
            self.menu_streak = 0;
        }
        self.fill_ema = 0.0;
        ClassificationResult {
            state,
            confidence,
            signals,
        }
    }

    fn classify_legacy(
        &mut self,
        source_bgr: &Image,
        canon: Option<&Image>,
        prev_state: Option<GameState>,
        canon_gray: Option<&Image>,
    ) -> ClassificationResult {
        let mut signals = menu_signals(source_bgr);

        if signals["overall_mean"] < NO_SIGNAL_LUMA {
            self.menu_streak = 0;
            return ClassificationResult {
                state: GameState::NoSignal,
                confidence: 1.0,
                signals,
            };
        }

        if let Some(canon) = canon
            && let Some(gameplay) =
                self.classify_gameplay(canon, prev_state, &mut signals, canon_gray, false)
        {
            self.menu_streak = 0;
            return gameplay;
        }
        self.classify_menu(signals, canon.is_some(), prev_state)
    }

    /// HUD/playfield checks on the canonical frame. `layout_matched`: the
    /// gameplay layout signature already matched (signature mode), so the
    /// HUD counts as present and the blank-frame checks are skipped.
    fn classify_gameplay(
        &mut self,
        canon: &Image,
        prev_state: Option<GameState>,
        signals: &mut BTreeMap<&'static str, f64>,
        canon_gray: Option<&Image>,
        layout_matched: bool,
    ) -> Option<ClassificationResult> {
        let gray_owned;
        let gray = match canon_gray {
            Some(g) => g,
            None => {
                gray_owned = to_luma(canon);
                &gray_owned
            }
        };
        let canon_mean = mean_u8(&gray.data);
        signals.insert("canon_mean", canon_mean);

        if canon_mean < NO_SIGNAL_LUMA && !layout_matched {
            return Some(ClassificationResult {
                state: GameState::NoSignal,
                confidence: 1.0,
                signals: signals.clone(),
            });
        }

        if canon_mean < BLANK_PAUSE_MAX && is_pause_state(prev_state) && !layout_matched {
            let center_text = center_text_fraction(gray);
            signals.insert("center_text", center_text);
            if (PAUSE_TEXT_MIN_FRAC..=PAUSE_TEXT_MAX_FRAC).contains(&center_text) {
                signals.insert("blank_pause", 1.0);
                return Some(ClassificationResult {
                    state: GameState::Paused,
                    confidence: 0.8,
                    signals: signals.clone(),
                });
            }
        }

        let (hud, from_labels) = self.hud_presence(gray);
        let pf_fill = self.playfield_fill(gray);
        let well_dark = self.well_dark(gray);
        let (top_fill, bottom_fill) = self.fill_split(gray);
        let band_text = self.pause_band(gray);
        signals.insert("hud", hud);
        signals.insert("pf_fill", pf_fill);
        signals.insert("well_dark", well_dark);
        signals.insert("top_fill", top_fill);
        signals.insert("bottom_fill", bottom_fill);
        signals.insert("band_text", band_text);
        signals.insert("fill_ema", self.fill_ema);

        let signature = !self.cfg.legacy();
        // Signature mode: a hidden-playfield pause shows only the text band;
        // a low stack with a piece falling through the band does not count.
        let vanilla_pause = is_pause_state(prev_state)
            && pf_fill < PAUSE_FIELD_MAX
            && self.fill_ema > PAUSE_MIN_RECENT_FILL
            && (PAUSE_TEXT_BAND_MIN..=PAUSE_TEXT_BAND_MAX).contains(&band_text)
            && (!signature || self.outside_band_fill(gray) < PAUSE_OUTSIDE_BAND_MAX);
        // Signature mode: the curtain fills whole rows from the top; full
        // rows never survive in play (they clear), whatever the palette.
        let curtain_rows = if signature {
            self.full_rows_from_top(gray)
        } else {
            0
        };
        signals.insert("curtain_rows", curtain_rows as f64);

        signals.insert("hud_from_labels", if from_labels { 1.0 } else { 0.0 });
        let hud_present = if layout_matched {
            true
        } else if from_labels {
            hud >= HUD_LABEL_PRESENT
        } else {
            let structural = well_dark >= WELL_DARK_MIN
                && (bottom_fill >= top_fill || pf_fill < 0.05 || vanilla_pause);
            hud >= HUD_LABEL_SCORE && structural
        };

        if hud_present {
            if vanilla_pause {
                return Some(ClassificationResult {
                    state: GameState::Paused,
                    confidence: 0.8,
                    signals: signals.clone(),
                });
            }
            if (pf_fill > CURTAIN_FILL && top_fill > 0.5) || curtain_rows >= CURTAIN_MIN_ROWS {
                return Some(ClassificationResult {
                    state: GameState::GameOver,
                    confidence: 0.75,
                    signals: signals.clone(),
                });
            }
            self.update_fill_ema(pf_fill);
            return Some(ClassificationResult {
                state: GameState::InGame,
                confidence: 0.85,
                signals: signals.clone(),
            });
        }

        if !from_labels && structural_well(signals) && canon_mean > MENU_DARK_MEAN {
            if vanilla_pause {
                return Some(ClassificationResult {
                    state: GameState::Paused,
                    confidence: 0.7,
                    signals: signals.clone(),
                });
            }
            self.update_fill_ema(pf_fill);
            return Some(ClassificationResult {
                state: GameState::InGame,
                confidence: 0.6,
                signals: signals.clone(),
            });
        }

        let menu_like = signals.get("chrome").copied().unwrap_or(0.0) >= CHROME_FRACTION
            || signals.get("logo").copied().unwrap_or(0.0) > LOGO_FRACTION;
        if is_curtain_prev(prev_state) && !menu_like && pf_fill > CURTAIN_FILL && top_fill > 0.5 {
            return Some(ClassificationResult {
                state: GameState::GameOver,
                confidence: 0.7,
                signals: signals.clone(),
            });
        }

        None
    }

    fn classify_menu(
        &mut self,
        mut signals: BTreeMap<&'static str, f64>,
        locked: bool,
        prev_state: Option<GameState>,
    ) -> ClassificationResult {
        let candidate = menu_candidate(&signals, prev_state);
        let Some(candidate) = candidate else {
            self.menu_streak = 0;
            return ClassificationResult {
                state: GameState::Unknown,
                confidence: 0.3,
                signals,
            };
        };

        if locked && is_pause_state(prev_state) {
            self.menu_streak += 1;
            signals.insert("menu_streak", self.menu_streak as f64);
            if self.menu_streak < MENU_CONFIRM_FRAMES {
                return ClassificationResult {
                    state: GameState::Unknown,
                    confidence: 0.3,
                    signals,
                };
            }
        } else {
            self.menu_streak = 0;
        }
        self.fill_ema = 0.0;
        ClassificationResult {
            state: candidate.0,
            confidence: candidate.1,
            signals,
        }
    }

    fn update_fill_ema(&mut self, pf_fill: f64) {
        self.fill_ema = (1.0 - FILL_EMA_ALPHA) * self.fill_ema + FILL_EMA_ALPHA * pf_fill;
    }

    fn hud_presence(&self, gray: &Image) -> (f64, bool) {
        let labels = label_templates();
        let rects = [
            ("LINES", &self.layout.label_lines),
            ("SCORE", &self.layout.label_score),
            ("NEXT", &self.layout.label_next),
            ("LEVEL", &self.layout.label_level),
        ];
        let (gh, gw) = (gray.height, gray.width);
        let pad = HUD_LABEL_SEARCH_PAD;
        let mut scores: Vec<f64> = Vec::new();
        for (name, rect) in rects {
            let Some((_, tpl)) = labels.iter().find(|(l, _)| *l == name) else {
                continue;
            };
            let (x0r, y0r, x1r, y1r) = rect.to_bounds();
            if x1r <= x0r || y1r <= y0r {
                continue;
            }
            let y0 = y0r.saturating_sub(pad);
            let y1 = (y1r + pad).min(gh);
            let x0 = x0r.saturating_sub(pad);
            let x1 = (x1r + pad).min(gw);
            let region = gray.crop(x0, y0, x1 - x0, y1 - y0);
            let tpl_r = resize::resize_area(tpl, x1r - x0r, y1r - y0r);
            if region.height < tpl_r.height || region.width < tpl_r.width {
                continue;
            }
            let response = ncc::match_template_ccoeff_normed(&region, &tpl_r);
            let (max_val, _) = response.max();
            scores.push((max_val as f64 + 1.0) / 2.0);
        }
        if !scores.is_empty() {
            return (scores.iter().sum::<f64>() / scores.len() as f64, true);
        }
        // Fallback: SCORE region carries ink during gameplay.
        let (x0, y0, x1, y1) = self.layout.score.to_bounds();
        let (x1, y1) = (x1.min(gray.width), y1.min(gray.height));
        if x1 <= x0 || y1 <= y0 {
            return (0.0, false);
        }
        let patch = gray.crop(x0, y0, x1 - x0, y1 - y0);
        (
            (mean_u8(&patch.data) / (HUD_INK_LUMA * 4.0)).min(1.0),
            false,
        )
    }

    fn playfield_patch(&self, gray: &Image) -> Image {
        let (x0, y0, x1, y1) = self.layout.playfield.to_bounds();
        let (x1, y1) = (x1.min(gray.width), y1.min(gray.height));
        gray.crop(x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0))
    }

    fn playfield_fill(&self, gray: &Image) -> f64 {
        let patch = self.playfield_patch(gray);
        if patch.data.is_empty() {
            return 0.0;
        }
        fraction(&patch.data, |v| v > 64)
    }

    fn well_dark(&self, gray: &Image) -> f64 {
        let patch = self.playfield_patch(gray);
        if patch.data.is_empty() {
            return 0.0;
        }
        fraction(&patch.data, |v| v < 64)
    }

    fn fill_split(&self, gray: &Image) -> (f64, f64) {
        let patch = self.playfield_patch(gray);
        if patch.data.is_empty() {
            return (0.0, 0.0);
        }
        let half = patch.height / 2;
        let split = half * patch.width;
        (
            fraction(&patch.data[..split], |v| v > 64),
            fraction(&patch.data[split..], |v| v > 64),
        )
    }

    /// Bright fraction of the playfield outside the pause text band.
    fn outside_band_fill(&self, gray: &Image) -> f64 {
        let patch = self.playfield_patch(gray);
        if patch.data.is_empty() {
            return 0.0;
        }
        let (b0, b1) = (
            (patch.height as f64 * 0.4) as usize,
            (patch.height as f64 * 0.58) as usize,
        );
        let mut bright = 0usize;
        let mut total = 0usize;
        for y in (0..b0).chain(b1..patch.height) {
            let row = patch.row(y);
            bright += row.iter().filter(|&&v| v > 64).count();
            total += row.len();
        }
        if total == 0 {
            0.0
        } else {
            bright as f64 / total as f64
        }
    }

    /// Consecutive completely filled playfield rows counted from the top:
    /// every cell brighter than the empty well and striped like the curtain
    /// (the uniform white of the tetris flash does not count).
    fn full_rows_from_top(&self, gray: &Image) -> usize {
        let patch = self.playfield_patch(gray);
        if patch.width < 10 || patch.height < 20 {
            return 0;
        }
        let (cw, ch) = (patch.width / 10, patch.height / 20);
        let n = (cw * ch) as f64;
        let mut rows = 0;
        for r in 0..20 {
            let full = (0..10).all(|c| {
                let (mut sum, mut sumsq) = (0f64, 0f64);
                for y in r * ch..(r + 1) * ch {
                    for &v in &patch.row(y)[c * cw..(c + 1) * cw] {
                        sum += v as f64;
                        sumsq += (v as f64) * (v as f64);
                    }
                }
                let mean = sum / n;
                let std = (sumsq / n - mean * mean).max(0.0).sqrt();
                mean > CURTAIN_CELL_LUMA && std > CURTAIN_CELL_STD
            });
            if !full {
                break;
            }
            rows += 1;
        }
        rows
    }

    fn pause_band(&self, gray: &Image) -> f64 {
        let pf = &self.layout.playfield;
        let strip = crate::geometry::Rect::new(pf.x, pf.y + pf.h * 0.4, pf.w, pf.h * 0.18);
        let (x0, y0, x1, y1) = strip.to_bounds();
        let (x1, y1) = (x1.min(gray.width), y1.min(gray.height));
        if x1 <= x0 || y1 <= y0 {
            return 0.0;
        }
        let patch = gray.crop(x0, y0, x1 - x0, y1 - y0);
        fraction(&patch.data, |v| v > 96)
    }
}

type FrameBox = (f64, f64, f64, f64);

/// Content-box change (px, any edge) that triggers a new refinement.
const BOX_MOVE_PX: f64 = 6.0;
/// A refinement needs some screen to correlate at least this well.
const BOX_REFINE_MIN: f32 = 0.6;
/// Tile-luma standard deviation of a real menu screen (they are high-contrast).
const MENU_MIN_CONTRAST: f64 = 15.0;
/// Luma standard deviation below which a raw frame is uniform (no picture).
const FLAT_FRAME_STD: f64 = 3.0;

fn box_distance(a: FrameBox, b: FrameBox) -> f64 {
    [
        (a.0 - b.0).abs(),
        (a.1 - b.1).abs(),
        (a.0 + a.2 - b.0 - b.2).abs(),
        (a.1 + a.3 - b.1 - b.3).abs(),
    ]
    .into_iter()
    .fold(0.0, f64::max)
}

/// Fit the NES frame box to the content box: coordinate descent on the
/// edges maximizing the best pooled signature score. `None` when no screen
/// resembles a reference (black frame, foreign content).
fn refine_box(matcher: &SignatureMatcher, bgr: &Image, start: FrameBox) -> Option<FrameBox> {
    let score = |b: FrameBox| {
        let grid = TileGrid::from_bgr_region(bgr, b.0, b.1, b.2, b.3);
        matcher.best_pooled(&grid).map_or(f32::MIN, |m| m.score)
    };
    let mut best = start;
    let mut best_score = score(start);
    for step_frac in [0.02, 0.01, 0.005] {
        let (sx, sy) = (start.2 * step_frac, start.3 * step_frac);
        loop {
            let mut improved = false;
            let candidates = [
                (best.0 - sx, best.1, best.2 + sx, best.3),
                (best.0 + sx, best.1, best.2 - sx, best.3),
                (best.0, best.1, best.2 - sx, best.3),
                (best.0, best.1, best.2 + sx, best.3),
                (best.0, best.1 - sy, best.2, best.3 + sy),
                (best.0, best.1 + sy, best.2, best.3 - sy),
                (best.0, best.1, best.2, best.3 - sy),
                (best.0, best.1, best.2, best.3 + sy),
            ];
            for c in candidates {
                let sc = score(c);
                if sc > best_score + 1e-4 {
                    best = c;
                    best_score = sc;
                    improved = true;
                }
            }
            if !improved {
                break;
            }
        }
    }
    (best_score >= BOX_REFINE_MIN).then_some(best)
}

fn grid_std(luma: &[f32]) -> f64 {
    let n = luma.len().max(1) as f64;
    let mean = luma.iter().map(|&v| v as f64).sum::<f64>() / n;
    (luma.iter().map(|&v| (v as f64 - mean).powi(2)).sum::<f64>() / n).sqrt()
}

/// Mean and standard deviation of the luma of a strided raw-frame sample.
fn raw_stats(bgr: &Image) -> (f64, f64) {
    const STEP: usize = 6;
    let (mut sum, mut sumsq, mut n) = (0f64, 0f64, 0f64);
    for y in (0..bgr.height).step_by(STEP) {
        for x in (0..bgr.width).step_by(STEP) {
            let px = bgr.pixel(x, y);
            let v = nestris_vision::color::bgr_pixel_to_gray(px[0], px[1], px[2]) as f64;
            sum += v;
            sumsq += v * v;
            n += 1.0;
        }
    }
    if n == 0.0 {
        return (0.0, 0.0);
    }
    let mean = sum / n;
    (mean, (sumsq / n - mean * mean).max(0.0).sqrt())
}

fn mean_u8(data: &[u8]) -> f64 {
    if data.is_empty() {
        return 0.0;
    }
    data.iter().map(|&v| v as f64).sum::<f64>() / data.len() as f64
}

fn fraction(data: &[u8], pred: impl Fn(u8) -> bool) -> f64 {
    if data.is_empty() {
        return 0.0;
    }
    data.iter().filter(|&&v| pred(v)).count() as f64 / data.len() as f64
}

fn center_text_fraction(gray: &Image) -> f64 {
    let h = gray.height;
    let y0 = (h as f64 * 0.40) as usize;
    let y1 = (h as f64 * 0.65) as usize;
    if y1 <= y0 {
        return 0.0;
    }
    let band = &gray.data[y0 * gray.width..y1 * gray.width];
    fraction(band, |v| v > PAUSE_TEXT_LUMA)
}

/// Coarse menu signatures from a strided ~160×120 subsample of the raw frame.
fn menu_signals(source_bgr: &Image) -> BTreeMap<&'static str, f64> {
    let (h, w) = (source_bgr.height, source_bgr.width);
    let step_y = (h / MENU_SIGNAL_H).max(1);
    let step_x = (w / MENU_SIGNAL_W).max(1);
    let sh = h.div_ceil(step_y);
    let sw = w.div_ceil(step_x);
    let mut small = Image::new(sw, sh, 3);
    for (yy, y) in (0..h).step_by(step_y).enumerate() {
        for (xx, x) in (0..w).step_by(step_x).enumerate() {
            let src = source_bgr.pixel(x, y);
            small.pixel_mut(xx, yy).copy_from_slice(src);
        }
    }
    let gray = to_luma(&small);
    let overall_mean = mean_u8(&gray.data);
    let hsv = color::bgr_to_hsv(&small);

    // Maze chrome: gray, low-saturation, mid-value.
    let mut chrome_count = 0usize;
    for px in hsv.data.chunks_exact(3) {
        if px[1] < 50 && px[2] > 70 && px[2] < 170 {
            chrome_count += 1;
        }
    }
    let chrome = chrome_count as f64 / (sw * sh) as f64;

    // Bright saturated content in the upper-center => TETRIS logo.
    let (uy0, uy1) = ((sh as f64 * 0.15) as usize, (sh as f64 * 0.45) as usize);
    let (ux0, ux1) = ((sw as f64 * 0.2) as usize, (sw as f64 * 0.8) as usize);
    let mut logo_count = 0usize;
    let mut logo_total = 0usize;
    for y in uy0..uy1 {
        for x in ux0..ux1 {
            let px = hsv.pixel(x, y);
            logo_total += 1;
            if px[1] > 110 && px[2] > 110 {
                logo_count += 1;
            }
        }
    }
    let logo = if logo_total > 0 {
        logo_count as f64 / logo_total as f64
    } else {
        0.0
    };

    // Large dark panel in the lower half => level-select high-score table.
    let ly0 = (sh as f64 * 0.55) as usize;
    let (lx0, lx1) = ((sw as f64 * 0.2) as usize, (sw as f64 * 0.85) as usize);
    let mut dark_count = 0usize;
    let mut dark_total = 0usize;
    for y in ly0..sh {
        for x in lx0..lx1 {
            dark_total += 1;
            if gray.data[y * sw + x] < 40 {
                dark_count += 1;
            }
        }
    }
    let lower_dark = if dark_total > 0 {
        dark_count as f64 / dark_total as f64
    } else {
        0.0
    };

    let mut signals = BTreeMap::new();
    signals.insert("overall_mean", overall_mean);
    signals.insert("chrome", chrome);
    signals.insert("logo", logo);
    signals.insert("lower_dark", lower_dark);
    signals
}

fn menu_candidate(
    signals: &BTreeMap<&'static str, f64>,
    prev_state: Option<GameState>,
) -> Option<(GameState, f64)> {
    let overall_mean = signals["overall_mean"];
    let chrome = signals["chrome"];
    let logo = signals["logo"];
    let lower_dark = signals["lower_dark"];

    if logo > LOGO_FRACTION && chrome < CHROME_FRACTION {
        return Some((GameState::Title, 0.7));
    }
    if overall_mean < MENU_DARK_MEAN && chrome < CHROME_FRACTION {
        return Some((GameState::Title, 0.5));
    }
    if signals.get("hud_from_labels").copied().unwrap_or(0.0) == 0.0 && structural_well(signals) {
        return None;
    }
    if chrome >= CHROME_FRACTION {
        if matches!(
            prev_state,
            Some(GameState::GameOver) | Some(GameState::HighscoreEntry)
        ) {
            return Some((GameState::HighscoreEntry, 0.5));
        }
        return Some((GameState::TypeSelect, 0.6));
    }
    if lower_dark > LOWER_DARK_FRACTION {
        return Some((GameState::LevelSelect, 0.6));
    }
    None
}

//! Continuous per-frame geometry micro-tracking for unstable (handheld)
//! sources.
//!
//! While the lock is `Locked`, the only cheap per-frame signal is the
//! dark-fraction check — it detects catastrophic misalignment but cannot
//! *follow* motion, and full re-solves are paced far too slowly for a
//! shaking camera. This tracker closes that gap: every frame it re-matches
//! the four HUD labels (LINES/SCORE/NEXT/LEVEL) inside small windows of the
//! already-rectified canonical frame, adds playfield-edge constraints, fits
//! a damped similarity correction, and composes it onto the homography so
//! the *next* frame is rectified in the right place.
//!
//! Cost model: four small NCC searches (~0.3 ms) plus a 4x4 solve. When the
//! source is stable the fitted correction falls below the deadband and the
//! frame costs only the measurements — no rectifier rebuild, no state
//! change, byte-identical output. Everything here is deterministic (frame
//! content + config only).

use nestris_vision::homography::Mat3;
use nestris_vision::{Image, ncc, resize};

use crate::config::TrackingConfig;
use crate::geometry::Rect;
use crate::layout::LayoutTable;
use crate::templates::label_templates;

/// Interior luma threshold shared with the lock's dark-fraction check.
const DARK: u8 = 64;
/// Weight of a 1-D playfield-edge constraint relative to a label match.
const EDGE_WEIGHT: f64 = 0.5;
/// Half-width of the edge scan around the expected boundary, in px.
const EDGE_SCAN: i32 = 8;
/// EMA factor for the offset-velocity estimate (feed-forward term).
const VELOCITY_EMA: f64 = 0.5;
/// Canonical content center used to linearize correction bookkeeping.
const CENTER: (f64, f64) = (128.0, 120.0);

/// One tracker measurement/correction cycle result.
#[derive(Clone, Debug)]
pub struct TrackOutcome {
    /// Number of HUD labels matched this frame (0..=4).
    pub matched: usize,
    /// Mean observed label displacement in canonical px (0 when unmatched).
    pub mean_shift: f64,
    /// Mean NCC score of the matched labels.
    pub mean_score: f64,
    /// Damped canonical-space correction to compose onto the homography
    /// (`H_new = correction * H`). `None` when unmatched or inside the
    /// deadband.
    pub correction: Option<Mat3>,
}

struct LabelSite {
    /// Template resized to its canonical layout rect (grayscale).
    template: Image,
    /// Ideal center in canonical space.
    center: (f64, f64),
    /// Layout rect (canonical px).
    rect: Rect,
}

/// See the module docs. One instance lives inside the calibration lock.
///
/// The tracker is a small controller, not just a measurer: a purely
/// proportional (damped) correction lags continuous motion by roughly
/// `motion / damping` pixels, so a velocity feed-forward term — an EMA of
/// how the offset moved beyond what was corrected — pre-compensates the
/// next frame's expected motion. On still sources both terms are ~0 and
/// the deadband keeps the geometry untouched.
pub struct LocalTracker {
    cfg: TrackingConfig,
    sites: Vec<LabelSite>,
    playfield: Rect,
    /// Mean label offset measured last frame (before correction).
    last_offset: Option<(f64, f64)>,
    /// Translation effect (at [`CENTER`]) of the correction applied last frame.
    last_correction_vec: (f64, f64),
    /// EMA of the per-frame offset motion (the feed-forward estimate).
    velocity: (f64, f64),
}

impl LocalTracker {
    pub fn new(cfg: TrackingConfig, layout: &LayoutTable) -> Self {
        // The four playfield-flanking labels (STATISTICS excluded — its
        // column is covered by the edge constraints and the long template
        // makes the search disproportionately expensive).
        let wanted = [
            ("LINES", layout.label_lines),
            ("SCORE", layout.label_score),
            ("NEXT", layout.label_next),
            ("LEVEL", layout.label_level),
        ];
        let sites = wanted
            .iter()
            .filter_map(|(name, rect)| {
                let (_, tpl) = label_templates().iter().find(|(l, _)| l == name)?;
                let (x0, y0, x1, y1) = rect.to_bounds();
                let (w, h) = (x1 - x0, y1 - y0);
                if w < 4 || h < 4 {
                    return None;
                }
                Some(LabelSite {
                    template: resize::resize_area(tpl, w, h),
                    center: (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0),
                    rect: *rect,
                })
            })
            .collect();
        Self {
            cfg,
            sites,
            playfield: layout.playfield,
            last_offset: None,
            last_correction_vec: (0.0, 0.0),
            velocity: (0.0, 0.0),
        }
    }

    /// Clear the motion history (lock reset / geometry replaced outright).
    pub fn reset_motion(&mut self) {
        self.last_offset = None;
        self.last_correction_vec = (0.0, 0.0);
        self.velocity = (0.0, 0.0);
    }

    /// Measure label/edge displacements on the rectified luma and fit the
    /// damped + feed-forward correction. The caller (the lock) decides what
    /// to do with the outcome.
    pub fn step(&mut self, canon_gray: &Image) -> TrackOutcome {
        debug_assert_eq!(canon_gray.channels, 1);
        let radius = f64::from(self.cfg.search_radius_px);

        // 2-D constraints: observed -> ideal label centers.
        let mut points: Vec<(f64, (f64, f64), (f64, f64))> = Vec::with_capacity(4);
        let mut score_sum = 0.0;
        for site in &self.sites {
            let Some((ox, oy, score)) = self.match_label(canon_gray, site, radius) else {
                continue;
            };
            let (dx, dy) = (ox - site.center.0, oy - site.center.1);
            if dx.hypot(dy) > self.cfg.max_correction_px {
                continue; // a jump this large is a mismatch, not motion
            }
            score_sum += score;
            points.push((1.0, (ox, oy), site.center));
        }
        let matched = points.len();
        if matched < 2 {
            // The motion chain is broken; velocity would be stale.
            self.last_offset = None;
            self.last_correction_vec = (0.0, 0.0);
            return TrackOutcome {
                matched,
                mean_shift: 0.0,
                mean_score: 0.0,
                correction: None,
            };
        }
        let mean_score = score_sum / matched as f64;
        let mean_shift = points
            .iter()
            .map(|(_, o, i)| (o.0 - i.0).hypot(o.1 - i.1))
            .sum::<f64>()
            / matched as f64;

        // Offset-velocity estimate: how far the offset moved beyond what the
        // previous correction accounted for = the actual inter-frame motion.
        let offset = points.iter().fold((0.0, 0.0), |acc, (_, o, i)| {
            (acc.0 + (o.0 - i.0), acc.1 + (o.1 - i.1))
        });
        let offset = (offset.0 / matched as f64, offset.1 / matched as f64);
        if let Some(prev) = self.last_offset {
            let m = (
                offset.0 - (prev.0 + self.last_correction_vec.0),
                offset.1 - (prev.1 + self.last_correction_vec.1),
            );
            self.velocity = (
                VELOCITY_EMA * m.0 + (1.0 - VELOCITY_EMA) * self.velocity.0,
                VELOCITY_EMA * m.1 + (1.0 - VELOCITY_EMA) * self.velocity.1,
            );
        }
        self.last_offset = Some(offset);

        // 1-D horizontal constraints from the playfield's vertical edges.
        let mut edges: Vec<(f64, (f64, f64), f64)> = Vec::new(); // (w, observed(x,y), ideal_x)
        for frac in [0.25, 0.5, 0.75] {
            let y = self.playfield.y + self.playfield.h * frac;
            for (expected_x, from_left) in [(self.playfield.x, true), (self.playfield.x2(), false)]
            {
                if let Some(observed_x) = find_edge(canon_gray, y, expected_x, from_left)
                    && (observed_x - expected_x).abs() <= self.cfg.max_correction_px
                {
                    edges.push((EDGE_WEIGHT, (observed_x, y), expected_x));
                }
            }
        }

        let Some(sim) = fit_similarity(&points, &edges) else {
            self.last_correction_vec = (0.0, 0.0);
            return TrackOutcome {
                matched,
                mean_shift,
                mean_score,
                correction: None,
            };
        };

        // Damping toward identity keeps the measure->correct->measure loop
        // stable even with noisy matches.
        let l = self.cfg.damping;
        let damped = [
            1.0 + l * (sim[0] - 1.0),
            l * sim[1],
            l * sim[2],
            1.0 + l * (sim[3] - 1.0),
            l * sim[4],
        ];
        // damped = [a, b, tx, a2==a, ty] (see fit_similarity's param layout).
        let mut correction: Mat3 = [
            damped[0], -damped[1], damped[2], //
            damped[1], damped[0], damped[4], //
            0.0, 0.0, 1.0,
        ];

        // Deadband on the proportional part: stable sources exit here every
        // frame without touching the geometry; the velocity decays away.
        if mean_corner_displacement(&correction) < self.cfg.deadband_px {
            self.last_correction_vec = (0.0, 0.0);
            self.velocity = (self.velocity.0 * 0.5, self.velocity.1 * 0.5);
            return TrackOutcome {
                matched,
                mean_shift,
                mean_score,
                correction: None,
            };
        }

        // Velocity feed-forward: pre-compensate the motion expected before
        // the next measurement, clamped so a bad estimate cannot overshoot.
        let ff_max = self.cfg.max_correction_px / 2.0;
        let ff_mag = self.velocity.0.hypot(self.velocity.1);
        let ff = if ff_mag > ff_max {
            let s = ff_max / ff_mag;
            (self.velocity.0 * s, self.velocity.1 * s)
        } else {
            self.velocity
        };
        correction[2] -= ff.0;
        correction[5] -= ff.1;

        // Bookkeeping: the translation effect of what we actually applied.
        self.last_correction_vec = (
            correction[0] * CENTER.0 + correction[1] * CENTER.1 + correction[2] - CENTER.0,
            correction[3] * CENTER.0 + correction[4] * CENTER.1 + correction[5] - CENTER.1,
        );

        TrackOutcome {
            matched,
            mean_shift,
            mean_score,
            correction: Some(correction),
        }
    }

    /// NCC-match one label template in a window around its layout rect.
    /// Returns the observed center and score.
    fn match_label(&self, gray: &Image, site: &LabelSite, radius: f64) -> Option<(f64, f64, f64)> {
        let (tw, th) = (site.template.width, site.template.height);
        let x0 = (site.rect.x - radius).max(0.0) as usize;
        let y0 = (site.rect.y - radius).max(0.0) as usize;
        let x1 = ((site.rect.x2() + radius).round_ties_even() as usize).min(gray.width);
        let y1 = ((site.rect.y2() + radius).round_ties_even() as usize).min(gray.height);
        if x1.saturating_sub(x0) < tw || y1.saturating_sub(y0) < th {
            return None;
        }
        let window = gray.crop(x0, y0, x1 - x0, y1 - y0);
        let response = ncc::match_template_ccoeff_normed(&window, &site.template);
        let (max_val, (mx, my)) = response.max();
        let score = f64::from(max_val);
        if score < self.cfg.min_label_score {
            return None;
        }
        Some((
            (x0 + mx) as f64 + tw as f64 / 2.0,
            (y0 + my) as f64 + th as f64 / 2.0,
            score,
        ))
    }
}

/// Locate the playfield's vertical edge near `expected_x` on row `y`: the
/// first position (scanning inward) where a run of 3 dark pixels starts.
/// Skips the constraint when the expected interior isn't dark (tall stack,
/// clear flash) — a bright interior would fake an edge shift.
fn find_edge(gray: &Image, y: f64, expected_x: f64, from_left: bool) -> Option<f64> {
    let y = y.round_ties_even() as i32;
    if y < 0 || y as usize >= gray.height {
        return None;
    }
    let row = &gray.data[y as usize * gray.width..(y as usize + 1) * gray.width];
    let at = |x: i32| -> Option<u8> { row.get(usize::try_from(x).ok()?).copied() };
    let inward: i32 = if from_left { 1 } else { -1 };
    let ex = expected_x.round_ties_even() as i32;

    // Interior guard: 4..8 px inside the expected boundary must be dark.
    for d in 4..8 {
        if at(ex + inward * d).is_none_or(|v| v >= DARK) {
            return None;
        }
    }
    // Scan from outside inward for the start of a 3-px dark run.
    for offset in -EDGE_SCAN..=EDGE_SCAN {
        let x = ex + inward * offset;
        let dark_run = (0..3).all(|d| at(x + inward * d).is_some_and(|v| v < DARK));
        let outside_bright = at(x - inward).is_some_and(|v| v >= DARK);
        if dark_run && outside_bright {
            return Some(f64::from(x) - if from_left { 0.0 } else { 0.0 });
        }
    }
    None
}

/// Weighted least-squares similarity fit mapping observed -> ideal:
/// `x' = a*x - b*y + tx`, `y' = b*x + a*y + ty`.
///
/// `points` are full 2-D constraints `(w, observed, ideal)`; `edges` are
/// horizontal-only constraints `(w, observed(x, y), ideal_x)`. Returns
/// `[a, b, tx, a, ty]` (a duplicated for the damping code's convenience).
fn fit_similarity(
    points: &[(f64, (f64, f64), (f64, f64))],
    edges: &[(f64, (f64, f64), f64)],
) -> Option<[f64; 5]> {
    // Parameters p = [a, b, tx, ty].
    let mut m = [[0.0f64; 4]; 4];
    let mut v = [0.0f64; 4];
    let mut add_row = |w: f64, row: [f64; 4], rhs: f64| {
        for i in 0..4 {
            for j in 0..4 {
                m[i][j] += w * row[i] * row[j];
            }
            v[i] += w * row[i] * rhs;
        }
    };
    for &(w, (x, y), (ix, iy)) in points {
        add_row(w, [x, -y, 1.0, 0.0], ix);
        add_row(w, [y, x, 0.0, 1.0], iy);
    }
    for &(w, (x, y), ix) in edges {
        add_row(w, [x, -y, 1.0, 0.0], ix);
    }

    let p = solve4(m, v)?;
    // Reject degenerate fits (collapse/flip); shake never scales this much.
    let scale = p[0].hypot(p[1]);
    if !(0.8..=1.25).contains(&scale) {
        return None;
    }
    Some([p[0], p[1], p[2], p[0], p[3]])
}

/// Gaussian elimination with partial pivoting for the 4x4 normal equations.
fn solve4(mut m: [[f64; 4]; 4], mut v: [f64; 4]) -> Option<[f64; 4]> {
    for col in 0..4 {
        let pivot = (col..4).max_by(|&a, &b| {
            m[a][col]
                .abs()
                .partial_cmp(&m[b][col].abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })?;
        if m[pivot][col].abs() < 1e-9 {
            return None;
        }
        m.swap(col, pivot);
        v.swap(col, pivot);
        for row in (col + 1)..4 {
            let f = m[row][col] / m[col][col];
            for k in col..4 {
                m[row][k] -= f * m[col][k];
            }
            v[row] -= f * v[col];
        }
    }
    let mut out = [0.0f64; 4];
    for col in (0..4).rev() {
        let mut acc = v[col];
        for k in (col + 1)..4 {
            acc -= m[col][k] * out[k];
        }
        out[col] = acc / m[col][col];
    }
    Some(out)
}

/// Mean displacement of the canonical corners under a canonical-space
/// correction — the deadband metric.
fn mean_corner_displacement(correction: &Mat3) -> f64 {
    let corners = [(0.0, 0.0), (256.0, 0.0), (256.0, 240.0), (0.0, 240.0)];
    corners
        .iter()
        .map(|&(x, y)| {
            let nx = correction[0] * x + correction[1] * y + correction[2];
            let ny = correction[3] * x + correction[4] * y + correction[5];
            (nx - x).hypot(ny - y)
        })
        .sum::<f64>()
        / 4.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::get_layout;

    fn tracker(cfg: TrackingConfig) -> LocalTracker {
        LocalTracker::new(cfg, get_layout())
    }

    fn enabled_cfg() -> TrackingConfig {
        TrackingConfig {
            enabled: true,
            ..TrackingConfig::default()
        }
    }

    /// Synthetic canonical frame: gray background, dark playfield, the real
    /// label templates blitted at their layout rects shifted by (dx, dy).
    fn synthetic_canonical(dx: f64, dy: f64) -> Image {
        let layout = get_layout();
        let mut img = Image::new(256, 240, 1);
        img.data.fill(120);
        let (px0, py0, px1, py1) = layout.playfield.to_bounds();
        for y in py0..py1 {
            for x in px0..px1 {
                let sx = (x as f64 - dx).round_ties_even();
                let sy = (y as f64 - dy).round_ties_even();
                // Shift the dark region too, so edges move with the labels.
                let inside = sx >= px0 as f64 && sx < px1 as f64 && sy >= py0 as f64;
                img.data[y * 256 + x] = if inside { 12 } else { 120 };
            }
        }
        // Extend dark shift beyond the nominal rect on the leading side.
        for y in py0..py1 {
            for x in 0..256usize {
                let ideal_x = x as f64 - dx;
                let ideal_y = y as f64 - dy;
                if ideal_x >= layout.playfield.x
                    && ideal_x < layout.playfield.x2()
                    && ideal_y >= layout.playfield.y
                    && ideal_y < layout.playfield.y2()
                {
                    img.data[y * 256 + x] = 12;
                }
            }
        }
        for (name, rect) in [
            ("LINES", layout.label_lines),
            ("SCORE", layout.label_score),
            ("NEXT", layout.label_next),
            ("LEVEL", layout.label_level),
        ] {
            let (_, tpl) = label_templates().iter().find(|(l, _)| *l == name).unwrap();
            let (x0, y0, x1, y1) = rect.to_bounds();
            let tpl = resize::resize_area(tpl, x1 - x0, y1 - y0);
            for ty in 0..tpl.height {
                for tx in 0..tpl.width {
                    let gx = (x0 + tx) as i64 + dx.round_ties_even() as i64;
                    let gy = (y0 + ty) as i64 + dy.round_ties_even() as i64;
                    if (0..256).contains(&gx) && (0..240).contains(&gy) {
                        img.data[gy as usize * 256 + gx as usize] =
                            tpl.data[ty * tpl.width + tx];
                    }
                }
            }
        }
        img
    }

    #[test]
    fn aligned_frame_stays_in_deadband() {
        let mut t = tracker(enabled_cfg());
        let outcome = t.step(&synthetic_canonical(0.0, 0.0));
        assert_eq!(outcome.matched, 4, "all labels found on aligned frame");
        assert!(outcome.mean_shift < 0.75, "shift {}", outcome.mean_shift);
        assert!(outcome.correction.is_none(), "deadband suppresses noise");
    }

    #[test]
    fn shifted_frame_yields_correction() {
        let mut t = tracker(enabled_cfg());
        let (dx, dy) = (4.0, -3.0);
        let outcome = t.step(&synthetic_canonical(dx, dy));
        assert!(outcome.matched >= 3, "matched {}", outcome.matched);
        assert!(
            (outcome.mean_shift - dx.hypot(dy)).abs() < 1.5,
            "measured {} expected {}",
            outcome.mean_shift,
            dx.hypot(dy)
        );
        let corr = outcome.correction.expect("correction above deadband");
        // The damped correction must move content back toward ideal:
        // observed point (center + d) maps near center + (1-damping)*d.
        let l = enabled_cfg().damping;
        let (cx, cy) = (128.0 + dx, 110.0 + dy);
        let nx = corr[0] * cx + corr[1] * cy + corr[2];
        let ny = corr[3] * cx + corr[4] * cy + corr[5];
        let residual = (nx - 128.0 - dx * (1.0 - l)).hypot(ny - 110.0 - dy * (1.0 - l));
        assert!(residual < 1.5, "residual {residual}");
    }

    #[test]
    fn iterated_corrections_converge() {
        // Apply the tracker's own correction to its measurement loop: the
        // remaining offset must shrink monotonically to (near) zero.
        let mut t = tracker(enabled_cfg());
        let (mut dx, mut dy) = (6.0, 5.0);
        for _ in 0..8 {
            let outcome = t.step(&synthetic_canonical(dx, dy));
            let Some(corr) = outcome.correction else {
                break; // inside deadband: converged
            };
            // The correction maps observed->ideal; the residual offset after
            // applying it is what the next frame would see.
            let (ox, oy) = (128.0 + dx, 110.0 + dy);
            let nx = corr[0] * ox + corr[1] * oy + corr[2];
            let ny = corr[3] * ox + corr[4] * oy + corr[5];
            (dx, dy) = (nx - 128.0, ny - 110.0);
        }
        assert!(
            dx.hypot(dy) < 1.0,
            "did not converge: residual ({dx:.2}, {dy:.2})"
        );
    }

    #[test]
    fn similarity_fit_recovers_exact_transform() {
        // observed = ideal shifted by (3, -2): fit must invert that.
        let ideal = [(40.0, 20.0), (200.0, 24.0), (210.0, 120.0), (60.0, 180.0)];
        let points: Vec<_> = ideal
            .iter()
            .map(|&(x, y)| (1.0, (x + 3.0, y - 2.0), (x, y)))
            .collect();
        let p = fit_similarity(&points, &[]).unwrap();
        assert!((p[0] - 1.0).abs() < 1e-9); // a = 1 (no rotation/scale)
        assert!(p[1].abs() < 1e-9); // b = 0
        assert!((p[2] + 3.0).abs() < 1e-9); // tx = -3
        assert!((p[4] - 2.0).abs() < 1e-9); // ty = +2
    }

    #[test]
    fn similarity_fit_recovers_rotation() {
        let angle: f64 = 0.02; // ~1.1 degrees
        let (s, c) = angle.sin_cos();
        let ideal = [(40.0, 20.0), (200.0, 24.0), (210.0, 120.0), (60.0, 180.0)];
        let points: Vec<_> = ideal
            .iter()
            .map(|&(x, y)| (1.0, (c * x - s * y, s * x + c * y), (x, y)))
            .collect();
        let p = fit_similarity(&points, &[]).unwrap();
        // Fitted transform must be the inverse rotation.
        assert!((p[0] - c).abs() < 1e-6);
        assert!((p[1] + s).abs() < 1e-6);
    }

    #[test]
    fn degenerate_fit_rejected() {
        let points = vec![
            (1.0, (10.0, 10.0), (100.0, 100.0)),
            (1.0, (10.1, 10.0), (200.0, 100.0)),
        ];
        assert!(fit_similarity(&points, &[]).is_none(), "absurd scale");
    }

    #[test]
    fn too_few_labels_is_a_miss() {
        let mut t = tracker(enabled_cfg());
        let mut img = Image::new(256, 240, 1);
        img.data.fill(120); // no labels anywhere
        let outcome = t.step(&img);
        assert!(outcome.matched < 2);
        assert!(outcome.correction.is_none());
    }
}

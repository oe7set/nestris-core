//! Playfield recognition: the 10×20 occupancy/color grid
//! (port of `recognition/playfield.py`, reference per-cell path — which is
//! bit-identical to the Python vectorized path by construction).

use nestris_vision::{Image, color, stats};

use crate::layout::{LayoutTable, PLAYFIELD_COLS, PLAYFIELD_ROWS};
use crate::nes_palette;
use crate::palette::{
    self, CellView, DEFAULT_FILL_MARGIN, MIN_FILL_MARGIN, WHITE_LAB, border_indices,
    bright_interior_mean_lab, center_border, chroma_margin_for, is_white_lab, occupancy_strength,
};

pub const EMPTY_ID: u8 = 0;
pub const WHITE_ID: u8 = 1;
pub const ACCENT_A_ID: u8 = 2;
pub const ACCENT_B_ID: u8 = 3;

const MIN_ACCENT_DIST: f32 = 34.0;
const KMEANS_ITERS: usize = 8;
const MARGIN_BRIGHT_FRAC: f32 = 0.18;
const BRIGHT_PCTILE: f64 = 90.0;
const EXPOSURE_REF: f32 = 248.0;
const EXPOSURE_MIN: f32 = 0.30;
const EXPOSURE_MAX: f32 = 1.10;
const AMBIGUITY_RATIO: f32 = 0.75;
const NEUTRAL_CHROMA: f32 = 30.0;
const GRAY_WHITE_SPLIT: f32 = 0.8;
/// Scale of the hue-angle cost term: a π hue difference costs this many
/// Lab-distance units (comparable magnitude to `dist3` on accents).
const HUE_SCALE: f32 = 120.0;
/// Target-pair separation that keeps the full ambiguity ratio (adaptive
/// ambiguity tightens the ratio for palettes with closer accents).
const AMBIGUITY_REF_DIST: f32 = 60.0;
/// Minimum white-classified cells before white-balance gains are trusted.
const WB_MIN_WHITE_CELLS: usize = 3;
const WB_GAIN_MIN: f32 = 0.7;
const WB_GAIN_MAX: f32 = 1.3;

/// Optional color-discrimination refinements (all default-off: the plain
/// path stays bit-identical to the verified oracle port).
#[derive(Clone, Copy, Debug, Default)]
pub struct ColorTuning {
    /// Weight of the hue-angle term in accent assignment, `0.0..=1.0`.
    pub hue_weight: f32,
    /// Tighten the ambiguity threshold when the level's targets are close.
    pub adaptive_ambiguity: bool,
    /// Estimate per-channel gains from white cells and re-assign once.
    pub white_balance: bool,
}

impl ColorTuning {
    pub fn from_config(cfg: &crate::config::RecognitionConfig) -> Self {
        Self {
            hue_weight: cfg.color_hue_weight.clamp(0.0, 1.0) as f32,
            adaptive_ambiguity: cfg.adaptive_ambiguity,
            white_balance: cfg.white_balance,
        }
    }
}

pub type Grid<T> = [[T; PLAYFIELD_COLS]; PLAYFIELD_ROWS];

/// Result of reading the playfield.
#[derive(Clone, Debug)]
pub struct PlayfieldReading {
    /// 0 = empty, 1 = white, 2/3 = the level's two accents.
    pub grid: Grid<u8>,
    pub occupancy: Grid<bool>,
    pub confidence: f32,
    pub filled_count: usize,
    /// Per-cell occupancy strength (> 1.0 = occupied), for the stabilizer.
    pub strength: Grid<f32>,
    /// True where a filled cell's color id was too close to call.
    pub color_ambiguous: Grid<bool>,
}

impl PlayfieldReading {
    pub fn grid_as_rows(&self) -> Vec<Vec<u8>> {
        self.grid.iter().map(|r| r.to_vec()).collect()
    }
}

/// Adaptive occupancy margin from all 200 per-cell contrasts.
fn adaptive_margin(deltas: &[f32]) -> f32 {
    let finite: Vec<f64> = deltas
        .iter()
        .filter(|v| v.is_finite())
        .map(|&v| (v.max(0.0)) as f64)
        .collect();
    if finite.is_empty() {
        return DEFAULT_FILL_MARGIN;
    }
    let reference = stats::percentile(&finite, BRIGHT_PCTILE) as f32;
    (MARGIN_BRIGHT_FRAC * reference).clamp(MIN_FILL_MARGIN, DEFAULT_FILL_MARGIN)
}

/// Estimate capture exposure from filled cells' Lab features
/// (port of `_estimate_exposure`: Lab -> u8 -> BGR, peak channel, p95 / 248).
fn estimate_exposure(lab_feats: &[(f32, f32, f32)]) -> f32 {
    let n = lab_feats.len();
    if n == 0 {
        return 1.0;
    }
    let peaks: Vec<f64> = lab_feats
        .iter()
        .map(|&(l, a, b)| {
            // np.clip(...).astype(np.uint8) truncates toward zero.
            let (bb, gg, rr) = color::lab_pixel_to_bgr(
                l.clamp(0.0, 255.0) as u8,
                a.clamp(0.0, 255.0) as u8,
                b.clamp(0.0, 255.0) as u8,
            );
            bb.max(gg).max(rr) as f64
        })
        .collect();
    let reference = if n >= 4 {
        stats::percentile(&peaks, 95.0)
    } else {
        peaks.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
    };
    ((reference as f32) / EXPOSURE_REF).clamp(EXPOSURE_MIN, EXPOSURE_MAX)
}

/// Scale full-brightness Lab targets down to the frame exposure in BGR space.
fn scale_targets(raw_targets: &[(f32, f32, f32); 3], exposure: f32) -> [(f32, f32, f32); 3] {
    raw_targets.map(|(l, a, b)| {
        let (bb, gg, rr) = color::lab_pixel_to_bgr(
            l.clamp(0.0, 255.0) as u8,
            a.clamp(0.0, 255.0) as u8,
            b.clamp(0.0, 255.0) as u8,
        );
        // float32 * exposure, clip, astype(uint8) truncation — then back to Lab.
        let scale = |v: u8| ((v as f32 * exposure).clamp(0.0, 255.0)) as u8;
        let (l2, a2, b2) = color::bgr_pixel_to_lab(scale(bb), scale(gg), scale(rr));
        (l2 as f32, a2 as f32, b2 as f32)
    })
}

/// The level's three color targets in CIELAB: (white, accentA, accentB).
fn level_targets(level: i64) -> [(f32, f32, f32); 3] {
    let (a_rgb, b_rgb) = nes_palette::LEVEL_PALETTE[(level.rem_euclid(10)) as usize];
    let a_lab = palette::bgr_to_lab((a_rgb.2, a_rgb.1, a_rgb.0));
    let b_lab = palette::bgr_to_lab((b_rgb.2, b_rgb.1, b_rgb.0));
    [WHITE_LAB, a_lab, b_lab]
}

/// A CIELAB triple on OpenCV's 0-255 scale.
pub type Lab = (f32, f32, f32);

fn dist3(a: Lab, b: Lab) -> f32 {
    let d = (a.0 - b.0, a.1 - b.1, a.2 - b.2);
    (d.0 * d.0 + d.1 * d.1 + d.2 * d.2).sqrt()
}

/// Deterministic 2-means over accent features (port of `_cluster_accents`).
fn cluster_accents(feats: &[Lab]) -> Option<(Lab, Lab)> {
    let n = feats.len();
    if n < 2 {
        return None;
    }
    let i = (0..n)
        .max_by(|&a, &b| {
            dist3(feats[a], feats[0])
                .partial_cmp(&dist3(feats[b], feats[0]))
                .unwrap()
        })
        .unwrap();
    let j = (0..n)
        .max_by(|&a, &b| {
            dist3(feats[a], feats[i])
                .partial_cmp(&dist3(feats[b], feats[i]))
                .unwrap()
        })
        .unwrap();
    if i == j {
        return None;
    }
    let mut centers = [feats[i], feats[j]];
    let mut labels = vec![0u8; n];
    for _ in 0..KMEANS_ITERS {
        let new_labels: Vec<u8> = feats
            .iter()
            .map(|&f| u8::from(dist3(f, centers[1]) < dist3(f, centers[0])))
            .collect();
        let converged = new_labels == labels;
        labels = new_labels;
        if converged {
            break;
        }
        for k in 0..2u8 {
            let members: Vec<&(f32, f32, f32)> = feats
                .iter()
                .zip(labels.iter())
                .filter(|(_, l)| **l == k)
                .map(|(f, _)| f)
                .collect();
            if !members.is_empty() {
                let n = members.len() as f32;
                centers[k as usize] = (
                    members.iter().map(|f| f.0).sum::<f32>() / n,
                    members.iter().map(|f| f.1).sum::<f32>() / n,
                    members.iter().map(|f| f.2).sum::<f32>() / n,
                );
            }
        }
    }
    if dist3(centers[0], centers[1]) < MIN_ACCENT_DIST {
        return None;
    }
    Some((centers[0], centers[1]))
}

/// Reads the 10×20 stack from a canonical frame.
pub struct PlayfieldReader;

impl PlayfieldReader {
    /// Read the playfield with default (oracle-exact) color handling.
    pub fn read(
        canon: &Image,
        gray: &Image,
        layout: &LayoutTable,
        level: Option<i64>,
    ) -> PlayfieldReading {
        Self::read_with(canon, gray, layout, level, ColorTuning::default())
    }

    /// Read the playfield. `gray` is the shared whole-frame luma; `tuning`
    /// selects the optional color-discrimination refinements.
    pub fn read_with(
        canon: &Image,
        gray: &Image,
        layout: &LayoutTable,
        level: Option<i64>,
        tuning: ColorTuning,
    ) -> PlayfieldReading {
        let pf = &layout.playfield;
        let (px0, py0, px1, py1) = pf.to_bounds();
        let (rw, rh) = (px1 - px0, py1 - py0);
        // Region luma as f32 + region Lab (pointwise, identical to slicing a
        // whole-frame conversion).
        let mut gray_region = vec![0.0f32; rw * rh];
        let mut lab_region = vec![(0.0f32, 0.0f32, 0.0f32); rw * rh];
        for y in 0..rh {
            for x in 0..rw {
                let src = (py0 + y) * gray.width + (px0 + x);
                gray_region[y * rw + x] = gray.data[src] as f32;
                let px = canon.pixel(px0 + x, py0 + y);
                let (l, a, b) = color::bgr_pixel_to_lab(px[0], px[1], px[2]);
                lab_region[y * rw + x] = (l as f32, a as f32, b as f32);
            }
        }

        let mut grid: Grid<u8> = [[0; PLAYFIELD_COLS]; PLAYFIELD_ROWS];
        let mut occupancy: Grid<bool> = [[false; PLAYFIELD_COLS]; PLAYFIELD_ROWS];
        let mut strength: Grid<f32> = [[0.0; PLAYFIELD_COLS]; PLAYFIELD_ROWS];
        let mut ambiguous: Grid<bool> = [[false; PLAYFIELD_COLS]; PLAYFIELD_ROWS];

        // Cell geometry relative to the region origin.
        let cell_bounds = |row: usize, col: usize| {
            let cell = layout.playfield_cell(row, col);
            let rel = crate::geometry::Rect::new(cell.x - pf.x, cell.y - pf.y, cell.w, cell.h);
            let (x0, y0, x1, y1) = rel.to_bounds();
            (x0.min(rw), y0.min(rh), x1.min(rw), y1.min(rh))
        };

        // Pass 1: per-cell luma contrast deltas for the adaptive margin.
        let mut deltas = [0.0f32; PLAYFIELD_ROWS * PLAYFIELD_COLS];
        for row in 0..PLAYFIELD_ROWS {
            for col in 0..PLAYFIELD_COLS {
                let (x0, y0, x1, y1) = cell_bounds(row, col);
                if x1 <= x0 || y1 <= y0 {
                    continue;
                }
                let view = CellView {
                    data: &gray_region[y0 * rw + x0..],
                    stride: rw,
                    h: y1 - y0,
                    w: x1 - x0,
                };
                if let Some((center, border)) = center_border(&view) {
                    deltas[row * PLAYFIELD_COLS + col] = center - border;
                }
            }
        }
        let margin = adaptive_margin(&deltas);
        let chroma_margin = chroma_margin_for(margin);

        // Pass 2: strength/occupancy/color features.
        let mut conf_sum = 0.0f64;
        let mut conf_count = 0usize;
        let mut filled_cells: Vec<(usize, usize)> = Vec::new();
        let mut filled_feats: Vec<(f32, f32, f32)> = Vec::new();
        for row in 0..PLAYFIELD_ROWS {
            for col in 0..PLAYFIELD_COLS {
                let (x0, y0, x1, y1) = cell_bounds(row, col);
                if x1 <= x0 || y1 <= y0 {
                    conf_count += 1;
                    continue;
                }
                let (h, w) = (y1 - y0, x1 - x0);
                let view = CellView {
                    data: &gray_region[y0 * rw + x0..],
                    stride: rw,
                    h,
                    w,
                };
                let cell_str;
                let conf;
                if let Some((center, border)) = center_border(&view) {
                    // Interior-vs-border chroma delta (palette._cell_chroma_delta).
                    let (cy0, cy1) = (h / 4, h - h / 4);
                    let (cx0, cx1) = (w / 4, w - w / 4);
                    let chroma_at = |y: usize, x: usize| {
                        let (_, a, b) = lab_region[(y0 + y) * rw + (x0 + x)];
                        (a - 128.0).hypot(b - 128.0)
                    };
                    let mut csum = 0.0f64;
                    let mut ccount = 0usize;
                    for y in cy0..cy1 {
                        for x in cx0..cx1 {
                            csum += chroma_at(y, x) as f64;
                            ccount += 1;
                        }
                    }
                    let c_center = (csum / ccount as f64) as f32;
                    let mut samples = [0.0f32; 8];
                    for (k, (sy, sx)) in border_indices(h, w).into_iter().enumerate() {
                        samples[k] = chroma_at(sy, sx);
                    }
                    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    let c_border = (samples[3] + samples[4]) / 2.0;
                    let chroma_delta = c_center - c_border;
                    cell_str =
                        occupancy_strength(center - border, chroma_delta, margin, chroma_margin);
                    conf = (cell_str - 1.0).abs().clamp(0.0, 1.0);
                } else {
                    // Cell too small: absolute-brightness fallback.
                    let mut sum = 0.0f64;
                    for y in 0..h {
                        for x in 0..w {
                            sum += view.at(y, x) as f64;
                        }
                    }
                    let mean = (sum / (h * w) as f64) as f32;
                    cell_str = mean / 64.0;
                    conf = (mean / 128.0).min(1.0);
                }
                strength[row][col] = cell_str;
                conf_sum += conf as f64;
                conf_count += 1;
                if cell_str <= 1.0 {
                    continue;
                }
                occupancy[row][col] = true;
                // Brightest-interior mean Lab (cell_color_lab_from_arrays).
                let (icy0, icy1, icx0, icx1) = if h >= 3 && w >= 3 {
                    (h / 4, h - h / 4, w / 4, w - w / 4)
                } else {
                    (0, h, 0, w)
                };
                let mut labs = Vec::with_capacity((icy1 - icy0) * (icx1 - icx0));
                let mut grays = Vec::with_capacity(labs.capacity());
                for y in icy0..icy1 {
                    for x in icx0..icx1 {
                        labs.push(lab_region[(y0 + y) * rw + (x0 + x)]);
                        grays.push(gray_region[(y0 + y) * rw + (x0 + x)]);
                    }
                }
                filled_cells.push((row, col));
                filled_feats.push(bright_interior_mean_lab(&labs, &grays));
            }
        }

        if !filled_feats.is_empty() {
            assign_colors(
                &mut grid,
                &filled_cells,
                &filled_feats,
                level,
                &mut ambiguous,
                tuning,
            );
        }
        let confidence = if conf_count > 0 {
            (conf_sum / conf_count as f64) as f32
        } else {
            0.0
        };
        PlayfieldReading {
            grid,
            occupancy,
            confidence,
            filled_count: filled_cells.len(),
            strength,
            color_ambiguous: ambiguous,
        }
    }
}

/// Hue angle of a Lab point around the neutral axis.
fn hue_angle(f: Lab) -> f32 {
    (f.2 - 128.0).atan2(f.1 - 128.0)
}

fn chroma_of(f: Lab) -> f32 {
    (f.1 - 128.0).hypot(f.2 - 128.0)
}

/// Assignment cost of feature `f` against target `t`: pure Lab distance, or
/// a hue-blended cost when both are chromatic (hue is far more exposure-
/// invariant than Lab distance for saturated accents).
fn assign_cost(f: Lab, t: Lab, hue_weight: f32) -> f32 {
    let d = dist3(f, t);
    if hue_weight <= 0.0 || chroma_of(t) < NEUTRAL_CHROMA || chroma_of(f) < NEUTRAL_CHROMA {
        return d;
    }
    let mut dh = (hue_angle(f) - hue_angle(t)).abs();
    if dh > std::f32::consts::PI {
        dh = 2.0 * std::f32::consts::PI - dh;
    }
    let hue_term = dh / std::f32::consts::PI * HUE_SCALE;
    (1.0 - hue_weight) * d + hue_weight * hue_term
}

/// One pass of nearest-target assignment; returns (ids, ambiguity flags).
fn assign_pass(feats: &[Lab], targets: &[Lab; 3], tuning: ColorTuning) -> (Vec<u8>, Vec<bool>) {
    let mut ids: Vec<u8> = Vec::with_capacity(feats.len());
    let mut amb: Vec<bool> = Vec::with_capacity(feats.len());
    for &f in feats {
        let d = [
            assign_cost(f, targets[0], tuning.hue_weight),
            assign_cost(f, targets[1], tuning.hue_weight),
            assign_cost(f, targets[2], tuning.hue_weight),
        ];
        // np.argsort ascending, stable: ties keep lower index (white first).
        let mut order = [0usize, 1, 2];
        order.sort_by(|&a, &b| d[a].partial_cmp(&d[b]).unwrap());
        ids.push(order[0] as u8 + 1);
        // Adaptive ambiguity: palettes whose two best targets sit close in
        // Lab flag ambiguity earlier (the voter resolves those cells).
        let ratio = if tuning.adaptive_ambiguity {
            let sep = dist3(targets[order[0]], targets[order[1]]);
            AMBIGUITY_RATIO * (sep / AMBIGUITY_REF_DIST).clamp(0.6, 1.0)
        } else {
            AMBIGUITY_RATIO
        };
        amb.push(d[order[0]] > ratio * d[order[1]]);
    }
    (ids, amb)
}

/// Per-channel white-balance gains estimated from white-classified cells:
/// `observed white BGR / (reference white * exposure)`, clamped.
fn white_balance_gains(feats: &[Lab], ids: &[u8], exposure: f32) -> Option<(f32, f32, f32)> {
    let whites: Vec<Lab> = feats
        .iter()
        .zip(ids.iter())
        .filter(|&(_, &id)| id == WHITE_ID)
        .map(|(&f, _)| f)
        .collect();
    if whites.len() < WB_MIN_WHITE_CELLS {
        return None;
    }
    let mut sum = (0.0f32, 0.0f32, 0.0f32);
    for &(l, a, b) in &whites {
        let (bb, gg, rr) = color::lab_pixel_to_bgr(
            l.clamp(0.0, 255.0) as u8,
            a.clamp(0.0, 255.0) as u8,
            b.clamp(0.0, 255.0) as u8,
        );
        sum = (sum.0 + bb as f32, sum.1 + gg as f32, sum.2 + rr as f32);
    }
    let n = whites.len() as f32;
    let reference = EXPOSURE_REF * exposure;
    let gain = |channel_mean: f32| (channel_mean / reference).clamp(WB_GAIN_MIN, WB_GAIN_MAX);
    Some((gain(sum.0 / n), gain(sum.1 / n), gain(sum.2 / n)))
}

/// Scale raw Lab targets by exposure and per-channel gains in BGR space.
fn scale_targets_per_channel(
    raw_targets: &[Lab; 3],
    exposure: f32,
    gains: (f32, f32, f32),
) -> [Lab; 3] {
    raw_targets.map(|(l, a, b)| {
        let (bb, gg, rr) = color::lab_pixel_to_bgr(
            l.clamp(0.0, 255.0) as u8,
            a.clamp(0.0, 255.0) as u8,
            b.clamp(0.0, 255.0) as u8,
        );
        let scale = |v: u8, g: f32| ((v as f32 * exposure * g).clamp(0.0, 255.0)) as u8;
        let (l2, a2, b2) =
            color::bgr_pixel_to_lab(scale(bb, gains.0), scale(gg, gains.1), scale(rr, gains.2));
        (l2 as f32, a2 as f32, b2 as f32)
    })
}

/// Assign color ids to filled cells (port of `_assign_colors`, plus the
/// optional [`ColorTuning`] refinements).
fn assign_colors(
    grid: &mut Grid<u8>,
    cells: &[(usize, usize)],
    feats: &[(f32, f32, f32)],
    level: Option<i64>,
    ambiguous: &mut Grid<bool>,
    tuning: ColorTuning,
) {
    if let Some(level) = level {
        let raw_targets = level_targets(level);
        let exposure = estimate_exposure(feats);
        let targets = scale_targets(&raw_targets, exposure);
        let (mut ids, mut amb) = assign_pass(feats, &targets, tuning);
        if tuning.white_balance
            && let Some(gains) = white_balance_gains(feats, &ids, exposure)
        {
            let rebalanced = scale_targets_per_channel(&raw_targets, exposure, gains);
            (ids, amb) = assign_pass(feats, &rebalanced, tuning);
        }
        split_neutral_gray(feats, &mut ids, &raw_targets);
        for (i, &(row, col)) in cells.iter().enumerate() {
            grid[row][col] = ids[i];
            ambiguous[row][col] = amb[i];
        }
        return;
    }

    // Level-agnostic fallback: white by absolute test, then cluster accents.
    let mut accent_idx: Vec<usize> = Vec::new();
    for (i, (&(row, col), &f)) in cells.iter().zip(feats.iter()).enumerate() {
        if is_white_lab(f) {
            grid[row][col] = WHITE_ID;
        } else {
            grid[row][col] = ACCENT_A_ID; // provisional
            accent_idx.push(i);
        }
    }
    if accent_idx.is_empty() {
        return;
    }
    let acc: Vec<(f32, f32, f32)> = accent_idx.iter().map(|&i| feats[i]).collect();
    let Some((mut ca, mut cb)) = cluster_accents(&acc) else {
        return;
    };
    if ca.0 > cb.0 {
        std::mem::swap(&mut ca, &mut cb);
    }
    for (k, &i) in accent_idx.iter().enumerate() {
        let (row, col) = cells[i];
        let da = dist3(acc[k], ca);
        let db = dist3(acc[k], cb);
        grid[row][col] = if da <= db { ACCENT_A_ID } else { ACCENT_B_ID };
        let (lo, hi) = if da <= db { (da, db) } else { (db, da) };
        ambiguous[row][col] = lo > AMBIGUITY_RATIO * hi;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_tuning_matches_plain_distance() {
        let targets: [Lab; 3] = [
            (240.0, 128.0, 128.0),
            (150.0, 190.0, 128.0),
            (150.0, 128.0, 190.0),
        ];
        let feats = vec![(150.0, 185.0, 130.0), (238.0, 129.0, 127.0)];
        let (ids, amb) = assign_pass(&feats, &targets, ColorTuning::default());
        assert_eq!(ids, vec![ACCENT_A_ID, WHITE_ID]);
        assert_eq!(amb, vec![false, false]);
    }

    #[test]
    fn hue_weight_rescues_dim_but_hue_true_accent() {
        // Feature: accent A's hue (pure +a), strongly dimmed. A decoy
        // target B sits closer in plain Lab distance but 90° away in hue.
        let t_white: Lab = (250.0, 128.0, 128.0);
        let t_a: Lab = (200.0, 190.0, 128.0); // hue 0°
        let t_b: Lab = (140.0, 128.0, 150.0); // hue 90°, dim
        let feat: Lab = (130.0, 170.0, 128.0); // hue 0°, dim
        let targets = [t_white, t_a, t_b];

        let (plain_ids, _) = assign_pass(&[feat], &targets, ColorTuning::default());
        assert_eq!(plain_ids[0], ACCENT_B_ID, "plain distance picks the decoy");

        let tuned = ColorTuning {
            hue_weight: 0.7,
            ..ColorTuning::default()
        };
        let (hue_ids, _) = assign_pass(&[feat], &targets, tuned);
        assert_eq!(hue_ids[0], ACCENT_A_ID, "hue term recovers the true accent");
    }

    #[test]
    fn adaptive_ambiguity_flags_close_palettes_earlier() {
        // Accents 56.6 Lab units apart tighten the ratio from 0.75 to
        // ~0.707; the feature's best/second distance ratio (~0.73) falls
        // exactly between the two thresholds.
        let targets: [Lab; 3] = [
            (250.0, 128.0, 128.0),
            (150.0, 168.0, 128.0),
            (150.0, 128.0, 168.0),
        ];
        let feat: Lab = (150.0, 150.6, 144.4);
        let (_, plain) = assign_pass(&[feat], &targets, ColorTuning::default());
        let (_, adaptive) = assign_pass(
            &[feat],
            &targets,
            ColorTuning {
                adaptive_ambiguity: true,
                ..ColorTuning::default()
            },
        );
        assert!(!plain[0], "plain ratio does not flag this cell");
        assert!(adaptive[0], "tightened ratio flags it for the voter");
    }

    #[test]
    fn white_balance_gains_reflect_color_cast() {
        // White cells captured with a red-heavy cast at exposure 1.0.
        let cast = palette::bgr_to_lab((200, 205, 248));
        let feats = vec![cast; 4];
        let ids = vec![WHITE_ID; 4];
        let (gb, gg, gr) = white_balance_gains(&feats, &ids, 1.0).expect("enough whites");
        assert!(gr > gb, "red gain {gr} should exceed blue gain {gb}");
        assert!(gr > gg);
        assert!((WB_GAIN_MIN..=WB_GAIN_MAX).contains(&gb));
    }

    #[test]
    fn white_balance_needs_enough_whites() {
        let feats = vec![WHITE_LAB; 2];
        let ids = vec![WHITE_ID; 2];
        assert!(white_balance_gains(&feats, &ids, 1.0).is_none());
    }
}

/// Re-separate white vs a low-chroma gray accent by relative brightness
/// (port of `_split_neutral_gray`).
fn split_neutral_gray(
    raw_lab: &[(f32, f32, f32)],
    ids: &mut [u8],
    raw_targets: &[(f32, f32, f32); 3],
) {
    let chroma = raw_targets.map(|(_, a, b)| (a - 128.0).hypot(b - 128.0));
    let mut gray_accent_ids: Vec<u8> = Vec::new();
    for k in [1usize, 2] {
        if chroma[k] < NEUTRAL_CHROMA {
            gray_accent_ids.push(k as u8 + 1);
        }
    }
    if gray_accent_ids.is_empty() {
        return;
    }
    let neutral = |id: u8| id == 1 || gray_accent_ids.contains(&id);
    let neutral_lums: Vec<f32> = ids
        .iter()
        .zip(raw_lab.iter())
        .filter(|(id, _)| neutral(**id))
        .map(|(_, &(l, _, _))| l)
        .collect();
    if neutral_lums.is_empty() {
        return;
    }
    let brightest = neutral_lums
        .iter()
        .cloned()
        .fold(f32::NEG_INFINITY, f32::max);
    let gray_id = gray_accent_ids[0];
    for (id, &(l, _, _)) in ids.iter_mut().zip(raw_lab.iter()) {
        if neutral(*id) {
            *id = if l >= brightest * GRAY_WHITE_SPLIT {
                1
            } else {
                gray_id
            };
        }
    }
}

//! Color-robust cell primitives (port of `recognition/palette.py`).

use nestris_vision::{Image, color};

/// Default center-vs-border luma contrast for a filled cell.
pub const DEFAULT_FILL_MARGIN: f32 = 40.0;
/// Absolute floor on the adaptive fill margin.
pub const MIN_FILL_MARGIN: f32 = 14.0;
/// Fraction of the interior's brightest pixels sampled for a cell's color.
pub const COLOR_BRIGHT_QUANTILE: f64 = 0.5;
/// Minimum luma contrast for the chroma channel to claim a cell.
pub const CHROMA_MIN_LUMA_DELTA: f32 = 6.0;
const CHROMA_MARGIN_FRAC: f32 = 0.5;
const CHROMA_MARGIN_MIN: f32 = 10.0;
const CHROMA_MARGIN_MAX: f32 = 20.0;

/// CIELAB neutral point of the a*/b* axes (OpenCV 0-255 scale).
pub const LAB_NEUTRAL: f32 = 128.0;
/// CIELAB of a pure-white NES tile.
pub const WHITE_LAB: (f32, f32, f32) = (255.0, LAB_NEUTRAL, LAB_NEUTRAL);
const WHITE_CHROMA: f32 = 34.0;
const WHITE_LIGHTNESS: f32 = 170.0;

/// BGR image (or crop) to single-channel luma (`cv2.COLOR_BGR2GRAY` exact).
pub fn to_luma(bgr: &Image) -> Image {
    color::bgr_to_gray(bgr)
}

/// The 8 border sample positions of a cell: 4 corners + 4 edge midpoints.
pub fn border_indices(h: usize, w: usize) -> [(usize, usize); 8] {
    [
        (0, 0),
        (0, w - 1),
        (h - 1, 0),
        (h - 1, w - 1),
        (0, w / 2),
        (h - 1, w / 2),
        (h / 2, 0),
        (h / 2, w - 1),
    ]
}

/// numpy-median of 8 samples (average of the two middle values).
fn median8(mut vals: [f32; 8]) -> f32 {
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (vals[3] + vals[4]) / 2.0
}

/// `(center_mean, border_median)` luma for one cell, or `None` if too small.
/// `cell` is a row-major `h×w` f32 luma view.
pub fn center_border(cell: &CellView<'_>) -> Option<(f32, f32)> {
    let (h, w) = (cell.h, cell.w);
    if h < 3 || w < 3 {
        return None;
    }
    let (cy0, cy1) = (h / 4, h - h / 4);
    let (cx0, cx1) = (w / 4, w - w / 4);
    let mut sum = 0.0f64;
    let mut count = 0usize;
    for y in cy0..cy1 {
        for x in cx0..cx1 {
            sum += cell.at(y, x) as f64;
            count += 1;
        }
    }
    let center = (sum / count as f64) as f32;
    let samples = border_indices(h, w).map(|(y, x)| cell.at(y, x));
    Some((center, median8(samples)))
}

/// A borrowed h×w scalar view into a larger row-major f32 buffer.
pub struct CellView<'a> {
    pub data: &'a [f32],
    pub stride: usize,
    pub h: usize,
    pub w: usize,
}

impl CellView<'_> {
    #[inline]
    pub fn at(&self, y: usize, x: usize) -> f32 {
        self.data[y * self.stride + x]
    }
}

/// The chroma-contrast margin paired with a luma margin.
pub fn chroma_margin_for(margin: f32) -> f32 {
    (CHROMA_MARGIN_FRAC * margin).clamp(CHROMA_MARGIN_MIN, CHROMA_MARGIN_MAX)
}

/// Occupancy strength; `> 1.0` means occupied.
pub fn occupancy_strength(delta: f32, chroma_delta: f32, margin: f32, chroma_margin: f32) -> f32 {
    let luma = delta / margin;
    let chroma = if delta > CHROMA_MIN_LUMA_DELTA {
        chroma_delta / chroma_margin
    } else {
        0.0
    };
    luma.max(chroma)
}

/// Whether a CIELAB color is a bright near-neutral (white) tile.
pub fn is_white_lab(lab: (f32, f32, f32)) -> bool {
    let chroma = (lab.1 - LAB_NEUTRAL).hypot(lab.2 - LAB_NEUTRAL);
    chroma < WHITE_CHROMA && lab.0 > WHITE_LIGHTNESS
}

/// Single BGR color to CIELAB (OpenCV 0-255 scale), via the vision crate's
/// cv2-exact conversion.
pub fn bgr_to_lab(bgr: (u8, u8, u8)) -> (f32, f32, f32) {
    let (l, a, b) = color::bgr_pixel_to_lab(bgr.0, bgr.1, bgr.2);
    (l as f32, a as f32, b as f32)
}

/// Mean CIELAB of a cell's brightest interior pixels (the `n=1` case of the
/// Python `bright_interior_mean_lab_batch`): keep pixels whose luma is >= the
/// `1-COLOR_BRIGHT_QUANTILE` quantile (numpy linear interpolation), average
/// their Lab.
pub fn bright_interior_mean_lab(lab: &[(f32, f32, f32)], gray: &[f32]) -> (f32, f32, f32) {
    debug_assert_eq!(lab.len(), gray.len());
    if gray.is_empty() {
        return (0.0, LAB_NEUTRAL, LAB_NEUTRAL);
    }
    let mut sorted: Vec<f64> = gray.iter().map(|&v| v as f64).collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let thresh =
        nestris_vision::stats::percentile_sorted(&sorted, (1.0 - COLOR_BRIGHT_QUANTILE) * 100.0);
    let mut sum = (0.0f64, 0.0f64, 0.0f64);
    let mut count = 0usize;
    for (l, &g) in lab.iter().zip(gray.iter()) {
        if g as f64 >= thresh {
            sum.0 += l.0 as f64;
            sum.1 += l.1 as f64;
            sum.2 += l.2 as f64;
            count += 1;
        }
    }
    if count == 0 {
        // Quantile never exceeds the max, so this is unreachable; guard anyway.
        let n = lab.len() as f64;
        for l in lab {
            sum.0 += l.0 as f64;
            sum.1 += l.1 as f64;
            sum.2 += l.2 as f64;
        }
        return ((sum.0 / n) as f32, (sum.1 / n) as f32, (sum.2 / n) as f32);
    }
    let n = count as f64;
    ((sum.0 / n) as f32, (sum.1 / n) as f32, (sum.2 / n) as f32)
}

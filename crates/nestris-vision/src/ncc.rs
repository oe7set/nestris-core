//! Normalized cross-correlation matching `cv2.matchTemplate(TM_CCOEFF_NORMED)`.
//!
//! Direct f64 evaluation per window — the engine only ever matches small
//! templates over small slack windows (never whole frames), so the integral-
//! image machinery OpenCV uses for large images is unnecessary; direct sums
//! are both simpler and closer to exact.

use crate::image::Image;

/// The TM_CCOEFF_NORMED response map: size `(W-tw+1) × (H-th+1)`, row-major.
/// Values match OpenCV within 1e-4 (OpenCV accumulates in different order).
pub struct ResponseMap {
    pub data: Vec<f32>,
    pub width: usize,
    pub height: usize,
}

impl ResponseMap {
    /// `(max_value, (x, y))` like `cv2.minMaxLoc`'s max side. First maximum
    /// in row-major scan order wins, matching OpenCV.
    pub fn max(&self) -> (f32, (usize, usize)) {
        let mut best = f32::NEG_INFINITY;
        let mut loc = (0, 0);
        for y in 0..self.height {
            for x in 0..self.width {
                let v = self.data[y * self.width + x];
                if v > best {
                    best = v;
                    loc = (x, y);
                }
            }
        }
        (best, loc)
    }
}

/// Zero-mean NCC of `templ` slid over `image` (both single-channel).
pub fn match_template_ccoeff_normed(image: &Image, templ: &Image) -> ResponseMap {
    assert_eq!(image.channels, 1);
    assert_eq!(templ.channels, 1);
    assert!(templ.width <= image.width && templ.height <= image.height);
    let (tw, th) = (templ.width, templ.height);
    let n = (tw * th) as f64;

    // Zero-mean template and its norm, once.
    let t_sum: f64 = templ.data.iter().map(|&v| v as f64).sum();
    let t_mean = t_sum / n;
    let t_zm: Vec<f64> = templ.data.iter().map(|&v| v as f64 - t_mean).collect();
    let t_norm2: f64 = t_zm.iter().map(|v| v * v).sum();

    let out_w = image.width - tw + 1;
    let out_h = image.height - th + 1;
    let mut out = vec![0.0f32; out_w * out_h];
    for oy in 0..out_h {
        for ox in 0..out_w {
            let mut i_sum = 0.0f64;
            let mut i_sum2 = 0.0f64;
            let mut cross = 0.0f64;
            for ty in 0..th {
                let irow = &image.row(oy + ty)[ox..ox + tw];
                let trow = &t_zm[ty * tw..(ty + 1) * tw];
                for (iv, tv) in irow.iter().zip(trow.iter()) {
                    let v = *iv as f64;
                    i_sum += v;
                    i_sum2 += v * v;
                    cross += v * tv;
                }
            }
            let i_var = i_sum2 - i_sum * i_sum / n;
            let denom2 = t_norm2 * i_var;
            // Degenerate windows (flat image or flat template): OpenCV's
            // normalization yields 0 there for any practical input.
            let v = if denom2 > f64::EPSILON {
                cross / denom2.sqrt()
            } else {
                0.0
            };
            out[oy * out_w + ox] = v as f32;
        }
    }
    ResponseMap {
        data: out,
        width: out_w,
        height: out_h,
    }
}

/// The engine's hot-path digit matcher: equal-size patches against a stack of
/// zero-mean/unit-norm template vectors (the Python `TemplateSet` matmul).
/// Returns per-template correlation for one patch.
pub fn correlate_normalized(patch: &[u8], templates_zm_unit: &[Vec<f64>]) -> Vec<f64> {
    let n = patch.len() as f64;
    let sum: f64 = patch.iter().map(|&v| v as f64).sum();
    let mean = sum / n;
    let mut norm2 = 0.0f64;
    let zm: Vec<f64> = patch
        .iter()
        .map(|&v| {
            let d = v as f64 - mean;
            norm2 += d * d;
            d
        })
        .collect();
    let norm = norm2.sqrt();
    templates_zm_unit
        .iter()
        .map(|t| {
            if norm <= f64::EPSILON {
                return 0.0;
            }
            let dot: f64 = t.iter().zip(zm.iter()).map(|(a, b)| a * b).sum();
            dot / norm
        })
        .collect()
}

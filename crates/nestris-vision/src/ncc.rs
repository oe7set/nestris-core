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
///
/// Window statistics (Σ I, Σ I²) come from integral images in O(1) per
/// position, and the cross term is an exact u8×u8 integer dot product, so
/// the per-position cost is one vectorizable MAC pass over the template —
/// this is the label-anchor hotspot at high source resolutions. All sums are
/// integer-exact; only the final normalization is float, matching the naive
/// formulation to f64 rounding.
pub fn match_template_ccoeff_normed(image: &Image, templ: &Image) -> ResponseMap {
    assert_eq!(image.channels, 1);
    assert_eq!(templ.channels, 1);
    assert!(templ.width <= image.width && templ.height <= image.height);
    let (tw, th) = (templ.width, templ.height);
    let (iw, ih) = (image.width, image.height);
    let n = (tw * th) as f64;

    // Template statistics (integer-exact).
    let t_sum: u64 = templ.data.iter().map(|&v| v as u64).sum();
    let t_mean = t_sum as f64 / n;
    let t_sum2: u64 = templ.data.iter().map(|&v| (v as u64) * (v as u64)).sum();
    let t_norm2 = t_sum2 as f64 - (t_sum as f64) * t_mean;

    // Integral images of I and I² ((iw+1) x (ih+1), zero top/left border).
    let stride = iw + 1;
    let mut ii = vec![0u64; stride * (ih + 1)];
    let mut ii2 = vec![0u64; stride * (ih + 1)];
    for y in 0..ih {
        let row = image.row(y);
        let mut run = 0u64;
        let mut run2 = 0u64;
        for x in 0..iw {
            let v = row[x] as u64;
            run += v;
            run2 += v * v;
            ii[(y + 1) * stride + x + 1] = ii[y * stride + x + 1] + run;
            ii2[(y + 1) * stride + x + 1] = ii2[y * stride + x + 1] + run2;
        }
    }
    let window_sum = |ox: usize, oy: usize, table: &[u64]| -> u64 {
        table[(oy + th) * stride + ox + tw] + table[oy * stride + ox]
            - table[oy * stride + ox + tw]
            - table[(oy + th) * stride + ox]
    };

    let out_w = iw - tw + 1;
    let out_h = ih - th + 1;
    let mut out = vec![0.0f32; out_w * out_h];
    // Each response row is an independent pure function of the inputs, so
    // the `parallel` row split is bit-exact.
    let compute_row = |oy: usize, out_row: &mut [f32]| {
        // `ox` also indexes the integral-image windows, not just `out_row`,
        // so enumerate() would not remove the arithmetic indexing.
        #[allow(clippy::needless_range_loop)]
        for ox in 0..out_w {
            // Cross term: exact integer dot product (row-wise u32, safe for
            // widths < 66k px at max u8 values).
            let mut cross: u64 = 0;
            for ty in 0..th {
                let irow = &image.row(oy + ty)[ox..ox + tw];
                let trow = &templ.data[ty * tw..(ty + 1) * tw];
                let mut row_acc: u32 = 0;
                for (&iv, &tv) in irow.iter().zip(trow.iter()) {
                    row_acc += iv as u32 * tv as u32;
                }
                cross += row_acc as u64;
            }
            let i_sum = window_sum(ox, oy, &ii) as f64;
            let i_sum2 = window_sum(ox, oy, &ii2) as f64;
            let cross_zm = cross as f64 - t_mean * i_sum;
            let i_var = i_sum2 - i_sum * i_sum / n;
            let denom2 = t_norm2 * i_var;
            // Degenerate windows (flat image or flat template): OpenCV's
            // normalization yields 0 there for any practical input.
            let v = if denom2 > f64::EPSILON {
                cross_zm / denom2.sqrt()
            } else {
                0.0
            };
            out_row[ox] = v as f32;
        }
    };
    #[cfg(feature = "parallel")]
    {
        use rayon::prelude::*;
        out.par_chunks_mut(out_w)
            .enumerate()
            .for_each(|(oy, row)| compute_row(oy, row));
    }
    #[cfg(not(feature = "parallel"))]
    for (oy, row) in out.chunks_mut(out_w).enumerate() {
        compute_row(oy, row);
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

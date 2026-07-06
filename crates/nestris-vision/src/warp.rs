//! `cv2.warpPerspective` / `cv2.remap` replication (bilinear, BORDER_CONSTANT 0).
//!
//! OpenCV silently substitutes INTER_LINEAR when warpPerspective is called
//! with INTER_AREA (confirmed empirically by the goldens:
//! `area_equals_linear: true`), so bilinear is the only path. It is
//! replicated at OpenCV's fixed-point precision: source coordinates are
//! quantized to 1/32 sub-pixel (INTER_BITS=5) and the 2×2 weights come from
//! the 2^15-scaled bilinear table with OpenCV's sum-correction.

use crate::homography::{Mat3, mat3_inv, project};
use crate::image::Image;

const INTER_BITS: i32 = 5;
const INTER_TAB_SIZE: i32 = 1 << INTER_BITS; // 32
const REMAP_COEF_BITS: i32 = 15;
const REMAP_COEF_SCALE: i32 = 1 << REMAP_COEF_BITS; // 32768

/// The 32×32 bilinear weight table (4 weights each), OpenCV `BilinearTab_i`
/// including its "weights must sum to exactly 2^15" correction.
fn bilinear_tab() -> &'static Vec<[i32; 4]> {
    use std::sync::OnceLock;
    static TAB: OnceLock<Vec<[i32; 4]>> = OnceLock::new();
    TAB.get_or_init(|| {
        let mut tab = Vec::with_capacity((INTER_TAB_SIZE * INTER_TAB_SIZE) as usize);
        let scale = 1.0 / INTER_TAB_SIZE as f32;
        for dy in 0..INTER_TAB_SIZE {
            for dx in 0..INTER_TAB_SIZE {
                let fx = dx as f32 * scale;
                let fy = dy as f32 * scale;
                let w = [
                    (1.0 - fx) * (1.0 - fy),
                    fx * (1.0 - fy),
                    (1.0 - fx) * fy,
                    fx * fy,
                ];
                let mut iw: [i32; 4] = std::array::from_fn(|k| {
                    (w[k] * REMAP_COEF_SCALE as f32).round_ties_even() as i32
                });
                let isum: i32 = iw.iter().sum();
                if isum != REMAP_COEF_SCALE {
                    // OpenCV's initInterTab2D correction: push the excess
                    // onto the max (deficit) or min (surplus) weight.
                    let diff = isum - REMAP_COEF_SCALE;
                    let (mut mk, mut bk) = (0usize, 0usize);
                    for k in 1..4 {
                        if iw[k] < iw[mk] {
                            mk = k;
                        } else if iw[k] > iw[bk] {
                            bk = k;
                        }
                    }
                    if diff < 0 {
                        iw[bk] -= diff;
                    } else {
                        iw[mk] -= diff;
                    }
                }
                tab.push(iw);
            }
        }
        tab
    })
}

/// Sample `src` at fixed-point coordinates: integer part (`sx`,`sy`), 5-bit
/// fraction packed as `fy*32+fx`. BORDER_CONSTANT 0 for out-of-image taps.
#[inline]
fn sample_fixed(src: &Image, sx: i64, sy: i64, frac: usize, out_px: &mut [u8]) {
    let tab = &bilinear_tab()[frac];
    let (w, h, c) = (src.width as i64, src.height as i64, src.channels);
    for (ch, out) in out_px.iter_mut().enumerate() {
        let mut acc = 0i64;
        for (k, &wt) in tab.iter().enumerate() {
            let x = sx + (k as i64 & 1);
            let y = sy + (k as i64 >> 1);
            let v = if x >= 0 && y >= 0 && x < w && y < h {
                src.data[((y * w + x) as usize) * c + ch] as i64
            } else {
                0
            };
            acc += wt as i64 * v;
        }
        *out = ((acc + (1 << (REMAP_COEF_BITS - 1))) >> REMAP_COEF_BITS).clamp(0, 255) as u8;
    }
}

/// `cv2.warpPerspective(src, m, (out_w, out_h), flags=INTER_LINEAR)`.
///
/// `m` maps SOURCE→DEST (OpenCV convention); it is inverted internally.
pub fn warp_perspective(src: &Image, m: &Mat3, out_w: usize, out_h: usize) -> Image {
    let inv = mat3_inv(m).expect("warp matrix not invertible");
    let mut out = Image::new(out_w, out_h, src.channels);
    let mut px = vec![0u8; src.channels];
    for dy in 0..out_h {
        for dx in 0..out_w {
            let (fx, fy) = project(&inv, dx as f64, dy as f64);
            // OpenCV computes X = saturate_cast<int>(x*INTER_TAB_SIZE) with
            // round-half-to-even, then splits integer/fraction.
            let xi = (fx * INTER_TAB_SIZE as f64).round_ties_even() as i64;
            let yi = (fy * INTER_TAB_SIZE as f64).round_ties_even() as i64;
            let sx = xi >> INTER_BITS;
            let sy = yi >> INTER_BITS;
            let frac = (((yi & (INTER_TAB_SIZE as i64 - 1)) << INTER_BITS)
                + (xi & (INTER_TAB_SIZE as i64 - 1))) as usize;
            let base = (dy * out_w + dx) * src.channels;
            sample_fixed(src, sx, sy, frac, &mut px);
            out.data[base..base + src.channels].copy_from_slice(&px);
        }
    }
    out
}

/// `cv2.remap(src, map_x, map_y, INTER_LINEAR)` with CV_32FC1 maps
/// (used by the barrel-undistort pre-pass, mirroring the Python two-pass
/// remap-then-warp rectification exactly).
pub fn remap_bilinear(
    src: &Image,
    map_x: &[f32],
    map_y: &[f32],
    out_w: usize,
    out_h: usize,
) -> Image {
    assert_eq!(map_x.len(), out_w * out_h);
    assert_eq!(map_y.len(), out_w * out_h);
    let ch = src.channels;
    let mut out = Image::new(out_w, out_h, ch);
    // Each output row is an independent pure function of the maps and the
    // source, so the row split under the `parallel` feature is bit-exact
    // (the undistort pre-pass runs this on full frames).
    let row_fn = |y: usize, out_row: &mut [u8]| {
        let mut px = vec![0u8; ch];
        for dx in 0..out_w {
            let i = y * out_w + dx;
            let xi = (map_x[i] as f64 * INTER_TAB_SIZE as f64).round_ties_even() as i64;
            let yi = (map_y[i] as f64 * INTER_TAB_SIZE as f64).round_ties_even() as i64;
            let sx = xi >> INTER_BITS;
            let sy = yi >> INTER_BITS;
            let frac = (((yi & (INTER_TAB_SIZE as i64 - 1)) << INTER_BITS)
                + (xi & (INTER_TAB_SIZE as i64 - 1))) as usize;
            sample_fixed(src, sx, sy, frac, &mut px);
            out_row[dx * ch..(dx + 1) * ch].copy_from_slice(&px);
        }
    };
    #[cfg(feature = "parallel")]
    if out_w * out_h >= crate::PAR_MIN_PIXELS {
        use rayon::prelude::*;
        out.data
            .par_chunks_mut(out_w * ch)
            .enumerate()
            .for_each(|(y, out_row)| row_fn(y, out_row));
        return out;
    }
    for y in 0..out_h {
        let out_row = &mut out.data[y * out_w * ch..(y + 1) * out_w * ch];
        row_fn(y, out_row);
    }
    out
}

/// A precomputed dst→src sampling map for a fixed homography and output size:
/// one table lookup + 4 taps per pixel per frame. Equivalent to
/// [`warp_perspective`] output byte-for-byte, amortizing the projection.
pub struct WarpMap {
    /// Per dst pixel: (sx, sy, frac) as in [`sample_fixed`].
    entries: Vec<(i32, i32, u16)>,
    pub out_w: usize,
    pub out_h: usize,
}

impl WarpMap {
    pub fn new(m: &Mat3, out_w: usize, out_h: usize) -> Option<WarpMap> {
        let inv = mat3_inv(m)?;
        let mut entries = Vec::with_capacity(out_w * out_h);
        for dy in 0..out_h {
            for dx in 0..out_w {
                let (fx, fy) = project(&inv, dx as f64, dy as f64);
                let xi = (fx * INTER_TAB_SIZE as f64).round_ties_even() as i64;
                let yi = (fy * INTER_TAB_SIZE as f64).round_ties_even() as i64;
                let sx = (xi >> INTER_BITS).clamp(i32::MIN as i64, i32::MAX as i64) as i32;
                let sy = (yi >> INTER_BITS).clamp(i32::MIN as i64, i32::MAX as i64) as i32;
                let frac = (((yi & (INTER_TAB_SIZE as i64 - 1)) << INTER_BITS)
                    + (xi & (INTER_TAB_SIZE as i64 - 1))) as u16;
                entries.push((sx, sy, frac));
            }
        }
        Some(WarpMap {
            entries,
            out_w,
            out_h,
        })
    }

    /// Warp into `out` (must be `out_w × out_h` with `src.channels`).
    /// With the `parallel` feature the output rows are computed on the
    /// rayon pool — a bit-exact split (each row is an independent pure
    /// function of the map and the source).
    pub fn apply_into(&self, src: &Image, out: &mut Image) {
        debug_assert_eq!(out.width, self.out_w);
        debug_assert_eq!(out.height, self.out_h);
        debug_assert_eq!(out.channels, src.channels);
        let ch = src.channels;
        let row_len = self.out_w * ch;
        let process_row = |entries: &[(i32, i32, u16)], out_row: &mut [u8]| {
            let mut px = vec![0u8; ch];
            for (k, &(sx, sy, frac)) in entries.iter().enumerate() {
                sample_fixed(src, sx as i64, sy as i64, frac as usize, &mut px);
                out_row[k * ch..(k + 1) * ch].copy_from_slice(&px);
            }
        };
        #[cfg(feature = "parallel")]
        {
            use rayon::prelude::*;
            self.entries
                .par_chunks(self.out_w)
                .zip(out.data.par_chunks_mut(row_len))
                .for_each(|(entries, out_row)| process_row(entries, out_row));
        }
        #[cfg(not(feature = "parallel"))]
        for (entries, out_row) in self
            .entries
            .chunks(self.out_w)
            .zip(out.data.chunks_mut(row_len))
        {
            process_row(entries, out_row);
        }
    }

    pub fn apply(&self, src: &Image) -> Image {
        let mut out = Image::new(self.out_w, self.out_h, src.channels);
        self.apply_into(src, &mut out);
        out
    }
}

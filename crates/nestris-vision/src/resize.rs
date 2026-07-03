//! `cv2.resize(..., INTER_AREA)` replication.
//!
//! OpenCV picks among three algorithms behind INTER_AREA:
//! * both axes downscaling → true fractional area averaging,
//! * otherwise → the generic bilinear machinery with the special
//!   *area-mode* coordinate mapping (`sx = floor(dx*scale)`,
//!   `fx = (dx+1) - (sx+1)/inv_scale` folded to `[0,1)`), evaluated in
//!   8-bit fixed point with 2048-scaled coefficients and OpenCV's exact
//!   `>>16` two-stage descale.
//!
//! The engine only calls INTER_AREA (label templates scale both ways
//! depending on capture resolution), so plain INTER_LINEAR is not exposed.

use crate::image::Image;

/// `cv2.resize(src, (out_w, out_h), interpolation=INTER_AREA)`.
pub fn resize_area(src: &Image, out_w: usize, out_h: usize) -> Image {
    assert_eq!(src.channels, 1, "engine only resizes grayscale templates");
    assert!(out_w > 0 && out_h > 0);
    if out_w <= src.width && out_h <= src.height {
        resize_area_down(src, out_w, out_h)
    } else {
        resize_area_bilinear(src, out_w, out_h)
    }
}

/// True area averaging (both axes shrink), f64 accumulation with OpenCV's
/// round-half-to-even saturate cast.
fn resize_area_down(src: &Image, out_w: usize, out_h: usize) -> Image {
    let scale_x = src.width as f64 / out_w as f64;
    let scale_y = src.height as f64 / out_h as f64;
    let mut out = Image::new(out_w, out_h, 1);
    for dy in 0..out_h {
        let sy0 = dy as f64 * scale_y;
        let sy1 = (dy as f64 + 1.0) * scale_y;
        for dx in 0..out_w {
            let sx0 = dx as f64 * scale_x;
            let sx1 = (dx as f64 + 1.0) * scale_x;
            let mut acc = 0.0f64;
            let mut area = 0.0f64;
            let y_start = sy0.floor() as usize;
            let y_end = (sy1.ceil() as usize).min(src.height);
            let x_start = sx0.floor() as usize;
            let x_end = (sx1.ceil() as usize).min(src.width);
            for y in y_start..y_end {
                let wy = (sy1.min(y as f64 + 1.0) - sy0.max(y as f64)).max(0.0);
                if wy <= 0.0 {
                    continue;
                }
                let row = src.row(y);
                for (x, &v) in row.iter().enumerate().take(x_end).skip(x_start) {
                    let wx = (sx1.min(x as f64 + 1.0) - sx0.max(x as f64)).max(0.0);
                    let w = wx * wy;
                    acc += v as f64 * w;
                    area += w;
                }
            }
            let v = acc / area;
            out.data[dy * out_w + dx] = v.round_ties_even().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

const COEF_SCALE: f32 = 2048.0; // 1 << INTER_RESIZE_COEF_BITS

/// Per-axis area-mode source index + fixed-point coefficient pair.
fn area_mode_coeffs(dst_len: usize, src_len: usize) -> Vec<(usize, i32, i32)> {
    let scale = src_len as f64 / dst_len as f64;
    let inv_scale = dst_len as f64 / src_len as f64;
    (0..dst_len)
        .map(|d| {
            let mut s = (d as f64 * scale).floor() as isize;
            let mut f = ((d as f64 + 1.0) - (s as f64 + 1.0) * inv_scale) as f32;
            f = if f <= 0.0 { 0.0 } else { f - f.floor() };
            if s < 0 {
                f = 0.0;
                s = 0;
            }
            if s >= src_len as isize - 1 {
                f = 0.0;
                s = src_len as isize - 1;
            }
            let a1 = (f * COEF_SCALE).round_ties_even() as i32;
            let a0 = ((1.0 - f) * COEF_SCALE).round_ties_even() as i32;
            (s as usize, a0, a1)
        })
        .collect()
}

/// The bilinear path INTER_AREA falls back to when either axis grows,
/// replicating OpenCV's 8u fixed-point pipeline exactly:
/// `rows[dx] = S[sx]*a0 + S[sx+1]*a1` (int), then
/// `dst = ((b0*(row0>>4))>>16) + ((b1*(row1>>4))>>16) + 2) >> 2`.
fn resize_area_bilinear(src: &Image, out_w: usize, out_h: usize) -> Image {
    let xc = area_mode_coeffs(out_w, src.width);
    let yc = area_mode_coeffs(out_h, src.height);
    let mut out = Image::new(out_w, out_h, 1);
    let mut row0 = vec![0i32; out_w];
    let mut row1 = vec![0i32; out_w];
    for (dy, &(sy, b0, b1)) in yc.iter().enumerate() {
        let sy1 = (sy + 1).min(src.height - 1);
        let s0 = src.row(sy);
        let s1 = src.row(sy1);
        for (dx, &(sx, a0, a1)) in xc.iter().enumerate() {
            let sx1 = (sx + 1).min(src.width - 1);
            row0[dx] = s0[sx] as i32 * a0 + s0[sx1] as i32 * a1;
            row1[dx] = s1[sx] as i32 * a0 + s1[sx1] as i32 * a1;
        }
        for dx in 0..out_w {
            let v = ((b0 * (row0[dx] >> 4)) >> 16) + ((b1 * (row1[dx] >> 4)) >> 16);
            out.data[dy * out_w + dx] = ((v + 2) >> 2).clamp(0, 255) as u8;
        }
    }
    out
}

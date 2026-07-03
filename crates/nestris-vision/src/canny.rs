//! Canny edge detection matching `cv2.Canny(gray, lo, hi)` defaults:
//! 3×3 Sobel with replicate borders, L1 gradient magnitude, OpenCV's
//! fixed-point sector NMS (TG22 = tan(22.5°) « 15), stack-based hysteresis.
//!
//! Consumed only by the barrel-undistort bow estimator, whose cross-language
//! contract is decision-level (same k1 pick, same engage/skip choice), so no
//! per-pixel golden is enforced — but the implementation follows OpenCV
//! closely enough that edge maps agree almost everywhere.

use crate::image::Image;

/// 3×3 Sobel derivatives with BORDER_REPLICATE, as `cv2.Canny` uses.
fn sobel3(src: &Image) -> (Vec<i32>, Vec<i32>) {
    let (w, h) = (src.width as isize, src.height as isize);
    let at = |x: isize, y: isize| -> i32 {
        let cx = x.clamp(0, w - 1);
        let cy = y.clamp(0, h - 1);
        src.data[(cy * w + cx) as usize] as i32
    };
    let mut dx = vec![0i32; (w * h) as usize];
    let mut dy = vec![0i32; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let idx = (y * w + x) as usize;
            dx[idx] = (at(x + 1, y - 1) + 2 * at(x + 1, y) + at(x + 1, y + 1))
                - (at(x - 1, y - 1) + 2 * at(x - 1, y) + at(x - 1, y + 1));
            dy[idx] = (at(x - 1, y + 1) + 2 * at(x, y + 1) + at(x + 1, y + 1))
                - (at(x - 1, y - 1) + 2 * at(x, y - 1) + at(x + 1, y - 1));
        }
    }
    (dx, dy)
}

/// `cv2.Canny(src, low, high)` (L2gradient=false → L1 magnitude).
/// Output: 0/255 edge map.
pub fn canny(src: &Image, low: f64, high: f64) -> Image {
    assert_eq!(src.channels, 1);
    let (w, h) = (src.width, src.height);
    let (dx, dy) = sobel3(src);
    let mag: Vec<i32> = dx
        .iter()
        .zip(dy.iter())
        .map(|(&a, &b)| a.abs() + b.abs())
        .collect();
    let low = low.min(high).round() as i32;
    let high = high.max(low as f64) as i32;

    // NMS with OpenCV's sector arithmetic: TG22 = tan(22.5°) in Q15.
    const TG22: i64 = 13573;
    // 0 = suppressed, 1 = weak candidate, 2 = strong edge.
    let mut state = vec![0u8; w * h];
    let mut stack: Vec<usize> = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let idx = y * w + x;
            let m = mag[idx];
            if m <= low {
                continue;
            }
            let (gx, gy) = (dx[idx] as i64, dy[idx] as i64);
            let ax = gx.abs();
            let ay = gy.abs() << 15;
            let tg22x = ax * TG22;
            let keep;
            let neighbor = |ox: isize, oy: isize| -> i32 {
                let nx = x as isize + ox;
                let ny = y as isize + oy;
                if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
                    0
                } else {
                    mag[(ny as usize) * w + nx as usize]
                }
            };
            if ay < tg22x {
                // Near-horizontal gradient: compare left/right.
                keep = m > neighbor(-1, 0) && m >= neighbor(1, 0);
            } else {
                let tg67x = tg22x + (ax << 16);
                if ay > tg67x {
                    // Near-vertical: compare up/down.
                    keep = m > neighbor(0, -1) && m >= neighbor(0, 1);
                } else {
                    // Diagonal: sign of gx*gy picks the diagonal.
                    let s = if (gx ^ gy) < 0 { -1isize } else { 1 };
                    keep = m > neighbor(-s, -1) && m >= neighbor(s, 1);
                }
            }
            if keep {
                if m > high {
                    state[idx] = 2;
                    stack.push(idx);
                } else {
                    state[idx] = 1;
                }
            }
        }
    }
    // Hysteresis: grow strong edges into 8-connected weak candidates.
    while let Some(idx) = stack.pop() {
        let (x, y) = (idx % w, idx / w);
        for oy in -1isize..=1 {
            for ox in -1isize..=1 {
                if ox == 0 && oy == 0 {
                    continue;
                }
                let nx = x as isize + ox;
                let ny = y as isize + oy;
                if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
                    continue;
                }
                let n = (ny as usize) * w + nx as usize;
                if state[n] == 1 {
                    state[n] = 2;
                    stack.push(n);
                }
            }
        }
    }
    let mut out = Image::new(w, h, 1);
    for (o, s) in out.data.iter_mut().zip(state.iter()) {
        *o = if *s == 2 { 255 } else { 0 };
    }
    out
}

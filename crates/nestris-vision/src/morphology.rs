//! Grayscale/binary morphology replicating `cv2.erode/dilate/morphologyEx`
//! with OpenCV's default border semantics (constant ±∞, i.e. out-of-image
//! taps never win the min/max).

use crate::image::Image;

/// A structuring element as a dense 0/1 mask with its anchor at the center.
///
/// OpenCV's `getStructuringElement(MORPH_ELLIPSE, ...)` rasterization is
/// quirky, so the exact masks are captured in the goldens
/// (`testdata/cv/morph/kernels.json`) and the two sizes the engine uses are
/// hard-coded here, verified byte-for-byte by the golden test.
#[derive(Clone, Debug)]
pub struct Kernel {
    pub mask: Vec<u8>,
    pub width: usize,
    pub height: usize,
}

impl Kernel {
    pub fn new(mask: Vec<u8>, width: usize, height: usize) -> Self {
        assert_eq!(mask.len(), width * height);
        Self {
            mask,
            width,
            height,
        }
    }

    /// `cv2.getStructuringElement(MORPH_ELLIPSE, (3, 3))` — a plus shape.
    pub fn ellipse3() -> Self {
        Self::new(vec![0, 1, 0, 1, 1, 1, 0, 1, 0], 3, 3)
    }

    /// `cv2.getStructuringElement(MORPH_ELLIPSE, (5, 5))`.
    ///
    /// OpenCV's ellipse rasterization fills full rows within the computed
    /// span; for 5×5 the corners are cut only on the first/last row.
    pub fn ellipse5() -> Self {
        #[rustfmt::skip]
        let mask = vec![
            0, 0, 1, 0, 0,
            1, 1, 1, 1, 1,
            1, 1, 1, 1, 1,
            1, 1, 1, 1, 1,
            0, 0, 1, 0, 0,
        ];
        Self::new(mask, 5, 5)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Erode,
    Dilate,
}

#[inline]
fn fold(op: Op, a: u8, b: u8) -> u8 {
    match op {
        Op::Erode => a.min(b),
        Op::Dilate => a.max(b),
    }
}

/// Horizontal running min/max with window radius `r` (window clamped to the
/// image, which equals OpenCV's constant ±∞ border: outside taps never win).
/// Output rows are independent pure functions of the source, so the row
/// split under the `parallel` feature is bit-exact.
fn pass_h(src: &Image, r: usize, op: Op) -> Image {
    let (w, h) = (src.width, src.height);
    let mut out = Image::new(w, h, 1);
    let row_fn = |y: usize, dst: &mut [u8]| {
        let row = &src.data[y * w..(y + 1) * w];
        for (x, d) in dst.iter_mut().enumerate() {
            let x0 = x.saturating_sub(r);
            let x1 = (x + r + 1).min(w);
            let mut acc = row[x0];
            for &v in &row[x0 + 1..x1] {
                acc = fold(op, acc, v);
            }
            *d = acc;
        }
    };
    #[cfg(feature = "parallel")]
    if w * h >= crate::PAR_MIN_PIXELS {
        use rayon::prelude::*;
        out.data
            .par_chunks_mut(w)
            .enumerate()
            .for_each(|(y, dst)| row_fn(y, dst));
        return out;
    }
    for y in 0..h {
        let dst = &mut out.data[y * w..(y + 1) * w];
        row_fn(y, dst);
    }
    out
}

/// Vertical running min/max with window radius `r` (row-parallel like
/// [`pass_h`]: each output row folds a fixed source-row window).
fn pass_v(src: &Image, r: usize, op: Op) -> Image {
    let (w, h) = (src.width, src.height);
    let mut out = Image::new(w, h, 1);
    let row_fn = |y: usize, dst: &mut [u8]| {
        let y0 = y.saturating_sub(r);
        let y1 = (y + r + 1).min(h);
        dst.copy_from_slice(&src.data[y0 * w..(y0 + 1) * w]);
        for yy in y0 + 1..y1 {
            let row = &src.data[yy * w..(yy + 1) * w];
            for (d, &v) in dst.iter_mut().zip(row.iter()) {
                *d = fold(op, *d, v);
            }
        }
    };
    #[cfg(feature = "parallel")]
    if w * h >= crate::PAR_MIN_PIXELS {
        use rayon::prelude::*;
        out.data
            .par_chunks_mut(w)
            .enumerate()
            .for_each(|(y, dst)| row_fn(y, dst));
        return out;
    }
    for y in 0..h {
        let dst = &mut out.data[y * w..(y + 1) * w];
        row_fn(y, dst);
    }
    out
}

/// Elementwise min/max of two equal-size images.
fn combine(a: &Image, b: &Image, op: Op) -> Image {
    let (w, h) = (a.width, a.height);
    let mut out = Image::new(w, h, 1);
    #[cfg(feature = "parallel")]
    if w * h >= crate::PAR_MIN_PIXELS {
        use rayon::prelude::*;
        out.data
            .par_chunks_mut(w)
            .zip(a.data.par_chunks(w).zip(b.data.par_chunks(w)))
            .for_each(|(d_row, (a_row, b_row))| {
                for ((d, &x), &y) in d_row.iter_mut().zip(a_row).zip(b_row) {
                    *d = fold(op, x, y);
                }
            });
        return out;
    }
    for ((d, &x), &y) in out.data.iter_mut().zip(a.data.iter()).zip(b.data.iter()) {
        *d = fold(op, x, y);
    }
    out
}

fn morph_once(src: &Image, kernel: &Kernel, op: Op) -> Image {
    assert_eq!(src.channels, 1);
    // Separable fast paths for the two kernels the engine uses (byte-exact,
    // enforced by the morphology golden tests):
    //   plus (ellipse 3x3)  = union of a 1x3 and a 3x1 window
    //   ellipse 5x5         = union of a 5x3 box and a 5x1 column
    // min/max over a union of windows = fold of the per-window results, so
    // each becomes cache-friendly separable passes instead of a per-pixel
    // masked kernel scan (the acquisition-path hotspot at full resolution).
    if kernel.mask == Kernel::ellipse3().mask {
        return combine(&pass_h(src, 1, op), &pass_v(src, 1, op), op);
    }
    if kernel.mask == Kernel::ellipse5().mask {
        let box5x3 = pass_v(&pass_h(src, 2, op), 1, op);
        return combine(&box5x3, &pass_v(src, 2, op), op);
    }

    // Generic reference path for arbitrary kernels.
    let (w, h) = (src.width as isize, src.height as isize);
    let ax = (kernel.width / 2) as isize;
    let ay = (kernel.height / 2) as isize;
    let mut out = Image::new(src.width, src.height, 1);
    for y in 0..h {
        for x in 0..w {
            let mut acc: i32 = match op {
                Op::Erode => 255,
                Op::Dilate => 0,
            };
            for ky in 0..kernel.height as isize {
                for kx in 0..kernel.width as isize {
                    if kernel.mask[(ky * kernel.width as isize + kx) as usize] == 0 {
                        continue;
                    }
                    let sx = x + kx - ax;
                    let sy = y + ky - ay;
                    if sx < 0 || sy < 0 || sx >= w || sy >= h {
                        // Border constant ±∞: never wins against in-image taps.
                        continue;
                    }
                    let v = src.data[(sy * w + sx) as usize] as i32;
                    acc = match op {
                        Op::Erode => acc.min(v),
                        Op::Dilate => acc.max(v),
                    };
                }
            }
            out.data[(y * w + x) as usize] = acc as u8;
        }
    }
    out
}

fn repeat(src: &Image, kernel: &Kernel, op: Op, iterations: usize) -> Image {
    let mut img = morph_once(src, kernel, op);
    for _ in 1..iterations {
        img = morph_once(&img, kernel, op);
    }
    img
}

/// `cv2.erode(src, kernel, iterations=n)`.
pub fn erode(src: &Image, kernel: &Kernel, iterations: usize) -> Image {
    repeat(src, kernel, Op::Erode, iterations.max(1))
}

/// `cv2.dilate(src, kernel, iterations=n)`.
pub fn dilate(src: &Image, kernel: &Kernel, iterations: usize) -> Image {
    repeat(src, kernel, Op::Dilate, iterations.max(1))
}

/// `cv2.morphologyEx(src, MORPH_OPEN, kernel, iterations=n)`:
/// n erosions followed by n dilations (OpenCV semantics, not n×(erode,dilate)).
pub fn open(src: &Image, kernel: &Kernel, iterations: usize) -> Image {
    let n = iterations.max(1);
    dilate(&erode(src, kernel, n), kernel, n)
}

/// `cv2.morphologyEx(src, MORPH_CLOSE, kernel, iterations=n)`:
/// n dilations followed by n erosions.
pub fn close(src: &Image, kernel: &Kernel, iterations: usize) -> Image {
    let n = iterations.max(1);
    erode(&dilate(src, kernel, n), kernel, n)
}

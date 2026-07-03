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

fn morph_once(src: &Image, kernel: &Kernel, op: Op) -> Image {
    assert_eq!(src.channels, 1);
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

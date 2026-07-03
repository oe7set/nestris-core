//! Minimal owned image buffer for interleaved `u8` pixel data.
//!
//! Deliberately not an ndarray: every hot routine in this crate is a
//! hand-written loop over interleaved bytes, and OpenCV-exact fixed-point
//! arithmetic wants explicit integer indexing.

/// An owned H×W×C interleaved `u8` image. Rows are contiguous (no padding).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub data: Vec<u8>,
    pub width: usize,
    pub height: usize,
    pub channels: usize,
}

impl Image {
    /// A zero-filled image.
    pub fn new(width: usize, height: usize, channels: usize) -> Self {
        Self {
            data: vec![0; width * height * channels],
            width,
            height,
            channels,
        }
    }

    /// Wrap an existing buffer; `data.len()` must equal `w * h * c`.
    pub fn from_vec(data: Vec<u8>, width: usize, height: usize, channels: usize) -> Self {
        assert_eq!(
            data.len(),
            width * height * channels,
            "buffer size mismatch"
        );
        Self {
            data,
            width,
            height,
            channels,
        }
    }

    #[inline]
    pub fn row(&self, y: usize) -> &[u8] {
        let stride = self.width * self.channels;
        &self.data[y * stride..(y + 1) * stride]
    }

    #[inline]
    pub fn pixel(&self, x: usize, y: usize) -> &[u8] {
        let idx = (y * self.width + x) * self.channels;
        &self.data[idx..idx + self.channels]
    }

    #[inline]
    pub fn pixel_mut(&mut self, x: usize, y: usize) -> &mut [u8] {
        let idx = (y * self.width + x) * self.channels;
        &mut self.data[idx..idx + self.channels]
    }

    /// Crop a rectangle (clamped to bounds) into a new owned image.
    pub fn crop(&self, x: usize, y: usize, w: usize, h: usize) -> Image {
        let x1 = (x + w).min(self.width);
        let y1 = (y + h).min(self.height);
        let (w, h) = (x1.saturating_sub(x), y1.saturating_sub(y));
        let mut out = Image::new(w, h, self.channels);
        for row in 0..h {
            let src = (y + row) * self.width + x;
            let dst = row * w;
            out.data[dst * self.channels..(dst + w) * self.channels]
                .copy_from_slice(&self.data[src * self.channels..(src + w) * self.channels]);
        }
        out
    }
}

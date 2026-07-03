//! Input frame descriptor for the sans-io processor.

use nestris_vision::Image;

/// One captured frame: BGR pixels + sequencing metadata.
pub struct Frame {
    pub image: Image,
    pub seq: i64,
    pub ts: f64,
    /// Source-signalled discontinuity (seek/switch): temporal state resets.
    pub discontinuity: bool,
}

impl Frame {
    pub fn new(image: Image, seq: i64, ts: f64) -> Self {
        Self {
            image,
            seq,
            ts,
            discontinuity: false,
        }
    }

    /// Build a BGR frame from RGBA bytes (the wasm `ImageData` path).
    pub fn from_rgba(data: &[u8], width: usize, height: usize, seq: i64, ts: f64) -> Self {
        let mut image = Image::new(width, height, 3);
        for (dst, src) in image.data.chunks_exact_mut(3).zip(data.chunks_exact(4)) {
            dst[0] = src[2];
            dst[1] = src[1];
            dst[2] = src[0];
        }
        Self::new(image, seq, ts)
    }
}

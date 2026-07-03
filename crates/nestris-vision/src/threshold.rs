//! Otsu thresholding, replicating `cv2.threshold(..., THRESH_BINARY + THRESH_OTSU)`.

use crate::image::Image;

/// Otsu threshold value for a grayscale image, exactly as OpenCV computes it
/// (`getThreshVal_Otsu_8u`), including its accumulation order and the
/// strictly-greater comparison that keeps the *first* maximum.
pub fn otsu_threshold(src: &Image) -> f64 {
    assert_eq!(src.channels, 1);
    let mut hist = [0u32; 256];
    for &v in &src.data {
        hist[v as usize] += 1;
    }
    let scale = 1.0 / (src.width * src.height) as f64;
    let mut mu = 0.0f64;
    for (i, &h) in hist.iter().enumerate() {
        mu += i as f64 * h as f64;
    }
    mu *= scale;

    let mut mu1 = 0.0f64;
    let mut q1 = 0.0f64;
    let mut max_sigma = 0.0f64;
    let mut max_val = 0.0f64;
    for (i, &h) in hist.iter().enumerate() {
        let p_i = h as f64 * scale;
        // OpenCV multiplies mu1 by the *previous* q1 before updating; the
        // early `continue` below deliberately leaves mu1 in that state.
        mu1 *= q1;
        q1 += p_i;
        let q2 = 1.0 - q1;
        const FLT_EPSILON: f64 = f32::EPSILON as f64;
        if q1.min(q2) < FLT_EPSILON || q1.max(q2) > 1.0 - FLT_EPSILON {
            continue;
        }
        mu1 = (mu1 + i as f64 * p_i) / q1;
        let mu2 = (mu - q1 * mu1) / q2;
        let sigma = q1 * q2 * (mu1 - mu2) * (mu1 - mu2);
        if sigma > max_sigma {
            max_sigma = sigma;
            max_val = i as f64;
        }
    }
    max_val
}

/// `cv2.threshold(src, 0, max_value, THRESH_BINARY | THRESH_OTSU)`:
/// returns `(threshold, binary)` where `binary = src > threshold ? max_value : 0`.
pub fn threshold_binary_otsu(src: &Image, max_value: u8) -> (f64, Image) {
    let thresh = otsu_threshold(src);
    let ithresh = thresh.floor() as i32;
    let mut out = Image::new(src.width, src.height, 1);
    for (dst, &v) in out.data.iter_mut().zip(src.data.iter()) {
        *dst = if (v as i32) > ithresh { max_value } else { 0 };
    }
    (thresh, out)
}

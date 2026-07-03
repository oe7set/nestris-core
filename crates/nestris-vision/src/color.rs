//! OpenCV-exact 8-bit color conversions: BGR→GRAY, BGR→HSV, BGR↔Lab.
//!
//! The engine's occupancy/color/menu thresholds were tuned against
//! `cv2.cvtColor`, so these replicate OpenCV's *fixed-point implementations*
//! (not the idealized documentation formulas). Golden tests in
//! `tests/goldens.rs` assert byte-exactness (gray) / ≤1 LSB (Lab, HSV)
//! against captured cv2 outputs.

use crate::image::Image;

// ---------------------------------------------------------------------------
// BGR -> GRAY
// ---------------------------------------------------------------------------

// OpenCV 4.x RGB2Gray<uchar>: BY15/GY15/RY15 coefficients at shift 15 with
// round-half-up descale (golden-verified; the older 4899/9617/1868 @14
// variant differs by 1 LSB on ~0.1% of inputs).
const RY15: u32 = 9798;
const GY15: u32 = 19235;
const BY15: u32 = 3735;

/// One BGR pixel to luma, exactly like `cv2.cvtColor(..., COLOR_BGR2GRAY)`.
#[inline]
pub fn bgr_pixel_to_gray(b: u8, g: u8, r: u8) -> u8 {
    let sum = b as u32 * BY15 + g as u32 * GY15 + r as u32 * RY15;
    ((sum + (1 << 14)) >> 15) as u8
}

/// BGR image to single-channel luma.
pub fn bgr_to_gray(src: &Image) -> Image {
    assert_eq!(src.channels, 3);
    let mut out = Image::new(src.width, src.height, 1);
    for (dst, px) in out.data.iter_mut().zip(src.data.chunks_exact(3)) {
        *dst = bgr_pixel_to_gray(px[0], px[1], px[2]);
    }
    out
}

// ---------------------------------------------------------------------------
// BGR -> HSV (H in 0..180)
// ---------------------------------------------------------------------------

const HSV_SHIFT: i32 = 12;

struct HsvTables {
    sdiv: [i32; 256],
    hdiv: [i32; 256],
}

fn hsv_tables() -> &'static HsvTables {
    use std::sync::OnceLock;
    static TABLES: OnceLock<HsvTables> = OnceLock::new();
    TABLES.get_or_init(|| {
        let mut t = HsvTables {
            sdiv: [0; 256],
            hdiv: [0; 256],
        };
        for i in 1..256usize {
            t.sdiv[i] = (((255 << HSV_SHIFT) as f64) / i as f64).round() as i32;
            t.hdiv[i] = (((180 << HSV_SHIFT) as f64) / (6.0 * i as f64)).round() as i32;
        }
        t
    })
}

/// One BGR pixel to HSV, exactly like `cv2.cvtColor(..., COLOR_BGR2HSV)`.
#[inline]
pub fn bgr_pixel_to_hsv(b: u8, g: u8, r: u8) -> (u8, u8, u8) {
    let t = hsv_tables();
    let (b, g, r) = (b as i32, g as i32, r as i32);
    let v = b.max(g).max(r);
    let vmin = b.min(g).min(r);
    let diff = v - vmin;
    let s = (diff * t.sdiv[v as usize] + (1 << (HSV_SHIFT - 1))) >> HSV_SHIFT;
    let vr = if v == r { -1i32 } else { 0 };
    let vg = if v == g { -1i32 } else { 0 };
    // Branchless selection mirroring OpenCV's masks: prefer R, then G, then B.
    let h_num = (vr & (g - b)) | (!vr & ((vg & (b - r + 2 * diff)) | (!vg & (r - g + 4 * diff))));
    let mut h = (h_num * t.hdiv[diff as usize] + (1 << (HSV_SHIFT - 1))) >> HSV_SHIFT;
    if h < 0 {
        h += 180;
    }
    (h as u8, s as u8, v as u8)
}

/// BGR image to 3-channel HSV.
pub fn bgr_to_hsv(src: &Image) -> Image {
    assert_eq!(src.channels, 3);
    let mut out = Image::new(src.width, src.height, 3);
    for (dst, px) in out.data.chunks_exact_mut(3).zip(src.data.chunks_exact(3)) {
        let (h, s, v) = bgr_pixel_to_hsv(px[0], px[1], px[2]);
        dst[0] = h;
        dst[1] = s;
        dst[2] = v;
    }
    out
}

// ---------------------------------------------------------------------------
// BGR <-> Lab (8-bit scaled: L*255/100, a/b offset +128)
// ---------------------------------------------------------------------------

const LAB_SHIFT: i32 = 12;
const GAMMA_SHIFT: i32 = 3;
const LAB_SHIFT2: i32 = LAB_SHIFT + GAMMA_SHIFT;
const GAMMA_TAB_MAX: usize = 255 * (1 << GAMMA_SHIFT); // 2040
const CBRT_TAB_SIZE: usize = 3 * GAMMA_TAB_MAX / 2; // 3060 (OpenCV LAB_CBRT_TAB_SIZE_B)

/// sRGB linearization (encode -> linear), the standard IEC 61966-2-1 curve.
#[inline]
fn srgb_inv_gamma(x: f64) -> f64 {
    if x <= 0.04045 {
        x / 12.92
    } else {
        ((x + 0.055) / 1.055).powf(2.4)
    }
}

/// sRGB companding (linear -> encode).
#[inline]
fn srgb_gamma(x: f64) -> f64 {
    if x <= 0.0031308 {
        12.92 * x
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    }
}

const SRGB2XYZ_D65: [f64; 9] = [
    0.412453, 0.357580, 0.180423, // X
    0.212671, 0.715160, 0.072169, // Y
    0.019334, 0.119193, 0.950227, // Z
];
const XYZ2SRGB_D65: [f64; 9] = [
    3.240479, -1.537150, -0.498535, //
    -0.969256, 1.875991, 0.041556, //
    0.055648, -0.204043, 1.057311,
];
const D65_WHITE: [f64; 3] = [0.950456, 1.0, 1.088754];

struct LabTables {
    /// sRGB gamma LUT scaled by `1 << GAMMA_SHIFT` (OpenCV `sRGBGammaTab_b`).
    gamma: [i32; 256],
    /// f(t) LUT scaled by `1 << LAB_SHIFT2` (OpenCV `LabCbrtTab_b`).
    cbrt: Vec<i32>,
    /// Row-whitened XYZ coefficients scaled by `1 << LAB_SHIFT`.
    coeffs: [i32; 9],
}

fn lab_tables() -> &'static LabTables {
    use std::sync::OnceLock;
    static TABLES: OnceLock<LabTables> = OnceLock::new();
    TABLES.get_or_init(|| {
        let mut gamma = [0i32; 256];
        for (i, g) in gamma.iter_mut().enumerate() {
            let x = i as f64 / 255.0;
            *g = ((GAMMA_TAB_MAX as f64) * srgb_inv_gamma(x)).round() as i32;
        }
        let mut cbrt = vec![0i32; CBRT_TAB_SIZE + 1];
        for (i, c) in cbrt.iter_mut().enumerate() {
            let x = i as f64 / GAMMA_TAB_MAX as f64;
            let f = if x < 0.008856 {
                x * 7.787 + 16.0 / 116.0
            } else {
                x.cbrt()
            };
            *c = (((1 << LAB_SHIFT2) as f64) * f).round() as i32;
        }
        let mut coeffs = [0i32; 9];
        for row in 0..3 {
            for col in 0..3 {
                coeffs[row * 3 + col] = (((1 << LAB_SHIFT) as f64) * SRGB2XYZ_D65[row * 3 + col]
                    / D65_WHITE[row])
                    .round() as i32;
            }
        }
        LabTables {
            gamma,
            cbrt,
            coeffs,
        }
    })
}

#[inline]
fn descale(x: i32, n: i32) -> i32 {
    (x + (1 << (n - 1))) >> n
}

/// One BGR pixel to 8-bit Lab, exactly like `cv2.cvtColor(..., COLOR_BGR2LAB)`
/// (fixed-point path with sRGB linearization).
#[inline]
pub fn bgr_pixel_to_lab(b: u8, g: u8, r: u8) -> (u8, u8, u8) {
    let t = lab_tables();
    // (116*255 + 50) / 100 and -((16*255*(1<<LAB_SHIFT2) + 50) / 100), the
    // integer-division constants from OpenCV's RGB2Lab_b.
    const L_SCALE: i32 = (116 * 255 + 50) / 100;
    const L_SHIFT: i32 = -((16 * 255 * (1 << LAB_SHIFT2) + 50) / 100);
    let rr = t.gamma[r as usize];
    let gg = t.gamma[g as usize];
    let bb = t.gamma[b as usize];
    let fx = t.cbrt[descale(
        rr * t.coeffs[0] + gg * t.coeffs[1] + bb * t.coeffs[2],
        LAB_SHIFT,
    ) as usize];
    let fy = t.cbrt[descale(
        rr * t.coeffs[3] + gg * t.coeffs[4] + bb * t.coeffs[5],
        LAB_SHIFT,
    ) as usize];
    let fz = t.cbrt[descale(
        rr * t.coeffs[6] + gg * t.coeffs[7] + bb * t.coeffs[8],
        LAB_SHIFT,
    ) as usize];
    let l = descale(l_mul(fy, L_SCALE) + L_SHIFT, LAB_SHIFT2);
    let a = descale(500 * (fx - fy) + (128 << LAB_SHIFT2), LAB_SHIFT2);
    let bb_out = descale(200 * (fy - fz) + (128 << LAB_SHIFT2), LAB_SHIFT2);
    (clamp_u8(l), clamp_u8(a), clamp_u8(bb_out))
}

#[inline]
fn l_mul(fy: i32, scale: i32) -> i32 {
    fy * scale
}

#[inline]
fn clamp_u8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// BGR image to 3-channel 8-bit Lab.
pub fn bgr_to_lab(src: &Image) -> Image {
    assert_eq!(src.channels, 3);
    let mut out = Image::new(src.width, src.height, 3);
    for (dst, px) in out.data.chunks_exact_mut(3).zip(src.data.chunks_exact(3)) {
        let (l, a, b) = bgr_pixel_to_lab(px[0], px[1], px[2]);
        dst[0] = l;
        dst[1] = a;
        dst[2] = b;
    }
    out
}

/// One 8-bit Lab pixel back to BGR (float inverse path + sRGB companding).
///
/// Used only for exposure normalization round-trips where ≤1 LSB deviations
/// are absorbed by the downstream decision gates; OpenCV's bit-exact integer
/// inverse is intentionally not replicated unless goldens demand it.
#[inline]
pub fn lab_pixel_to_bgr(l: u8, a: u8, b: u8) -> (u8, u8, u8) {
    let li = l as f64 * (100.0 / 255.0);
    let ai = a as f64 - 128.0;
    let bi = b as f64 - 128.0;
    let fy = (li + 16.0) / 116.0;
    let fx = fy + ai / 500.0;
    let fz = fy - bi / 200.0;
    let finv = |f: f64| {
        let f3 = f * f * f;
        if f3 > 0.008856 {
            f3
        } else {
            (f - 16.0 / 116.0) / 7.787
        }
    };
    let x = finv(fx) * D65_WHITE[0];
    let y = if li > 8.0 { fy * fy * fy } else { li / 903.3 };
    let z = finv(fz) * D65_WHITE[2];
    let mut bgr = [0u8; 3];
    for (i, out) in bgr.iter_mut().enumerate() {
        // XYZ2SRGB rows are R, G, B; emit BGR.
        let row = 2 - i;
        let lin = XYZ2SRGB_D65[row * 3] * x
            + XYZ2SRGB_D65[row * 3 + 1] * y
            + XYZ2SRGB_D65[row * 3 + 2] * z;
        let v = 255.0 * srgb_gamma(lin.clamp(0.0, 1.0));
        *out = v.round_ties_even().clamp(0.0, 255.0) as u8;
    }
    (bgr[0], bgr[1], bgr[2])
}

/// 8-bit Lab image back to BGR.
pub fn lab_to_bgr(src: &Image) -> Image {
    assert_eq!(src.channels, 3);
    let mut out = Image::new(src.width, src.height, 3);
    for (dst, px) in out.data.chunks_exact_mut(3).zip(src.data.chunks_exact(3)) {
        let (b, g, r) = lab_pixel_to_bgr(px[0], px[1], px[2]);
        dst[0] = b;
        dst[1] = g;
        dst[2] = r;
    }
    out
}

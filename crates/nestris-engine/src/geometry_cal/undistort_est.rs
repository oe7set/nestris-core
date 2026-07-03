//! Barrel-distortion estimation (port of `geometry/undistort.py`).
//!
//! Decision-level cross-language contract: same engage/skip choice and k1
//! pick per fixture (the underlying Canny/Hough differ in RNG).

use nestris_vision::rng::{Pcg32, splitmix64};
use nestris_vision::undistort::{UndistortMap, build_map};
use nestris_vision::{Image, canny, hough, warp};

use crate::palette::to_luma;

const BOW_TRIGGER: f64 = 0.004;
const K1_CANDIDATES: [f64; 6] = [-0.30, -0.25, -0.20, -0.15, -0.10, -0.05];

fn measure_bow(gray: &Image, rng: &mut Pcg32) -> f64 {
    let (h, w) = (gray.height as f64, gray.width as f64);
    let edges = canny::canny(gray, 60.0, 160.0);
    let min_len = 0.25 * h.min(w);
    let lines = hough::hough_lines_p(&edges, std::f64::consts::PI / 180.0, 80, min_len, 6, rng);
    if lines.is_empty() {
        return 0.0;
    }
    let diag = w.hypot(h);
    let mut devs = Vec::with_capacity(lines.len());
    for (x1, y1, x2, y2) in lines {
        let ang = ((y2 - y1) as f64)
            .atan2((x2 - x1) as f64)
            .to_degrees()
            .rem_euclid(90.0);
        let dev = ang.min(90.0 - ang);
        let length = ((x2 - x1) as f64).hypot((y2 - y1) as f64);
        devs.push((dev / 90.0) * (length / diag));
    }
    devs.iter().sum::<f64>() / devs.len() as f64
}

/// Apply an undistort map to a BGR image (`cv2.remap` INTER_LINEAR).
pub fn apply_undistort(map: &UndistortMap, image: &Image) -> Image {
    warp::remap_bilinear(image, &map.map_x, &map.map_y, map.width, map.height)
}

/// Estimate a mild barrel-undistortion map, or `None` if not needed.
pub fn estimate_radial_distortion(image: &Image) -> Option<UndistortMap> {
    let gray = to_luma(image);
    let (w, h) = (gray.width, gray.height);
    let mut rng = Pcg32::new(splitmix64(0xB0B0));
    let base_bow = measure_bow(&gray, &mut rng);
    if base_bow < BOW_TRIGGER {
        return None;
    }
    let mut best: Option<(f64, UndistortMap)> = None;
    for k1 in K1_CANDIDATES {
        let umap = build_map(w, h, k1);
        let corrected = to_luma(&apply_undistort(&umap, image));
        let bow = measure_bow(&corrected, &mut rng);
        if best.as_ref().is_none_or(|(b, _)| bow < *b) {
            best = Some((bow, umap));
        }
    }
    match best {
        Some((bow, umap)) if bow < base_bow => Some(umap),
        _ => None,
    }
}

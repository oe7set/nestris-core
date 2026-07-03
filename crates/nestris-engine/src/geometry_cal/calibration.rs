//! Anchor-based geometry solver + rectifier (port of `geometry/calibration.py`).

use std::sync::Arc;

use nestris_vision::homography::{Mat3, find_homography_ransac, perspective_transform_4, project};
use nestris_vision::rng::{Pcg32, splitmix64};
use nestris_vision::undistort::UndistortMap;
use nestris_vision::warp::{WarpMap, warp_perspective};
use nestris_vision::{Image, ncc, resize};

use crate::geometry::Quad;
use crate::geometry_cal::anchors::{
    AnchorCorrespondence, detect_label_anchors, detect_playfield_candidates,
    hud_constellation_score_from, playfield_correspondences,
};
use crate::geometry_cal::undistort_est::{apply_undistort, estimate_radial_distortion};
use crate::layout::{CANON_HEIGHT, CANON_WIDTH, LayoutTable, get_layout};
use crate::palette::to_luma;
use crate::templates::label_templates;

const RANSAC_REPROJ: f64 = 3.0;
const NO_CONSTELLATION_ACQUIRE: f64 = 0.65;
const RELABEL_PAD: usize = 2;
const CONSTELLATION_WEIGHT: f64 = 1.5;

/// Outcome of [`estimate_geometry`].
#[derive(Clone)]
pub struct GeometryResult {
    pub homography: Option<Mat3>,
    pub confidence: f64,
    pub undistort: Option<Arc<UndistortMap>>,
    pub residual: f64,
}

impl GeometryResult {
    pub fn ok(&self) -> bool {
        self.homography.is_some()
    }

    fn failed(confidence: f64, undistort: Option<Arc<UndistortMap>>) -> Self {
        GeometryResult {
            homography: None,
            confidence,
            undistort,
            residual: f64::INFINITY,
        }
    }
}

fn solve_homography(
    correspondences: &[AnchorCorrespondence],
    rng: &mut Pcg32,
) -> (Option<Mat3>, f64, usize) {
    if correspondences.len() < 4 {
        return (None, f64::INFINITY, 0);
    }
    let src: Vec<(f64, f64)> = correspondences.iter().map(|c| c.src_xy).collect();
    let dst: Vec<(f64, f64)> = correspondences.iter().map(|c| c.canon_xy).collect();
    let Some(result) = find_homography_ransac(&src, &dst, RANSAC_REPROJ, rng, 2000, 0.995) else {
        return (None, f64::INFINITY, 0);
    };
    let mut residual = 0.0;
    let mut count = 0usize;
    for (i, inlier) in result.inliers.iter().enumerate() {
        if !inlier {
            continue;
        }
        let (px, py) = project(&result.h, src[i].0, src[i].1);
        residual += (px - dst[i].0).hypot(py - dst[i].1);
        count += 1;
    }
    if count == 0 {
        return (None, f64::INFINITY, 0);
    }
    (Some(result.h), residual / count as f64, count)
}

/// Mean label-template correlation at canonical label rects, or `None`.
fn relabel_score(canon_gray: &Image, layout: &LayoutTable) -> Option<f64> {
    let templates = label_templates();
    if templates.is_empty() {
        return None;
    }
    let rects = [
        ("LINES", &layout.label_lines),
        ("SCORE", &layout.label_score),
        ("NEXT", &layout.label_next),
        ("LEVEL", &layout.label_level),
    ];
    let (h, w) = (canon_gray.height, canon_gray.width);
    let mut scores: Vec<f64> = Vec::new();
    for (name, rect) in rects {
        let Some((_, tpl)) = templates.iter().find(|(l, _)| *l == name) else {
            continue;
        };
        let x0 = (rect.x as usize).saturating_sub(RELABEL_PAD);
        let y0 = (rect.y as usize).saturating_sub(RELABEL_PAD);
        let x1 = ((rect.x2() as usize) + RELABEL_PAD).min(w);
        let y1 = ((rect.y2() as usize) + RELABEL_PAD).min(h);
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        let patch = canon_gray.crop(x0, y0, x1 - x0, y1 - y0);
        if (patch.height as f64) < rect.h || (patch.width as f64) < rect.w {
            continue;
        }
        let tpl_r = resize::resize_area(tpl, rect.w as usize, rect.h as usize);
        let response = ncc::match_template_ccoeff_normed(&patch, &tpl_r);
        let (max_val, _) = response.max();
        scores.push((max_val as f64).max(0.0));
    }
    if scores.is_empty() {
        None
    } else {
        Some(scores.iter().sum::<f64>() / scores.len() as f64)
    }
}

/// Score a candidate homography in `[0, 1]` (port of `_validate_geometry`).
fn validate_geometry(
    image: &Image,
    h: &Mat3,
    layout: &LayoutTable,
    residual: f64,
    resid_informative: bool,
) -> f64 {
    let warped = warp_perspective(image, h, CANON_WIDTH, CANON_HEIGHT);
    let gray = to_luma(&warped);
    let (px0, py0, px1, py1) = layout.playfield.to_bounds();
    let (px1, py1) = (px1.min(gray.width), py1.min(gray.height));
    if px1 <= px0 || py1 <= py0 {
        return 0.0;
    }
    let mut dark = 0usize;
    let mut total = 0usize;
    for y in py0..py1 {
        for x in px0..px1 {
            total += 1;
            if gray.data[y * gray.width + x] < 64 {
                dark += 1;
            }
        }
    }
    let dark_frac = dark as f64 / total as f64;
    let resid_score = (1.0 - residual / (2.0 * RANSAC_REPROJ)).max(0.0);
    match relabel_score(&gray, layout) {
        None => {
            if !resid_informative {
                dark_frac
            } else {
                0.45 * dark_frac + 0.55 * resid_score
            }
        }
        Some(label_score) => {
            if !resid_informative {
                0.375 * dark_frac + 0.625 * label_score
            } else {
                0.30 * dark_frac + 0.20 * resid_score + 0.50 * label_score
            }
        }
    }
}

fn pf_quad_to_canonical(quad: &Quad, layout: &LayoutTable) -> Option<Mat3> {
    let pf = &layout.playfield;
    perspective_transform_4(
        &quad.corners(),
        &[
            (pf.x, pf.y),
            (pf.x2(), pf.y),
            (pf.x2(), pf.y2()),
            (pf.x, pf.y2()),
        ],
    )
}

/// Estimate the source->canonical homography for one frame.
///
/// `seed` drives the deterministic RANSAC (derive from the frame seq).
pub fn estimate_geometry(
    image: &Image,
    layout: &LayoutTable,
    undistort: Option<Arc<UndistortMap>>,
    try_undistort: bool,
    seed: u64,
) -> GeometryResult {
    let mut umap = undistort;
    if umap.is_none() && try_undistort {
        umap = estimate_radial_distortion(image).map(Arc::new);
    }
    let work_owned;
    let work: &Image = match &umap {
        Some(m) => {
            work_owned = apply_undistort(m, image);
            &work_owned
        }
        None => image,
    };

    let candidates = detect_playfield_candidates(work, 6);
    if candidates.is_empty() {
        return GeometryResult::failed(0.0, umap);
    }
    let work_gray = to_luma(work);
    let mut rng = Pcg32::new(splitmix64(seed));

    let mut best: Option<GeometryResult> = None;
    let mut best_combined = -1.0f64;
    let mut best_conf_only: Option<GeometryResult> = None;
    for anchor in &candidates {
        // One label-detection pass per candidate, shared by the
        // correspondence set and the constellation (deterministic, so this
        // is cost-only — the Python oracle detects twice with equal results).
        let labels = detect_label_anchors(&work_gray, anchor, layout, 3.0);
        let mut correspondences = playfield_correspondences(anchor, layout);
        correspondences.extend(labels.iter().cloned());
        let (mut h, mut residual, inlier_count) = solve_homography(&correspondences, &mut rng);
        let exact_solve = h.is_none() || inlier_count <= 4;
        if h.is_none() {
            h = pf_quad_to_canonical(&anchor.quad, layout);
            residual = 0.0;
        }
        let Some(h) = h else { continue };
        let confidence = validate_geometry(work, &h, layout, residual, !exact_solve);
        let constellation = hud_constellation_score_from(&labels, anchor, layout);
        let result = GeometryResult {
            homography: Some(h),
            confidence,
            undistort: umap.clone(),
            residual,
        };
        if best_conf_only
            .as_ref()
            .is_none_or(|b| confidence > b.confidence)
        {
            best_conf_only = Some(result.clone());
        }
        let combined = confidence + CONSTELLATION_WEIGHT * constellation;
        if combined > best_combined {
            best_combined = combined;
            best = if constellation > 0.0 {
                Some(result)
            } else {
                None
            };
        }
    }

    if let Some(best) = best {
        return best;
    }
    let best_conf_only = best_conf_only.expect("candidates were non-empty");
    // Label templates are always embedded in the Rust build, so the
    // no-constellation fallback is always gated.
    if best_conf_only.confidence < NO_CONSTELLATION_ACQUIRE {
        return GeometryResult::failed(best_conf_only.confidence, umap);
    }
    best_conf_only
}

/// Warps source frames onto the canonical 256×240 raster (one precomputed
/// sampling map per adopted geometry; optional undistort pre-pass mirrors the
/// Python two-pass remap-then-warp semantics).
pub struct Rectifier {
    matrix: Mat3,
    warp_map: WarpMap,
    undistort: Option<Arc<UndistortMap>>,
}

impl Rectifier {
    pub fn new(matrix: Mat3, undistort: Option<Arc<UndistortMap>>) -> Option<Rectifier> {
        let warp_map = WarpMap::new(&matrix, CANON_WIDTH, CANON_HEIGHT)?;
        Some(Rectifier {
            matrix,
            warp_map,
            undistort,
        })
    }

    pub fn matrix(&self) -> &Mat3 {
        &self.matrix
    }

    pub fn undistort(&self) -> Option<&Arc<UndistortMap>> {
        self.undistort.as_ref()
    }

    /// Warp a BGR frame to the canonical 256×240 raster.
    pub fn rectify(&self, image: &Image) -> Image {
        match &self.undistort {
            Some(map) => self.warp_map.apply(&apply_undistort(map, image)),
            None => self.warp_map.apply(image),
        }
    }

    pub fn default_layout() -> &'static LayoutTable {
        get_layout()
    }
}

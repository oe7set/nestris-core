//! Anchor detection for self-calibration (port of `geometry/anchors.py`).

use nestris_vision::{Image, components, contour, morphology, ncc, resize};

use crate::geometry::Quad;
use crate::layout::{LayoutTable, TILE};
use crate::palette::to_luma;
use crate::templates::label_templates;

pub const PLAYFIELD_DARK_THRESH: u8 = 40;
const DARK_THRESHES: [u8; 2] = [PLAYFIELD_DARK_THRESH, 64];
const MIN_AREA_FRAC: f64 = 0.02;
const ASPECT_LO: f64 = 1.0;
const ASPECT_HI: f64 = 2.6;
const ASPECT_FLAT_LO: f64 = 1.1;
const ASPECT_FLAT_HI: f64 = 2.2;
const BBOX_ASPECT_PRE: (f64, f64) = (0.7, 3.6);
const LABEL_MIN_SCORE: f64 = 0.30;
pub const PLAYFIELD_HUD_LABELS: [&str; 4] = ["LINES", "SCORE", "NEXT", "LEVEL"];
const OPEN_KERNELS: [usize; 2] = [0, 5];
const DEDUPE_DIST: f64 = 40.0;
const DEDUPE_KEEP_PER_GROUP: usize = 2;
const MIN_CONSTELLATION_LABELS: usize = 3;
const CONSTELLATION_TOL_TILES: f64 = 2.0;

/// The detected playfield interior.
#[derive(Clone, Debug)]
pub struct PlayfieldAnchor {
    pub quad: Quad,
    pub score: f64,
    pub area_frac: f64,
    pub aspect: f64,
}

/// One canonical<->source point correspondence for the homography solve.
#[derive(Clone, Debug)]
pub struct AnchorCorrespondence {
    pub canon_xy: (f64, f64),
    pub src_xy: (f64, f64),
    pub score: f64,
    pub name: String,
}

/// Order four points as (tl, tr, br, bl) using sum/diff heuristics.
fn order_quad_corners(pts: &[(f64, f64); 4]) -> Quad {
    let argmin = |vals: [f64; 4]| {
        (0..4)
            .min_by(|&a, &b| vals[a].partial_cmp(&vals[b]).unwrap())
            .unwrap()
    };
    let argmax = |vals: [f64; 4]| {
        (0..4)
            .max_by(|&a, &b| vals[a].partial_cmp(&vals[b]).unwrap())
            .unwrap()
    };
    let sums = pts.map(|(x, y)| x + y);
    let diffs = pts.map(|(x, y)| y - x);
    Quad {
        tl: pts[argmin(sums)],
        br: pts[argmax(sums)],
        tr: pts[argmin(diffs)],
        bl: pts[argmax(diffs)],
    }
}

fn quad_edge_lengths(quad: &Quad) -> (f64, f64) {
    let d = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).hypot(a.1 - b.1);
    let w = (d(quad.tr, quad.tl) + d(quad.br, quad.bl)) / 2.0;
    let h = (d(quad.bl, quad.tl) + d(quad.br, quad.tr)) / 2.0;
    (w, h)
}

fn quad_aspect(quad: &Quad) -> f64 {
    let (w, h) = quad_edge_lengths(quad);
    h / w.max(1e-6)
}

fn aspect_score(aspect: f64) -> f64 {
    if aspect < ASPECT_FLAT_LO {
        ((aspect - ASPECT_LO) / (ASPECT_FLAT_LO - ASPECT_LO)).max(0.0)
    } else if aspect > ASPECT_FLAT_HI {
        ((ASPECT_HI - aspect) / (ASPECT_HI - ASPECT_FLAT_HI)).max(0.0)
    } else {
        1.0
    }
}

pub fn quad_center(quad: &Quad) -> (f64, f64) {
    let xs = quad.tl.0 + quad.tr.0 + quad.br.0 + quad.bl.0;
    let ys = quad.tl.1 + quad.tr.1 + quad.br.1 + quad.bl.1;
    (xs / 4.0, ys / 4.0)
}

/// Detect up to `top_n` playfield-interior candidates, best score first.
pub fn detect_playfield_candidates(image: &Image, top_n: usize) -> Vec<PlayfieldAnchor> {
    let gray = to_luma(image);
    let (w, h) = (gray.width, gray.height);
    let frame_area = (w * h) as f64;

    let k3 = morphology::Kernel::ellipse3();
    let k5 = morphology::Kernel::ellipse5();
    let mut candidates: Vec<PlayfieldAnchor> = Vec::new();
    for thresh in DARK_THRESHES {
        let mut base = Image::new(w, h, 1);
        for (dst, &v) in base.data.iter_mut().zip(gray.data.iter()) {
            *dst = u8::from(v < thresh);
        }
        let base = morphology::open(&morphology::close(&base, &k3, 2), &k3, 1);
        for extra_open in OPEN_KERNELS {
            let mask = if extra_open > 0 {
                morphology::open(&base, &k5, 1)
            } else {
                base.clone()
            };
            candidates.extend(candidates_from_mask(&mask, w, h, frame_area));
        }
    }
    let mut candidates = dedupe_candidates(candidates);
    candidates.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
    candidates.truncate(top_n);
    candidates
}

fn candidates_from_mask(mask: &Image, w: usize, h: usize, frame_area: f64) -> Vec<PlayfieldAnchor> {
    let labeled = components::connected_components(mask);
    let mut out = Vec::new();
    for (i, comp) in labeled.components.iter().enumerate() {
        let area = comp.area as f64;
        if area < MIN_AREA_FRAC * frame_area || comp.w == 0 || comp.h == 0 {
            continue;
        }
        let bbox_aspect = comp.h as f64 / comp.w as f64;
        if bbox_aspect <= BBOX_ASPECT_PRE.0 || bbox_aspect >= BBOX_ASPECT_PRE.1 {
            continue;
        }
        if comp.x == 0 && comp.y == 0 && comp.x + comp.w >= w as u32 && comp.y + comp.h >= h as u32
        {
            continue;
        }
        let pts: Vec<(f64, f64)> = labeled
            .component_points(i)
            .into_iter()
            .map(|(x, y)| (x as f64, y as f64))
            .collect();
        let Some(rect) = contour::min_area_rect(&pts) else {
            continue;
        };
        let quad = order_quad_corners(&rect.box_points());
        let aspect = quad_aspect(&quad);
        if !(aspect > ASPECT_LO && aspect < ASPECT_HI) {
            continue;
        }
        let fill = area / frame_area;
        let fill_score = (fill / 0.10).min(1.0);
        let cx = comp.x as f64 + comp.w as f64 / 2.0;
        let cy = comp.y as f64 + comp.h as f64 / 2.0;
        let centrality = 1.0
            - ((cx - w as f64 / 2.0).abs() / (w as f64 / 2.0)) * 0.5
            - ((cy - h as f64 / 2.0).abs() / (h as f64 / 2.0)) * 0.5;
        let score = aspect_score(aspect) * fill_score * centrality.max(0.0);
        out.push(PlayfieldAnchor {
            quad,
            score,
            area_frac: fill,
            aspect,
        });
    }
    out
}

fn dedupe_candidates(candidates: Vec<PlayfieldAnchor>) -> Vec<PlayfieldAnchor> {
    let mut sorted = candidates;
    sorted.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
    let mut kept: Vec<PlayfieldAnchor> = Vec::new();
    let mut group_centers: Vec<(f64, f64)> = Vec::new();
    let mut group_counts: Vec<usize> = Vec::new();
    for cand in sorted {
        let (cx, cy) = quad_center(&cand.quad);
        let mut grouped = false;
        for (gi, &(kx, ky)) in group_centers.iter().enumerate() {
            if (cx - kx).abs() < DEDUPE_DIST && (cy - ky).abs() < DEDUPE_DIST {
                if group_counts[gi] < DEDUPE_KEEP_PER_GROUP {
                    group_counts[gi] += 1;
                    kept.push(cand.clone());
                }
                grouped = true;
                break;
            }
        }
        if !grouped {
            group_centers.push((cx, cy));
            group_counts.push(1);
            kept.push(cand);
        }
    }
    kept
}

/// Map the four playfield-anchor corners to canonical playfield corners.
pub fn playfield_correspondences(
    anchor: &PlayfieldAnchor,
    layout: &LayoutTable,
) -> Vec<AnchorCorrespondence> {
    let pf = &layout.playfield;
    let canon = [
        ("tl", (pf.x, pf.y)),
        ("tr", (pf.x2(), pf.y)),
        ("br", (pf.x2(), pf.y2())),
        ("bl", (pf.x, pf.y2())),
    ];
    let src = [
        ("tl", anchor.quad.tl),
        ("tr", anchor.quad.tr),
        ("br", anchor.quad.br),
        ("bl", anchor.quad.bl),
    ];
    canon
        .iter()
        .zip(src.iter())
        .map(|((k, c), (_, s))| AnchorCorrespondence {
            canon_xy: *c,
            src_xy: *s,
            score: anchor.score,
            name: format!("playfield.{k}"),
        })
        .collect()
}

/// Per-axis similarity mapping canonical px -> approximate source px.
pub struct ApproxSimilarity {
    pf_x: f64,
    pf_y: f64,
    ox: f64,
    oy: f64,
    pub sx: f64,
    pub sy: f64,
}

impl ApproxSimilarity {
    pub fn to_src(&self, cx: f64, cy: f64) -> (f64, f64) {
        (
            self.ox + (cx - self.pf_x) * self.sx,
            self.oy + (cy - self.pf_y) * self.sy,
        )
    }
}

pub fn approx_similarity(anchor: &PlayfieldAnchor, layout: &LayoutTable) -> ApproxSimilarity {
    let pf = &layout.playfield;
    let (src_w, src_h) = quad_edge_lengths(&anchor.quad);
    ApproxSimilarity {
        pf_x: pf.x,
        pf_y: pf.y,
        ox: anchor.quad.tl.0,
        oy: anchor.quad.tl.1,
        sx: src_w / pf.w,
        sy: src_h / pf.h,
    }
}

fn label_rects(layout: &LayoutTable) -> [(&'static str, &crate::geometry::Rect); 5] {
    [
        ("LINES", &layout.label_lines),
        ("SCORE", &layout.label_score),
        ("NEXT", &layout.label_next),
        ("LEVEL", &layout.label_level),
        ("STATISTICS", &layout.label_statistics),
    ]
}

/// Find HUD label anchors via predicted-window template matching.
pub fn detect_label_anchors(
    image_gray: &Image,
    anchor: &PlayfieldAnchor,
    layout: &LayoutTable,
    slack_tiles: f64,
) -> Vec<AnchorCorrespondence> {
    let templates = label_templates();
    let (h, w) = (image_gray.height, image_gray.width);
    let sim = approx_similarity(anchor, layout);
    let slack_x = slack_tiles * TILE * sim.sx;
    let slack_y = slack_tiles * TILE * sim.sy;

    let mut out = Vec::new();
    for (name, rect) in label_rects(layout) {
        let Some((_, tpl)) = templates.iter().find(|(l, _)| *l == name) else {
            continue;
        };
        let ccx = rect.x + rect.w / 2.0;
        let ccy = rect.y + rect.h / 2.0;
        let (psx, psy) = sim.to_src(ccx, ccy);
        let tw = ((rect.w * sim.sx).round_ties_even() as usize).max(4);
        let th = ((rect.h * sim.sy).round_ties_even() as usize).max(4);
        let tpl_r = resize::resize_area(tpl, tw, th);

        let x0 = (psx - tw as f64 / 2.0 - slack_x).max(0.0) as usize;
        let y0 = (psy - th as f64 / 2.0 - slack_y).max(0.0) as usize;
        let x1 = ((psx + tw as f64 / 2.0 + slack_x) as usize).min(w);
        let y1 = ((psy + th as f64 / 2.0 + slack_y) as usize).min(h);
        if x1.saturating_sub(x0) < tw || y1.saturating_sub(y0) < th {
            continue;
        }
        let window = image_gray.crop(x0, y0, x1 - x0, y1 - y0);
        let response = ncc::match_template_ccoeff_normed(&window, &tpl_r);
        let (max_val, (mx, my)) = response.max();
        let score = (max_val as f64).max(0.0);
        if score < LABEL_MIN_SCORE {
            continue;
        }
        out.push(AnchorCorrespondence {
            canon_xy: (ccx, ccy),
            src_xy: (
                x0 as f64 + mx as f64 + tw as f64 / 2.0,
                y0 as f64 + my as f64 + th as f64 / 2.0,
            ),
            score,
            name: format!("label.{name}"),
        });
    }
    out
}

/// Score how well the playfield-surrounding HUD labels fit `anchor`.
pub fn hud_constellation_score(
    image_gray: &Image,
    anchor: &PlayfieldAnchor,
    layout: &LayoutTable,
) -> f64 {
    let labels = detect_label_anchors(image_gray, anchor, layout, 3.0);
    let sim = approx_similarity(anchor, layout);
    let rects = [
        ("LINES", &layout.label_lines),
        ("SCORE", &layout.label_score),
        ("NEXT", &layout.label_next),
        ("LEVEL", &layout.label_level),
    ];
    let mut hits: Vec<f64> = Vec::new();
    let mut offsets: Vec<(f64, f64)> = Vec::new();
    for c in &labels {
        let name = c.name.split('.').next_back().unwrap_or("");
        let Some((_, rect)) = rects.iter().find(|(n, _)| *n == name) else {
            continue;
        };
        let (px, py) = sim.to_src(rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
        offsets.push((c.src_xy.0 - px, c.src_xy.1 - py));
        hits.push(c.score);
    }
    if hits.len() < MIN_CONSTELLATION_LABELS {
        return 0.0;
    }
    let mean = hits.iter().sum::<f64>() / hits.len() as f64;
    let coverage = hits.len() as f64 / PLAYFIELD_HUD_LABELS.len() as f64;
    let std = |vals: Vec<f64>| {
        let n = vals.len() as f64;
        let m = vals.iter().sum::<f64>() / n;
        (vals.iter().map(|v| (v - m) * (v - m)).sum::<f64>() / n).sqrt()
    };
    let tol_x = CONSTELLATION_TOL_TILES * TILE * sim.sx.max(1e-3);
    let tol_y = CONSTELLATION_TOL_TILES * TILE * sim.sy.max(1e-3);
    let spread_norm = (std(offsets.iter().map(|o| o.0).collect()) / tol_x)
        .hypot(std(offsets.iter().map(|o| o.1).collect()) / tol_y);
    let consistency = (1.0 - spread_norm).max(0.0);
    mean * coverage * consistency
}

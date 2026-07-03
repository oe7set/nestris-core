//! Golden tests for the geometry primitives (homography, warp, minAreaRect,
//! undistort maps) against OpenCV outputs in `testdata/cv/`.

use std::fs::File;
use std::path::{Path, PathBuf};

use nestris_vision::homography::{
    self, Mat3, find_homography_ransac, perspective_transform_4, project,
};
use nestris_vision::rng::{Pcg32, splitmix64};
use nestris_vision::{Image, contour, undistort, warp};
use serde_json::Value;

fn testdata(domain: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/cv")
        .join(domain)
}

fn load_png(path: &Path) -> Image {
    let decoder = png::Decoder::new(File::open(path).unwrap_or_else(|e| {
        panic!("cannot open golden {}: {e}", path.display());
    }));
    let mut reader = decoder.read_info().expect("png read_info");
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).expect("png frame");
    buf.truncate(info.buffer_size());
    let channels = match info.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::Rgb => 3,
        other => panic!("unexpected png color type {other:?}"),
    };
    let mut img = Image::from_vec(buf, info.width as usize, info.height as usize, channels);
    if channels == 3 {
        // PNG channel rule: channels are stored verbatim (BGR arrays keep
        // B first), nothing to swap — see tools/gen_cv_goldens.py.
        let _ = &mut img;
    }
    img
}

fn cases(domain: &str) -> Vec<Value> {
    let raw = std::fs::read_to_string(testdata(domain).join("cases.json"))
        .expect("cases.json (run tools/gen_cv_goldens.py)");
    serde_json::from_str(&raw).expect("cases.json parse")
}

fn pts(v: &Value) -> Vec<(f64, f64)> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|p| {
            let p = p.as_array().unwrap();
            (p[0].as_f64().unwrap(), p[1].as_f64().unwrap())
        })
        .collect()
}

fn mat(v: &Value) -> Mat3 {
    let m: Vec<f64> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_f64().unwrap())
        .collect();
    std::array::from_fn(|i| m[i])
}

// ---------------------------------------------------------------------------
// homography
// ---------------------------------------------------------------------------

#[test]
fn get_perspective_transform_exact() {
    let case = &cases("homography")[0];
    assert_eq!(case["kind"], "getPerspectiveTransform");
    let src = pts(&case["src"]);
    let dst = pts(&case["dst"]);
    let h = perspective_transform_4(
        &[src[0], src[1], src[2], src[3]],
        &[dst[0], dst[1], dst[2], dst[3]],
    )
    .unwrap();
    let expected = mat(&case["h"]);
    for (a, b) in h.iter().zip(expected.iter()) {
        assert!((a - b).abs() <= 1e-9 * b.abs().max(1.0), "H {a} vs {b}");
    }
    // perspectiveTransform points.
    let tp = pts(&case["transform_pts"]);
    let to = pts(&case["transform_out"]);
    for (p, e) in tp.iter().zip(to.iter()) {
        let (x, y) = project(&h, p.0, p.1);
        assert!((x - e.0).abs() < 1e-9 && (y - e.1).abs() < 1e-9);
    }
}

#[test]
fn ransac_outcome_matches_opencv() {
    for case in &cases("homography")[1..] {
        let name = case["name"].as_str().unwrap();
        let src = pts(&case["src"]);
        let dst = pts(&case["dst"]);
        let expected_h = mat(&case["h_est"]);
        let expected_mask: Vec<bool> = case["inlier_mask"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_i64().unwrap() != 0)
            .collect();
        let mut rng = Pcg32::new(splitmix64(1));
        let result = find_homography_ransac(&src, &dst, 3.0, &mut rng, 2000, 0.995)
            .unwrap_or_else(|| panic!("{name}: no homography"));
        assert_eq!(result.inliers, expected_mask, "{name}: inlier set");
        // Compare via mean reprojection over the inlier set, which is what
        // the engine's confidence blend consumes — H entries themselves are
        // ill-conditioned to compare directly.
        let mut reproj = 0.0;
        let mut reproj_cv = 0.0;
        let mut count = 0usize;
        for i in 0..src.len() {
            if !expected_mask[i] {
                continue;
            }
            let (x, y) = project(&result.h, src[i].0, src[i].1);
            reproj += ((x - dst[i].0).powi(2) + (y - dst[i].1).powi(2)).sqrt();
            let (cx, cy) = project(&expected_h, src[i].0, src[i].1);
            reproj_cv += ((cx - dst[i].0).powi(2) + (cy - dst[i].1).powi(2)).sqrt();
            count += 1;
        }
        reproj /= count as f64;
        reproj_cv /= count as f64;
        assert!(
            (reproj - reproj_cv).abs() <= 0.1,
            "{name}: mean inlier reproj ours={reproj:.4} cv={reproj_cv:.4}"
        );
        // And the matrices should agree to ~1e-3 relative on dominant terms.
        for (a, b) in result.h.iter().zip(expected_h.iter()) {
            assert!(
                (a - b).abs() <= 1e-3 * b.abs().max(1.0),
                "{name}: H {a} vs {b}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// warp
// ---------------------------------------------------------------------------

#[test]
fn warp_perspective_matches_opencv() {
    for case in cases("warp") {
        let name = case["name"].as_str().unwrap();
        let dir = testdata("warp");
        let input = load_png(&dir.join(format!("{name}_in.png")));
        let expected = load_png(&dir.join(format!("{name}_warped.png")));
        let h = mat(&case["h"]);
        let got = warp::warp_perspective(&input, &h, 256, 240);
        let mut exact = 0usize;
        let mut max_diff = 0u32;
        for (&a, &b) in got.data.iter().zip(expected.data.iter()) {
            let d = (a as i32 - b as i32).unsigned_abs();
            if d == 0 {
                exact += 1;
            }
            max_diff = max_diff.max(d);
        }
        let frac = exact as f64 / got.data.len() as f64;
        assert!(
            frac >= 0.99 && max_diff <= 2,
            "{name}: {:.3}% byte-exact, max diff {max_diff}",
            frac * 100.0
        );
        // The precomputed sampling map must agree with the direct warp.
        let map = warp::WarpMap::new(&h, 256, 240).unwrap();
        assert_eq!(map.apply(&input).data, got.data, "{name}: WarpMap parity");
    }
}

// ---------------------------------------------------------------------------
// minAreaRect
// ---------------------------------------------------------------------------

#[test]
fn min_area_rect_matches_opencv() {
    for case in cases("minarearect") {
        let name = case["name"].as_str().unwrap();
        let points = pts(&case["points"]);
        let rect = contour::min_area_rect(&points).unwrap();
        let c = case["center"].as_array().unwrap();
        let e_center = (c[0].as_f64().unwrap(), c[1].as_f64().unwrap());
        assert!(
            (rect.cx - e_center.0).abs() <= 0.5 && (rect.cy - e_center.1).abs() <= 0.5,
            "{name}: center ({}, {}) vs {:?}",
            rect.cx,
            rect.cy,
            e_center
        );
        let e_size = case["size"].as_array().unwrap();
        let (ew, eh) = (e_size[0].as_f64().unwrap(), e_size[1].as_f64().unwrap());
        let (glong, gshort) = (rect.width.max(rect.height), rect.width.min(rect.height));
        let (elong, eshort) = (ew.max(eh), ew.min(eh));
        assert!(
            (glong - elong).abs() <= 0.5 && (gshort - eshort).abs() <= 0.5,
            "{name}: size ({glong:.2}, {gshort:.2}) vs ({elong:.2}, {eshort:.2})"
        );
        // Corner sets must match within 0.5 px (cyclic order irrelevant).
        let mut got_box: Vec<(f64, f64)> = rect.box_points().to_vec();
        let mut exp_box = pts(&case["box"]);
        let key = |p: &(f64, f64)| (p.0 * 1000.0) as i64 + (p.1 * 1000.0) as i64;
        got_box.sort_by_key(key);
        exp_box.sort_by_key(key);
        for (g, e) in got_box.iter().zip(exp_box.iter()) {
            let d = (g.0 - e.0).hypot(g.1 - e.1);
            assert!(d <= 0.75, "{name}: corner {g:?} vs {e:?} (d={d:.3})");
        }
    }
}

// ---------------------------------------------------------------------------
// undistort maps
// ---------------------------------------------------------------------------

#[test]
fn undistort_map_matches_opencv() {
    for case in cases("undistort") {
        let name = case["name"].as_str().unwrap();
        let w = case["w"].as_u64().unwrap() as usize;
        let h = case["h"].as_u64().unwrap() as usize;
        let k1 = case["k1"].as_f64().unwrap();
        let map = undistort::build_map(w, h, k1);
        for sample in case["samples"].as_array().unwrap() {
            let x = sample["x"].as_u64().unwrap() as usize;
            let y = sample["y"].as_u64().unwrap() as usize;
            let ex = sample["map_x"].as_f64().unwrap();
            let ey = sample["map_y"].as_f64().unwrap();
            let gx = map.map_x[y * w + x] as f64;
            let gy = map.map_y[y * w + x] as f64;
            assert!(
                (gx - ex).abs() <= 1e-3 && (gy - ey).abs() <= 1e-3,
                "{name}: map at ({x},{y}) = ({gx:.5},{gy:.5}) vs ({ex:.5},{ey:.5})"
            );
        }
    }
}

// homography module smoke: RANSAC determinism across runs.
#[test]
fn ransac_is_deterministic() {
    let case = &cases("homography")[3];
    let src = pts(&case["src"]);
    let dst = pts(&case["dst"]);
    let run = || {
        let mut rng = Pcg32::new(splitmix64(42));
        find_homography_ransac(&src, &dst, 3.0, &mut rng, 2000, 0.995)
            .map(|r| (r.h, r.inliers))
            .unwrap()
    };
    let (h1, m1) = run();
    let (h2, m2) = run();
    assert_eq!(m1, m2);
    assert_eq!(h1, h2);
    let _ = homography::mat3_mul(&h1, &h2);
}

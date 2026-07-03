//! Golden tests against OpenCV/numpy outputs captured by
//! `tools/gen_cv_goldens.py` into `testdata/cv/` (committed).
//!
//! PNG channel rule (mirroring the generator): a 3-channel PNG's RGB
//! channels hold the original array's channels *verbatim* (channel 0 first),
//! so BGR/Lab/HSV arrays round-trip without any implicit swap.

use std::fs::File;
use std::path::{Path, PathBuf};

use nestris_vision::{Image, color, components, morphology, ncc, resize, stats, threshold};
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
        other => panic!("unexpected png color type {other:?} in {}", path.display()),
    };
    assert_eq!(info.bit_depth, png::BitDepth::Eight);
    Image::from_vec(buf, info.width as usize, info.height as usize, channels)
}

fn cases(domain: &str) -> Vec<Value> {
    let raw = std::fs::read_to_string(testdata(domain).join("cases.json"))
        .expect("cases.json (run tools/gen_cv_goldens.py)");
    serde_json::from_str(&raw).expect("cases.json parse")
}

/// Max per-channel absolute difference and the number of differing bytes.
fn diff_stats(a: &Image, b: &Image) -> (u32, usize) {
    assert_eq!(a.data.len(), b.data.len(), "size mismatch");
    let mut max = 0u32;
    let mut count = 0usize;
    for (&x, &y) in a.data.iter().zip(b.data.iter()) {
        let d = (x as i32 - y as i32).unsigned_abs();
        if d > 0 {
            count += 1;
            max = max.max(d);
        }
    }
    (max, count)
}

// ---------------------------------------------------------------------------
// color
// ---------------------------------------------------------------------------

#[test]
fn color_bgr_to_gray_byte_exact() {
    for case in cases("color") {
        let name = case["name"].as_str().unwrap();
        let dir = testdata("color");
        let input = load_png(&dir.join(format!("{name}_in.png")));
        let expected = load_png(&dir.join(format!("{name}_gray.png")));
        let got = color::bgr_to_gray(&input);
        let (max, count) = diff_stats(&got, &expected);
        assert_eq!((max, count), (0, 0), "{name}: gray mismatch");
    }
}

#[test]
fn color_bgr_to_lab_within_1_lsb() {
    for case in cases("color") {
        let name = case["name"].as_str().unwrap();
        let dir = testdata("color");
        let input = load_png(&dir.join(format!("{name}_in.png")));
        let expected = load_png(&dir.join(format!("{name}_lab.png")));
        let got = color::bgr_to_lab(&input);
        let (max, count) = diff_stats(&got, &expected);
        assert!(
            max <= 1,
            "{name}: lab max diff {max} ({count} bytes differ of {})",
            got.data.len()
        );
    }
}

#[test]
fn color_bgr_to_hsv_within_1_lsb() {
    for case in cases("color") {
        let name = case["name"].as_str().unwrap();
        let dir = testdata("color");
        let input = load_png(&dir.join(format!("{name}_in.png")));
        let expected = load_png(&dir.join(format!("{name}_hsv.png")));
        let got = color::bgr_to_hsv(&input);
        // Hue is circular modulo 180.
        let mut max = 0u32;
        for (px_g, px_e) in got.data.chunks_exact(3).zip(expected.data.chunks_exact(3)) {
            let dh = (px_g[0] as i32 - px_e[0] as i32).unsigned_abs();
            max = max.max(dh.min(180 - dh));
            max = max.max((px_g[1] as i32 - px_e[1] as i32).unsigned_abs());
            max = max.max((px_g[2] as i32 - px_e[2] as i32).unsigned_abs());
        }
        assert!(max <= 1, "{name}: hsv max diff {max}");
    }
}

#[test]
fn color_lab_to_bgr_close() {
    for case in cases("color") {
        let name = case["name"].as_str().unwrap();
        let dir = testdata("color");
        let lab = load_png(&dir.join(format!("{name}_lab.png")));
        let expected = load_png(&dir.join(format!("{name}_lab2bgr.png")));
        let got = color::lab_to_bgr(&lab);
        let (max, count) = diff_stats(&got, &expected);
        // Float inverse vs OpenCV's bit-exact integer inverse: allow 2 LSB;
        // the palette decision gate in the engine tests is the real check.
        assert!(
            max <= 2,
            "{name}: lab2bgr max diff {max} ({count} of {} bytes)",
            got.data.len()
        );
    }
}

// ---------------------------------------------------------------------------
// otsu
// ---------------------------------------------------------------------------

#[test]
fn otsu_threshold_exact() {
    for case in cases("otsu") {
        let name = case["name"].as_str().unwrap();
        let dir = testdata("otsu");
        let input = load_png(&dir.join(format!("{name}_in.png")));
        let expected_bin = load_png(&dir.join(format!("{name}_bin.png")));
        let expected_thresh = case["thresh"].as_f64().unwrap();
        let (thresh, binary) = threshold::threshold_binary_otsu(&input, 255);
        assert_eq!(thresh, expected_thresh, "{name}: threshold value");
        let (max, count) = diff_stats(&binary, &expected_bin);
        assert_eq!((max, count), (0, 0), "{name}: binary mismatch");
    }
}

// ---------------------------------------------------------------------------
// morphology
// ---------------------------------------------------------------------------

#[test]
fn morph_kernels_match_opencv_rasterization() {
    let raw = std::fs::read_to_string(testdata("morph").join("kernels.json")).unwrap();
    let kernels: Value = serde_json::from_str(&raw).unwrap();
    for (name, kernel) in [
        ("ellipse_3", morphology::Kernel::ellipse3()),
        ("ellipse_5", morphology::Kernel::ellipse5()),
    ] {
        let expected: Vec<u8> = kernels[name]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|row| row.as_array().unwrap().iter())
            .map(|v| v.as_u64().unwrap() as u8)
            .collect();
        assert_eq!(kernel.mask, expected, "{name} rasterization");
    }
}

#[test]
fn morph_ops_byte_exact() {
    let k3 = morphology::Kernel::ellipse3();
    let k5 = morphology::Kernel::ellipse5();
    for case in cases("morph") {
        let name = case["name"].as_str().unwrap();
        let dir = testdata("morph");
        let input = load_png(&dir.join(format!("{name}_in.png")));
        let ops: Vec<(&str, Image)> = vec![
            ("close_k3_i2", morphology::close(&input, &k3, 2)),
            ("open_k3_i1", morphology::open(&input, &k3, 1)),
            ("open_k5_i1", morphology::open(&input, &k5, 1)),
            ("erode_k3", morphology::erode(&input, &k3, 1)),
            ("dilate_k3", morphology::dilate(&input, &k3, 1)),
        ];
        for (op, got) in ops {
            let expected = load_png(&dir.join(format!("{name}_{op}.png")));
            let (max, count) = diff_stats(&got, &expected);
            assert_eq!((max, count), (0, 0), "{name}/{op} mismatch");
        }
    }
}

// ---------------------------------------------------------------------------
// ncc
// ---------------------------------------------------------------------------

#[test]
fn ncc_matches_opencv() {
    for case in cases("ncc") {
        let name = case["name"].as_str().unwrap();
        let dir = testdata("ncc");
        let image = load_png(&dir.join(format!("{name}_image.png")));
        let templ = load_png(&dir.join(format!("{name}_templ.png")));
        let got = ncc::match_template_ccoeff_normed(&image, &templ);
        let expected: Vec<Vec<f64>> = case["response"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                row.as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_f64().unwrap())
                    .collect()
            })
            .collect();
        assert_eq!(got.height, expected.len(), "{name}: response height");
        assert_eq!(got.width, expected[0].len(), "{name}: response width");
        let mut max_diff = 0.0f64;
        for (y, row) in expected.iter().enumerate() {
            for (x, &e) in row.iter().enumerate() {
                let d = (got.data[y * got.width + x] as f64 - e).abs();
                max_diff = max_diff.max(d);
            }
        }
        assert!(max_diff <= 1e-4, "{name}: ncc max diff {max_diff}");
        let (max_val, (mx, my)) = got.max();
        let exp_loc = case["max_loc"].as_array().unwrap();
        assert_eq!(
            (mx as u64, my as u64),
            (exp_loc[0].as_u64().unwrap(), exp_loc[1].as_u64().unwrap()),
            "{name}: max loc"
        );
        let exp_val = case["max_val"].as_f64().unwrap();
        assert!((max_val as f64 - exp_val).abs() <= 1e-4, "{name}: max val");
    }
}

// ---------------------------------------------------------------------------
// connected components
// ---------------------------------------------------------------------------

#[test]
fn components_decision_equal() {
    for case in cases("cc") {
        let name = case["name"].as_str().unwrap();
        let input = load_png(&testdata("cc").join(format!("{name}_in.png")));
        let labeled = components::connected_components(&input);
        let mut got: Vec<_> = labeled.components.clone();
        got.sort_by_key(|c| (std::cmp::Reverse(c.area), c.x, c.y));
        let expected = case["components"].as_array().unwrap();
        assert_eq!(got.len(), expected.len(), "{name}: component count");
        for (g, e) in got.iter().zip(expected.iter()) {
            assert_eq!(g.area as u64, e["area"].as_u64().unwrap(), "{name}: area");
            assert_eq!(g.x as u64, e["x"].as_u64().unwrap(), "{name}: x");
            assert_eq!(g.y as u64, e["y"].as_u64().unwrap(), "{name}: y");
            assert_eq!(g.w as u64, e["w"].as_u64().unwrap(), "{name}: w");
            assert_eq!(g.h as u64, e["h"].as_u64().unwrap(), "{name}: h");
            assert!(
                (g.cx - e["cx"].as_f64().unwrap()).abs() < 1e-9,
                "{name}: cx"
            );
            assert!(
                (g.cy - e["cy"].as_f64().unwrap()).abs() < 1e-9,
                "{name}: cy"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// resize
// ---------------------------------------------------------------------------

#[test]
fn resize_area_within_1_lsb() {
    for case in cases("resize") {
        let name = case["name"].as_str().unwrap();
        let dir = testdata("resize");
        let input = load_png(&dir.join(format!("{name}_in.png")));
        let expected = load_png(&dir.join(format!("{name}_out.png")));
        let got = resize::resize_area(
            &input,
            case["out_w"].as_u64().unwrap() as usize,
            case["out_h"].as_u64().unwrap() as usize,
        );
        let (max, count) = diff_stats(&got, &expected);
        assert!(
            max <= 1,
            "{name}: resize max diff {max} ({count} of {} bytes)",
            got.data.len()
        );
    }
}

// ---------------------------------------------------------------------------
// numpy stats
// ---------------------------------------------------------------------------

#[test]
fn npstats_match_numpy() {
    for case in cases("npstats") {
        let name = case["name"].as_str().unwrap();
        let data: Vec<f64> = case["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap())
            .collect();
        if let Some(percs) = case.get("percentiles").and_then(|p| p.as_object()) {
            for (q, expected) in percs {
                let got = stats::percentile(&data, q.parse::<f64>().unwrap());
                let e = expected.as_f64().unwrap();
                assert!((got - e).abs() <= 1e-12 * e.abs().max(1.0), "{name}: p{q}");
            }
            let med = stats::median(&data);
            assert!(
                (med - case["median"].as_f64().unwrap()).abs() <= 1e-12,
                "{name}: median"
            );
            let sd = stats::std_dev(&data);
            let e = case["std"].as_f64().unwrap();
            assert!((sd - e).abs() <= 1e-12 * e.max(1.0), "{name}: std");
        }
        if let Some(idx) = case.get("argsort").and_then(|a| a.as_array()) {
            let got = stats::argsort(&data);
            let expected: Vec<usize> = idx.iter().map(|v| v.as_u64().unwrap() as usize).collect();
            assert_eq!(got, expected, "{name}: argsort");
        }
    }
}

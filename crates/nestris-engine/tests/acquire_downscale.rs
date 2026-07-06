//! Downscaled candidate detection must not change lock quality: the same
//! synthetic source (a real rectified fixture re-projected into a 1280-wide
//! frame) has to solve with and without `downscale_width`, at nearly the
//! same confidence. Candidates only seed the label windows and 4 RANSAC
//! correspondences — labels/RANSAC/validation always run at full resolution.

use nestris_engine::geometry_cal::calibration::{SolveOptions, estimate_geometry_with};
use nestris_engine::layout::get_layout;
use nestris_vision::Image;

const SRC_W: usize = 1280;
const SRC_H: usize = 1120;
const SCALE: f64 = 4.0;
const BASE_TX: f64 = 128.0;
const BASE_TY: f64 = 80.0;

fn load_canonical_fixture() -> Image {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../testdata/cv/canon_tetris01.png"
    );
    let bytes = std::fs::read(path).expect("fixture png");
    let decoder = png::Decoder::new(&bytes[..]);
    let mut reader = decoder.read_info().expect("png info");
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).expect("png frame");
    buf.truncate(info.buffer_size());
    assert_eq!(info.color_type, png::ColorType::Rgb, "fixture is RGB");
    let mut bgr = Image::new(info.width as usize, info.height as usize, 3);
    for (dst, src) in bgr.data.chunks_exact_mut(3).zip(buf.chunks_exact(3)) {
        dst[0] = src[2];
        dst[1] = src[1];
        dst[2] = src[0];
    }
    bgr
}

/// Render the canonical fixture into a big synthetic source frame
/// (axis-aligned similarity: SCALE + translation, bilinear taps).
fn render_source(canon: &Image) -> Image {
    let inv_scale = 1.0 / SCALE;
    let mut out = Image::new(SRC_W, SRC_H, 3);
    for y in 0..SRC_H {
        for x in 0..SRC_W {
            let cx = (x as f64 - BASE_TX) * inv_scale;
            let cy = (y as f64 - BASE_TY) * inv_scale;
            let base = (y * SRC_W + x) * 3;
            if cx >= 0.0 && cy >= 0.0 && cx < 255.0 && cy < 239.0 {
                let (x0, y0) = (cx as usize, cy as usize);
                let (fx, fy) = (cx - x0 as f64, cy - y0 as f64);
                for ch in 0..3 {
                    let at = |xx: usize, yy: usize| {
                        f64::from(canon.data[(yy * canon.width + xx) * 3 + ch])
                    };
                    let v = at(x0, y0) * (1.0 - fx) * (1.0 - fy)
                        + at(x0 + 1, y0) * fx * (1.0 - fy)
                        + at(x0, y0 + 1) * (1.0 - fx) * fy
                        + at(x0 + 1, y0 + 1) * fx * fy;
                    out.data[base + ch] = v.round() as u8;
                }
            } else {
                out.data[base..base + 3].copy_from_slice(&[110, 110, 110]);
            }
        }
    }
    out
}

#[test]
fn downscaled_candidates_match_full_res_lock_quality() {
    let source = render_source(&load_canonical_fixture());
    let layout = get_layout();

    let full = estimate_geometry_with(&source, layout, None, false, 1, &SolveOptions::default());
    let down = estimate_geometry_with(
        &source,
        layout,
        None,
        false,
        1,
        &SolveOptions {
            downscale_width: 640,
        },
    );

    assert!(full.ok(), "full-res solve must lock");
    assert!(down.ok(), "downscaled solve must lock");
    assert!(
        (full.confidence - down.confidence).abs() < 0.05,
        "confidence delta too large: full={} downscaled={}",
        full.confidence,
        down.confidence
    );
}

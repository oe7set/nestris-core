//! Debug harness: run candidate detection + the geometry solve on one PNG
//! (BGR stored as RGB-reversed by cv2, or a plain RGB screenshot).
//!
//! ```sh
//! cargo run -p nestris-engine --example solve_debug -- frame.png [--cv2]
//! ```
//! `--cv2` marks the PNG as written by `cv2.imwrite` (RGB channels hold BGR).

use std::fs::File;

use nestris_engine::geometry_cal::anchors::{detect_playfield_candidates, hud_constellation_score};
use nestris_engine::geometry_cal::calibration::estimate_geometry;
use nestris_engine::layout::get_layout;
use nestris_vision::Image;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: solve_debug <png> [--cv2]");
    let cv2_channels = args.next().as_deref() == Some("--cv2");

    let decoder = png::Decoder::new(File::open(&path).expect("open png"));
    let mut reader = decoder.read_info().expect("png info");
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).expect("png frame");
    buf.truncate(info.buffer_size());
    assert_eq!(info.color_type, png::ColorType::Rgb, "need 8-bit RGB png");
    if !cv2_channels {
        // Ordinary RGB png -> engine-native BGR.
        for px in buf.chunks_exact_mut(3) {
            px.swap(0, 2);
        }
    }
    let image = Image::from_vec(buf, info.width as usize, info.height as usize, 3);
    println!("shape: {}x{}", image.width, image.height);

    let layout = get_layout();
    let gray = nestris_engine::palette::to_luma(&image);
    for (i, cand) in detect_playfield_candidates(&image, 6).iter().enumerate() {
        let constellation = hud_constellation_score(&gray, cand, layout);
        println!(
            "cand {i}: score={:.3} aspect={:.2} area={:.3} quad_tl=({:.1}, {:.1}) constellation={:.3}",
            cand.score, cand.aspect, cand.area_frac, cand.quad.tl.0, cand.quad.tl.1, constellation
        );
    }
    let result = estimate_geometry(&image, layout, None, true, 1010);
    println!(
        "estimate_geometry: ok={} conf={:.3} resid={:.2}",
        result.ok(),
        result.confidence,
        result.residual
    );
}

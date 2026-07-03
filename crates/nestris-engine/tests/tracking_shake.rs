//! Synthetic-shake integration test for the continuous geometry tracker.
//!
//! A real rectified fixture frame is re-projected through a jittering
//! homography (sinusoidal translation + rotation, like handheld phone
//! footage) into a synthetic source video. With tracking enabled the lock
//! must follow the motion and keep the canonical frame aligned; with
//! tracking disabled the same sequence visibly misaligns. Alignment is
//! measured as the tracker's own mean label displacement on the produced
//! canonical frames (a pure measurement here — corrections are not applied
//! by the probe).

use nestris_engine::config::{EngineConfig, TrackingConfig};
use nestris_engine::frame::Frame;
use nestris_engine::geometry_cal::lock::LockState;
use nestris_engine::geometry_cal::tracker::LocalTracker;
use nestris_engine::layout::get_layout;
use nestris_engine::palette::to_luma;
use nestris_engine::processor::FrameProcessor;
use nestris_vision::Image;

const SRC_W: usize = 640;
const SRC_H: usize = 560;
const FRAMES: usize = 140;
const FPS: f64 = 60.0;
/// Base placement of the canonical content in the synthetic source.
const SCALE: f64 = 2.0;
const BASE_TX: f64 = 64.0;
const BASE_TY: f64 = 40.0;
/// Frames to skip before measuring (acquisition + jitter ramp-in).
const SETTLE: usize = 20;

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
    // Fixture is RGB; the engine consumes BGR.
    let mut bgr = Image::new(info.width as usize, info.height as usize, 3);
    for (dst, src) in bgr.data.chunks_exact_mut(3).zip(buf.chunks_exact(3)) {
        dst[0] = src[2];
        dst[1] = src[1];
        dst[2] = src[0];
    }
    bgr
}

/// The jitter pose for frame `k`: handheld-like sway (~1.5 Hz at 60 fps,
/// ±10 source px + slight rotation), ramping in over the first frames so
/// the initial acquisition sees a still image.
fn pose(k: usize) -> (f64, f64, f64) {
    let ramp = (k as f64 / 12.0).min(1.0);
    let dx = 10.0 * (k as f64 * 0.16).sin() * ramp;
    let dy = 8.0 * (k as f64 * 0.13).cos() * ramp;
    let rot = 0.008 * (k as f64 * 0.07).sin() * ramp;
    (dx, dy, rot)
}

/// Render the canonical fixture into a synthetic source frame under a
/// similarity pose (canonical -> source): rotation, SCALE, translation.
fn render_source(canon: &Image, dx: f64, dy: f64, rot: f64) -> Image {
    let (s, c) = rot.sin_cos();
    // source = R*S*canon + t  =>  canon = (R*S)^-1 * (source - t)
    let (tx, ty) = (BASE_TX + dx, BASE_TY + dy);
    let inv_scale = 1.0 / SCALE;
    let mut out = Image::new(SRC_W, SRC_H, 3);
    for y in 0..SRC_H {
        for x in 0..SRC_W {
            let (px, py) = (x as f64 - tx, y as f64 - ty);
            let cx = (c * px + s * py) * inv_scale;
            let cy = (-s * px + c * py) * inv_scale;
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

/// Run the pipeline over the jittered sequence; returns (mean alignment
/// error, max alignment error, final lock state).
fn run(tracking: bool) -> (f64, f64, LockState) {
    let mut config = EngineConfig::default();
    config.calibration.undistort = "off".into();
    // Background mode with no host solver: after acquisition the per-frame
    // path is dark-fraction + tracker only — the GUI-like configuration.
    config.calibration.background_recalibration = true;
    config.tracking = TrackingConfig {
        enabled: tracking,
        ..TrackingConfig::default()
    };
    let mut processor = FrameProcessor::new(config);

    // Independent measurement probe (never applies corrections).
    let mut probe = LocalTracker::new(
        TrackingConfig {
            enabled: true,
            search_radius_px: 16, // wide, so big misalignment is measurable
            min_label_score: 0.3,
            ..TrackingConfig::default()
        },
        get_layout(),
    );

    let canon_fixture = load_canonical_fixture();
    let mut worst = 0.0f64;
    let mut sum = 0.0f64;
    let mut samples = 0usize;
    let mut state = LockState::Unlocked;
    for k in 0..FRAMES {
        let (dx, dy, rot) = pose(k);
        let source = render_source(&canon_fixture, dx, dy, rot);
        let frame = Frame {
            image: source,
            seq: k as i64,
            ts: k as f64 / FPS,
            discontinuity: false,
        };
        processor.process(&frame);
        state = processor.lock_state();
        if k >= SETTLE {
            // Past the ramp: measure how well the canonical stays aligned.
            // Note the probe sees each frame BEFORE that frame's correction,
            // so even a perfect tracker shows one frame's fresh motion here.
            if let Some(canon) = processor.last_canonical() {
                let outcome = probe.step(&to_luma(canon));
                if outcome.matched >= 2 {
                    if std::env::var_os("SHAKE_TRACE").is_some() {
                        eprintln!(
                            "k={k:3} tracking={tracking} shift={:.2} matched={} state={:?}",
                            outcome.mean_shift, outcome.matched, state
                        );
                    }
                    worst = worst.max(outcome.mean_shift);
                    sum += outcome.mean_shift;
                    samples += 1;
                }
            }
        }
    }
    assert!(samples > (FRAMES - SETTLE) / 2, "probe must keep matching");
    let mean = sum / samples as f64;
    if std::env::var_os("SHAKE_TRACE").is_some() {
        eprintln!("summary tracking={tracking}: mean={mean:.2} worst={worst:.2} ({samples} samples)");
    }
    (mean, worst, state)
}

#[test]
fn tracker_follows_synthetic_handheld_shake() {
    let (mean_on, worst_on, state_on) = run(true);
    let (mean_off, worst_off, _) = run(false);

    assert_eq!(
        state_on,
        LockState::Locked,
        "lock must ride out the shake with tracking on"
    );
    // Absolute quality: residual + one frame of fresh motion stays inside
    // the digit re-centering radius (2 px) on average, bounded at peaks.
    assert!(
        mean_on < 1.6,
        "tracked mean alignment error {mean_on:.2}px too high"
    );
    assert!(
        worst_on < 4.5,
        "tracked worst alignment error {worst_on:.2}px too high"
    );
    // Relative quality: tracking must clearly beat the static lock.
    assert!(
        mean_off > mean_on * 2.0,
        "untracked mean {mean_off:.2}px should clearly exceed tracked {mean_on:.2}px"
    );
    assert!(
        worst_off > worst_on * 1.5,
        "untracked worst {worst_off:.2}px should clearly exceed tracked {worst_on:.2}px"
    );
}

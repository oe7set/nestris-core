//! Lock state-machine tests for host-driven background acquisition:
//! scripted `offer_solution` results must mirror the inline `acquire`
//! semantics (threshold gating, streak counting, streak reset on failure).

use nestris_engine::config::CalibrationConfig;
use nestris_engine::geometry_cal::calibration::GeometryResult;
use nestris_engine::geometry_cal::lock::{CalibrationLock, LockState};
use nestris_vision::Image;

const IDENTITY: [f64; 9] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];

fn background_lock() -> CalibrationLock {
    let cfg = CalibrationConfig {
        background_recalibration: true,
        background_acquisition: true,
        ..CalibrationConfig::default()
    };
    CalibrationLock::new(cfg)
}

fn ok_result(confidence: f64) -> GeometryResult {
    GeometryResult {
        homography: Some(IDENTITY),
        confidence,
        undistort: None,
        residual: 0.5,
    }
}

fn failed_result(confidence: f64) -> GeometryResult {
    GeometryResult {
        homography: None,
        confidence,
        undistort: None,
        residual: f64::INFINITY,
    }
}

fn frame() -> Image {
    Image::new(64, 64, 3)
}

#[test]
fn locks_after_acquire_frames_ok_results() {
    let mut lock = background_lock();
    let img = frame();

    // No result yet: stays acquiring, asks the host for a solve, and never
    // runs the inline solve (a black frame would otherwise fail it anyway).
    let status = lock.prepare(&img, false);
    assert_eq!(status.state, LockState::Acquiring);
    assert!(lock.wants_background_solve());
    assert!(!status.usable);

    lock.offer_solution(ok_result(0.9));
    let status = lock.prepare(&img, false);
    assert_eq!(status.state, LockState::Acquiring); // streak 1 of 2

    lock.offer_solution(ok_result(0.9));
    let status = lock.prepare(&img, false);
    assert_eq!(status.state, LockState::Locked);
    assert!(status.usable);
    assert!(lock.rectifier().is_some());
}

#[test]
fn failed_result_resets_streak() {
    let mut lock = background_lock();
    let img = frame();

    lock.offer_solution(ok_result(0.9));
    lock.prepare(&img, false); // streak 1

    lock.offer_solution(failed_result(0.2));
    let status = lock.prepare(&img, false);
    assert_eq!(status.state, LockState::Acquiring); // streak reset

    // One more ok result is not enough after the reset...
    lock.offer_solution(ok_result(0.9));
    let status = lock.prepare(&img, false);
    assert_eq!(status.state, LockState::Acquiring);

    // ...the second consecutive one locks.
    lock.offer_solution(ok_result(0.9));
    let status = lock.prepare(&img, false);
    assert_eq!(status.state, LockState::Locked);
}

#[test]
fn below_threshold_never_adopts() {
    let mut lock = background_lock();
    let img = frame();

    for _ in 0..5 {
        lock.offer_solution(ok_result(0.3)); // below acquire_threshold 0.55
        let status = lock.prepare(&img, false);
        assert_eq!(status.state, LockState::Acquiring);
        assert!(!status.usable);
        assert!(lock.rectifier().is_none());
    }
}

#[test]
fn failed_offer_does_not_clobber_pending_ok() {
    let mut lock = background_lock();
    let img = frame();

    lock.prepare(&img, false);
    lock.offer_solution(ok_result(0.9));
    lock.offer_solution(failed_result(0.0)); // must not replace the ok one
    lock.prepare(&img, false); // adopts the ok result: streak 1
    lock.offer_solution(ok_result(0.9));
    let status = lock.prepare(&img, false);
    assert_eq!(status.state, LockState::Locked);
}

#[test]
fn acquisition_solves_pace_back_to_back() {
    let mut lock = background_lock();
    let img = frame();
    lock.prepare(&img, false);
    assert_eq!(lock.solve_interval_hint(), Some(0.0));

    // Once locked, the acquisition hint must yield to normal pacing.
    lock.offer_solution(ok_result(0.9));
    lock.prepare(&img, false);
    lock.offer_solution(ok_result(0.9));
    lock.prepare(&img, false);
    assert_ne!(lock.solve_interval_hint(), Some(0.0));
}

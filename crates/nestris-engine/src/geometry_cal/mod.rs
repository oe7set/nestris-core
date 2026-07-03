//! Geometry calibration: anchors, solver/rectifier, undistort estimation,
//! and the per-frame lock state machine.

pub mod anchors;
pub mod calibration;
pub mod lock;
pub mod tracker;
pub mod undistort_est;

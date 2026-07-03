//! The sans-io NES-Tetris OCR engine.
//!
//! `Processor::process(&mut self, FrameView) -> OutputFrame` is the single
//! synchronous entry point; hosts (CLI thread, Web Worker, Android) supply
//! frames and drive the background-recalibration protocol
//! (`wants_background_solve` / `offer_solution`). The JSON output reproduces
//! the Python implementation's schema v4 field-for-field.

pub mod config;
pub mod enums;
pub mod frame;
pub mod geometry;
pub mod geometry_cal;
pub mod layout;
pub mod nes_palette;
pub mod output;
pub mod palette;
pub mod processor;
pub mod recognition;
pub mod state;
pub mod stats;
pub mod templates;

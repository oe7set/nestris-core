//! The sans-io NES-Tetris OCR engine.
//!
//! `Processor::process(&mut self, FrameView) -> OutputFrame` is the single
//! synchronous entry point; hosts (CLI thread, Web Worker, Android) supply
//! frames and drive the background-recalibration protocol
//! (`wants_background_solve` / `offer_solution`). The JSON output reproduces
//! the Python implementation's schema v4 field-for-field.

pub mod enums;
pub mod geometry;
pub mod layout;
pub mod nes_palette;
pub mod palette;
pub mod recognition;
pub mod templates;

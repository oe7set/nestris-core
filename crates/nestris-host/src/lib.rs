//! Native host utilities shared by the CLI and the desktop GUI:
//! ffmpeg-pipe video capture, output sinks, and the background-recalibration
//! worker thread. Everything here is native-only glue around the sans-io
//! engine — nothing in this crate is needed by the wasm or Android hosts.

pub mod capture_ffmpeg;
pub mod recalib_thread;
pub mod sinks;

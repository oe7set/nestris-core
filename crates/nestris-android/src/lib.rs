//! Android UniFFI bindings (Phase-9 stub: the interface an app builds on;
//! camera integration, NV21 fast path, and threading policy come with the
//! app itself).
//!
//! Kotlin usage sketch:
//! ```kotlin
//! val engine = NestrisEngine("")            // config JSON ("" = defaults)
//! val json = engine.pushFrame(rgba, w, h, ts) // OutputFrame JSON per frame
//! ```

use std::sync::Mutex;

use nestris_engine::config::EngineConfig;
use nestris_engine::frame::Frame;
use nestris_engine::geometry_cal::calibration::{GeometryResult, estimate_geometry};
use nestris_engine::layout::get_layout;
use nestris_engine::processor::FrameProcessor;

uniffi::setup_scaffolding!();

fn parse_config(config_json: &str) -> EngineConfig {
    if config_json.trim().is_empty() {
        EngineConfig::default()
    } else {
        serde_json::from_str(config_json).unwrap_or_default()
    }
}

/// The engine as a UniFFI object; interior mutability because UniFFI hands
/// out `Arc<Self>` across the FFI.
#[derive(uniffi::Object)]
pub struct NestrisEngine {
    processor: Mutex<FrameProcessor>,
    seq: Mutex<i64>,
}

#[uniffi::export]
impl NestrisEngine {
    /// Create an engine; `config_json` mirrors the CLI's TOML config as JSON
    /// (empty = defaults).
    #[uniffi::constructor]
    pub fn new(config_json: String) -> Self {
        Self {
            processor: Mutex::new(FrameProcessor::new(parse_config(&config_json))),
            seq: Mutex::new(0),
        }
    }

    /// Process one RGBA frame (`width*height*4` bytes); returns the
    /// OutputFrame JSON (schema v4).
    pub fn push_frame(&self, data: Vec<u8>, width: u32, height: u32, ts: f64) -> String {
        let seq = {
            let mut guard = self.seq.lock().unwrap();
            let s = *guard;
            *guard += 1;
            s
        };
        let frame = Frame::from_rgba(&data, width as usize, height as usize, seq, ts);
        self.processor.lock().unwrap().process(&frame).to_json()
    }

    /// Whether the host should run a background solve (drive from a Kotlin
    /// coroutine / worker thread via [`NestrisEngine::solve_frame`]).
    pub fn wants_background_solve(&self) -> bool {
        self.processor
            .lock()
            .unwrap()
            .lock()
            .wants_background_solve()
    }

    /// Run a geometry solve on an RGBA frame snapshot (call off the frame
    /// thread) and hand the result to the lock.
    pub fn solve_frame(&self, data: Vec<u8>, width: u32, height: u32, seed: u64) {
        let frame = Frame::from_rgba(&data, width as usize, height as usize, 0, 0.0);
        let result: GeometryResult =
            estimate_geometry(&frame.image, get_layout(), None, false, seed);
        if result.ok() {
            self.processor.lock().unwrap().lock().offer_solution(result);
        }
    }

    /// Signal a source switch/seek: temporal tracking resets, lock kept.
    pub fn mark_discontinuity(&self) {
        self.processor.lock().unwrap().reset_tracking();
    }

    pub fn reset_lock(&self) {
        self.processor.lock().unwrap().reset_lock();
    }

    pub fn lock_state(&self) -> String {
        self.processor
            .lock()
            .unwrap()
            .lock_state()
            .name()
            .to_string()
    }
}

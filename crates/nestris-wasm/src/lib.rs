//! WebAssembly bindings: one engine instance per page plus a standalone
//! `Solver` for the recalibration Web Worker.
//!
//! Per-frame JS↔WASM contract (exactly one copy per frame): JS asks for
//! `frame_ptr(w, h)`, copies the RGBA `ImageData` bytes into linear memory,
//! then calls `process(ts)` and receives the OutputFrame JSON. Overlay
//! drawing reads `lock_quad()` / `canonical_rgba()` without JSON parsing.

use wasm_bindgen::prelude::*;

pub mod snapshot;

use std::collections::VecDeque;

use nestris_engine::config::EngineConfig;
use nestris_engine::frame::Frame;
use nestris_engine::geometry_cal::calibration::{GeometryResult, estimate_geometry};
use nestris_engine::layout::get_layout;
use nestris_engine::output::OutputFrame;
use nestris_engine::processor::FrameProcessor;
use nestris_ngf::recorder::{GameRecorder, RecorderConfig, RecorderEvent};
use nestris_vision::homography::{Mat3, mat3_inv, project};

/// Snapshot lock-state code for replay frames (no live geometry).
const LOCK_CODE_REPLAY: u8 = 5;

fn parse_config(config_json: &str) -> EngineConfig {
    if config_json.trim().is_empty() {
        EngineConfig::default()
    } else {
        serde_json::from_str(config_json).unwrap_or_default()
    }
}

/// The main-thread engine: feeds frames, emits OutputFrame JSON.
#[wasm_bindgen]
pub struct Engine {
    processor: FrameProcessor,
    rgba: Vec<u8>,
    width: usize,
    height: usize,
    seq: i64,
    /// Per-game NGF recorder (on by default; `set_recording` toggles).
    recorder: Option<GameRecorder>,
    /// Finished recordings as gzipped .ngf.gz bytes, awaiting pickup by JS.
    finished_games: VecDeque<Vec<u8>>,
    /// Persistent buffers read by JS via ptr/len (no per-frame Vec returns).
    snapshot_buf: Vec<u8>,
    canon_buf: Vec<u8>,
}

fn browser_recorder() -> GameRecorder {
    GameRecorder::new(RecorderConfig {
        // Browser sources (file/camera) often start mid-game; record those
        // partial games too instead of waiting for a new-game boundary.
        record_partial: true,
        ..Default::default()
    })
}

#[wasm_bindgen]
impl Engine {
    #[wasm_bindgen(constructor)]
    pub fn new(config_json: &str) -> Engine {
        Engine {
            processor: FrameProcessor::new(parse_config(config_json)),
            rgba: Vec::new(),
            width: 0,
            height: 0,
            seq: 0,
            recorder: Some(browser_recorder()),
            finished_games: VecDeque::new(),
            snapshot_buf: Vec::new(),
            canon_buf: Vec::new(),
        }
    }

    /// Pointer to an RGBA frame buffer of `width*height*4` bytes; JS copies
    /// `ImageData.data` here before calling [`Engine::process`].
    pub fn frame_ptr(&mut self, width: usize, height: usize) -> *mut u8 {
        self.width = width;
        self.height = height;
        self.rgba.resize(width * height * 4, 0);
        self.rgba.as_mut_ptr()
    }

    fn process_inner(&mut self, ts: f64) -> OutputFrame {
        let mut frame = Frame::from_rgba(&self.rgba, self.width, self.height, self.seq, ts);
        frame.ts = ts;
        self.seq += 1;
        let output = self.processor.process(&frame);
        if let Some(recorder) = &mut self.recorder {
            for event in recorder.push(&output) {
                if let RecorderEvent::GameFinished { bytes, .. } = event
                    && let Ok(gz) = nestris_ngf::io::compress_gz(&bytes)
                {
                    self.finished_games.push_back(gz);
                }
            }
        }
        output
    }

    /// Process the frame currently in the buffer; returns OutputFrame JSON.
    /// Kept for compatibility and A/B checks — the app uses
    /// [`Engine::process_snapshot`].
    pub fn process(&mut self, ts: f64) -> String {
        self.process_inner(ts).to_json()
    }

    /// Process the frame currently in the buffer into the binary snapshot
    /// buffer (see `snapshot.rs` for the layout); returns its byte length.
    /// Read via [`Engine::snapshot_ptr`].
    pub fn process_snapshot(&mut self, ts: f64) -> usize {
        let output = self.process_inner(ts);
        let quad = self.lock_quad_array();
        snapshot::encode_snapshot(
            &output,
            snapshot::lock_state_code(self.processor.lock_state()),
            quad,
            self.recorder.is_some(),
            &mut self.snapshot_buf,
        );
        self.snapshot_buf.len()
    }

    pub fn snapshot_ptr(&self) -> *const u8 {
        self.snapshot_buf.as_ptr()
    }

    /// Copy the last rectified canonical frame (RGBA 256x240) into the
    /// persistent canonical buffer; returns its byte length (0 = no lock).
    /// Read via [`Engine::canonical_ptr`].
    pub fn canonical_update(&mut self) -> usize {
        let Some(canon) = self.processor.last_canonical() else {
            self.canon_buf.clear();
            return 0;
        };
        self.canon_buf.clear();
        self.canon_buf.reserve(canon.width * canon.height * 4);
        for px in canon.data.chunks_exact(3) {
            self.canon_buf.extend_from_slice(&[px[2], px[1], px[0], 255]);
        }
        self.canon_buf.len()
    }

    pub fn canonical_ptr(&self) -> *const u8 {
        self.canon_buf.as_ptr()
    }

    fn lock_quad_array(&mut self) -> Option<[f64; 8]> {
        let rectifier = self.processor.lock().rectifier()?;
        let inv = mat3_inv(rectifier.matrix())?;
        let corners = [(0.0, 0.0), (256.0, 0.0), (256.0, 240.0), (0.0, 240.0)];
        let mut out = [0.0f64; 8];
        for (i, &(x, y)) in corners.iter().enumerate() {
            let (sx, sy) = project(&inv, x, y);
            out[i * 2] = sx;
            out[i * 2 + 1] = sy;
        }
        Some(out)
    }

    /// Enable or disable per-game NGF recording (enabled by default).
    /// Disabling discards any partially recorded game.
    pub fn set_recording(&mut self, enabled: bool) {
        match (enabled, self.recorder.is_some()) {
            (true, false) => self.recorder = Some(browser_recorder()),
            (false, true) => self.recorder = None,
            _ => {}
        }
    }

    pub fn recording(&self) -> bool {
        self.recorder.is_some()
    }

    /// Whether a finished game recording is waiting for pickup.
    pub fn has_finished_game(&self) -> bool {
        !self.finished_games.is_empty()
    }

    /// Take the oldest finished recording as gzipped .ngf.gz bytes
    /// (empty when none is pending).
    pub fn take_finished_game(&mut self) -> Vec<u8> {
        self.finished_games.pop_front().unwrap_or_default()
    }

    /// Signal a stream discontinuity (source switch / seek) before the next
    /// frame: temporal tracking resets, the geometry lock is kept. A partial
    /// game recording is dropped — its frame continuity is torn.
    pub fn mark_discontinuity(&mut self) {
        self.processor.reset_tracking();
        if let Some(recorder) = &mut self.recorder {
            recorder.abort();
        }
    }

    pub fn reset_lock(&mut self) {
        self.processor.reset_lock();
    }

    /// Whether the host should run a background solve on the current frame
    /// (drive from a Web Worker via [`Solver`], feed back via
    /// [`Engine::offer_solution`]).
    pub fn wants_background_solve(&mut self) -> bool {
        self.processor.lock().wants_background_solve()
    }

    /// Milliseconds the host should currently wait between background
    /// solves: drops to the fast drift interval when the geometry tracker
    /// reports urgency (misses or sustained handheld motion).
    pub fn solve_interval_ms(&mut self) -> f64 {
        self.processor
            .lock()
            .solve_interval_hint()
            .map(|s| s * 1000.0)
            .unwrap_or(500.0)
    }

    /// Adopt a Worker-computed solve: 9 homography values + confidence.
    pub fn offer_solution(&mut self, h: Vec<f64>, confidence: f64) {
        if h.len() != 9 {
            return;
        }
        let mut mat: Mat3 = [0.0; 9];
        mat.copy_from_slice(&h);
        self.processor.lock().offer_solution(GeometryResult {
            homography: Some(mat),
            confidence,
            undistort: None,
            residual: 0.0,
        });
    }

    /// Source-space corners of the canonical raster (tl,tr,br,bl as x,y
    /// pairs, 8 values), for the raw-preview overlay. Empty when unlocked.
    pub fn lock_quad(&mut self) -> Vec<f64> {
        let Some(rectifier) = self.processor.lock().rectifier() else {
            return Vec::new();
        };
        let Some(inv) = mat3_inv(rectifier.matrix()) else {
            return Vec::new();
        };
        let corners = [(0.0, 0.0), (256.0, 0.0), (256.0, 240.0), (0.0, 240.0)];
        corners
            .iter()
            .flat_map(|&(x, y)| {
                let (sx, sy) = project(&inv, x, y);
                [sx, sy]
            })
            .collect()
    }

    /// The last rectified canonical frame as RGBA bytes (256*240*4), for the
    /// canonical preview canvas. Empty when no lock.
    pub fn canonical_rgba(&self) -> Vec<u8> {
        let Some(canon) = self.processor.last_canonical() else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(canon.width * canon.height * 4);
        for px in canon.data.chunks_exact(3) {
            out.extend_from_slice(&[px[2], px[1], px[0], 255]);
        }
        out
    }

    pub fn canonical_width(&self) -> usize {
        get_layout();
        256
    }

    pub fn canonical_height(&self) -> usize {
        240
    }

    pub fn lock_state(&self) -> String {
        self.processor.lock_state().name().to_string()
    }
}

/// A loaded NGF replay: random access to recorded games with statistics
/// re-derived by the same stats engine the live pipeline uses.
#[wasm_bindgen]
pub struct Replay {
    engine: nestris_ngf::replay::ReplayEngine,
    snapshot_buf: Vec<u8>,
}

#[wasm_bindgen]
impl Replay {
    /// Decode raw or gzipped NGF bytes. Throws on malformed input.
    #[wasm_bindgen(constructor)]
    pub fn new(bytes: &[u8]) -> Result<Replay, JsValue> {
        let file = nestris_ngf::replay::ReplayFile::from_bytes(bytes)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(Replay {
            engine: nestris_ngf::replay::ReplayEngine::new(file),
            snapshot_buf: Vec::new(),
        })
    }

    pub fn frame_count(&self) -> usize {
        self.engine.frame_count()
    }

    pub fn duration_ms(&self) -> u32 {
        self.engine.duration_ms()
    }

    pub fn ctime_ms_at(&self, index: usize) -> u32 {
        self.engine.ctime_ms_at(index)
    }

    /// Frame index at or before a recording timestamp (for seek bars).
    pub fn index_at_ms(&self, ctime_ms: u32) -> usize {
        self.engine.index_at_ms(ctime_ms)
    }

    /// The OutputFrame JSON at `index` (clamped), stats included.
    pub fn output_at(&mut self, index: usize) -> String {
        self.engine.output_at(index).to_json()
    }

    /// Binary snapshot of the frame at `index` (same layout as the live
    /// engine's snapshots); returns the byte length, read via
    /// [`Replay::snapshot_ptr`].
    pub fn snapshot_at(&mut self, index: usize) -> usize {
        let output = self.engine.output_at(index);
        snapshot::encode_snapshot(&output, LOCK_CODE_REPLAY, None, false, &mut self.snapshot_buf);
        self.snapshot_buf.len()
    }

    pub fn snapshot_ptr(&self) -> *const u8 {
        self.snapshot_buf.as_ptr()
    }
}

/// Standalone geometry solver for the recalibration Web Worker: its own wasm
/// instance receives downscaled/raw RGBA frames and returns solve JSON.
#[wasm_bindgen]
pub struct Solver {
    rgba: Vec<u8>,
    width: usize,
    height: usize,
}

#[wasm_bindgen]
impl Solver {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Solver {
        Solver {
            rgba: Vec::new(),
            width: 0,
            height: 0,
        }
    }

    pub fn frame_ptr(&mut self, width: usize, height: usize) -> *mut u8 {
        self.width = width;
        self.height = height;
        self.rgba.resize(width * height * 4, 0);
        self.rgba.as_mut_ptr()
    }

    /// Solve the buffered frame; returns `{"h":[...9],"confidence":x}` JSON
    /// or `"null"` when no geometry was found.
    pub fn solve(&mut self, seed: u64) -> String {
        let frame = Frame::from_rgba(&self.rgba, self.width, self.height, 0, 0.0);
        let result = estimate_geometry(&frame.image, get_layout(), None, false, seed);
        match result.homography {
            Some(h) => format!(
                "{{\"h\":{:?},\"confidence\":{}}}",
                h.to_vec(),
                result.confidence
            ),
            None => "null".to_string(),
        }
    }
}

impl Default for Solver {
    fn default() -> Self {
        Self::new()
    }
}

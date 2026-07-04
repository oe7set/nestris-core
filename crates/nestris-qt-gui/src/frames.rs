//! Frame data shared between the Qt thread (writer, in `apply_update`)
//! and the painted items' `paint` calls (readers): the raw preview, the
//! canonical frame, and the tracked playfield. Scalar values travel as
//! qproperties instead.

use std::sync::Mutex;

use nestris_engine::enums::Piece;

pub struct FrameStore {
    /// Downscaled raw frame as RGBA plus its dimensions.
    pub raw: Option<(Vec<u8>, usize, usize)>,
    /// Canonical 256×240 RGBA (when locked).
    pub canon: Option<Vec<u8>>,
    /// Detected-playfield quad in raw-preview coordinates.
    pub lock_quad: Option<[(f32, f32); 4]>,
    /// 20×10 cell ids (0 empty, 1 white, 2 accent A, 3 accent B).
    pub grid: Option<Vec<Vec<u8>>>,
    /// Level for the NES palette accent pair.
    pub level: Option<i64>,
    /// Falling piece, drawn over the settled cells in its guideline color.
    pub piece: Option<Piece>,
    pub piece_cells: Vec<(u32, u32)>,
}

pub static FRAMES: Mutex<FrameStore> = Mutex::new(FrameStore {
    raw: None,
    canon: None,
    lock_quad: None,
    grid: None,
    level: None,
    piece: None,
    piece_cells: Vec::new(),
});

/// Reset everything (a new source opens).
pub fn clear() {
    let mut store = FRAMES.lock().unwrap();
    store.raw = None;
    store.canon = None;
    store.lock_quad = None;
    store.grid = None;
    store.level = None;
    store.piece = None;
    store.piece_cells.clear();
}

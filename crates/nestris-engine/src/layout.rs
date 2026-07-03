//! Canonical tile-coordinate table (verbatim port of `regions/layout.py`).
//!
//! One region-agnostic layout: playfield anchored at tiles (12, 5) spanning
//! 10×20 cells on the 256×240 canonical raster.

use crate::enums::Piece;
use crate::geometry::Rect;

/// One tile is 8×8 pixels on the NES.
pub const TILE: f64 = 8.0;
pub const CANON_WIDTH: usize = 256;
pub const CANON_HEIGHT: usize = 240;
pub const PLAYFIELD_COLS: usize = 10;
pub const PLAYFIELD_ROWS: usize = 20;

/// STATISTICS row order in the licensed ROM, top to bottom.
pub const STATS_ORDER: [Piece; 7] = [
    Piece::T,
    Piece::J,
    Piece::Z,
    Piece::O,
    Piece::S,
    Piece::L,
    Piece::I,
];

const fn tiles(col: f64, row: f64, w_tiles: f64, h_tiles: f64) -> Rect {
    Rect::new(col * TILE, row * TILE, w_tiles * TILE, h_tiles * TILE)
}

/// Pixel rectangles for every readable field on the canonical raster.
pub struct LayoutTable {
    pub score: Rect,
    pub lines: Rect,
    pub level: Rect,
    pub next_box: Rect,
    pub playfield: Rect,
    pub spawn_zone: Rect,
    pub statistics: [Rect; 7],
    pub statistics_glyphs: [Rect; 7],
    pub label_lines: Rect,
    pub label_score: Rect,
    pub label_next: Rect,
    pub label_level: Rect,
    pub label_statistics: Rect,
    pub score_digits: usize,
    pub score_max_digits: usize,
    pub lines_digits: usize,
    pub level_digits: usize,
    pub stats_digits: usize,
}

impl LayoutTable {
    /// Pixel rectangle of playfield cell `(row, col)` (row 0 = top).
    pub fn playfield_cell(&self, row: usize, col: usize) -> Rect {
        let cw = self.playfield.w / PLAYFIELD_COLS as f64;
        let ch = self.playfield.h / PLAYFIELD_ROWS as f64;
        Rect::new(
            self.playfield.x + col as f64 * cw,
            self.playfield.y + row as f64 * ch,
            cw,
            ch,
        )
    }

    /// Pixel rectangle of one digit within a numeric field.
    pub fn digit_cell(&self, field: &Rect, digit_count: usize, index: usize) -> Rect {
        let dw = field.w / digit_count as f64;
        Rect::new(field.x + index as f64 * dw, field.y, dw, field.h)
    }
}

const STATS_DIGITS: usize = 3;
const STATS_ROW0: f64 = 11.0;
const STATS_ROW_STEP: f64 = 2.0;
const STATS_DIGIT_COL: f64 = 6.0;
const STATS_GLYPH_COL: f64 = 3.0;

fn stats_rows() -> [Rect; 7] {
    std::array::from_fn(|i| {
        tiles(
            STATS_DIGIT_COL,
            STATS_ROW0 + i as f64 * STATS_ROW_STEP,
            STATS_DIGITS as f64,
            1.0,
        )
    })
}

fn stats_glyph_rows() -> [Rect; 7] {
    std::array::from_fn(|i| {
        tiles(
            STATS_GLYPH_COL,
            STATS_ROW0 + i as f64 * STATS_ROW_STEP,
            3.0,
            2.0,
        )
    })
}

/// The single canonical A-Type layout (identical for NTSC/PAL).
pub fn get_layout() -> &'static LayoutTable {
    use std::sync::OnceLock;
    static LAYOUT: OnceLock<LayoutTable> = OnceLock::new();
    LAYOUT.get_or_init(|| LayoutTable {
        score: tiles(24.0, 7.0, 6.0, 1.0),
        score_digits: 6,
        score_max_digits: 7,
        lines: tiles(19.0, 2.0, 3.0, 1.0),
        lines_digits: 3,
        level: tiles(26.0, 20.0, 2.0, 1.0),
        level_digits: 2,
        next_box: tiles(24.0, 14.0, 4.0, 2.0),
        playfield: tiles(12.0, 5.0, PLAYFIELD_COLS as f64, PLAYFIELD_ROWS as f64),
        spawn_zone: tiles(12.0, 5.0, PLAYFIELD_COLS as f64, 2.0),
        statistics: stats_rows(),
        statistics_glyphs: stats_glyph_rows(),
        stats_digits: STATS_DIGITS,
        label_lines: tiles(13.0, 2.0, 6.0, 1.0),
        label_score: tiles(24.0, 6.0, 5.0, 1.0),
        label_next: tiles(24.0, 12.0, 4.0, 1.0),
        label_level: tiles(25.0, 21.0, 5.0, 1.0),
        label_statistics: tiles(2.0, 5.0, 10.0, 1.0),
    })
}

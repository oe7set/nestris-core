//! Authentic NES Tetris level palette (port of `output/nes_palette.py`).

use crate::enums::Piece;

/// RGB color triple.
pub type Color = (u8, u8, u8);

/// White block color (achromatic bucket at every level).
pub const WHITE: Color = (252, 252, 252);

/// The two accent colors per level, indexed by `level % 10` (RGB).
pub const LEVEL_PALETTE: [(Color, Color); 10] = [
    ((0x00, 0x58, 0xF8), (0x3C, 0xBC, 0xFC)), // 0: blue / cyan
    ((0x00, 0xA8, 0x00), (0xB8, 0xF8, 0x18)), // 1: green / light green
    ((0xD8, 0x00, 0xCC), (0xF8, 0x78, 0xF8)), // 2: magenta / pink
    ((0x00, 0x58, 0xF8), (0x58, 0xD8, 0x54)), // 3: blue / green
    ((0xE4, 0x00, 0x58), (0x58, 0xF8, 0x98)), // 4: red-pink / mint
    ((0x58, 0xF8, 0x98), (0x68, 0x88, 0xFC)), // 5: mint / periwinkle
    ((0xF8, 0x38, 0x00), (0x7C, 0x7C, 0x7C)), // 6: red / gray
    ((0x68, 0x44, 0xFC), (0xA8, 0x00, 0x20)), // 7: purple / dark red
    ((0x00, 0x58, 0xF8), (0xF8, 0x38, 0x00)), // 8: blue / red
    ((0xF8, 0x38, 0x00), (0xFC, 0x98, 0x38)), // 9: red / orange
];

/// Standard tetromino colors (falling piece / NEXT preview, RGB).
pub fn piece_color(piece: Option<Piece>) -> Color {
    match piece {
        Some(Piece::I) => (0x00, 0xF0, 0xF0),
        Some(Piece::O) => (0xF0, 0xF0, 0x00),
        Some(Piece::T) => (0xA0, 0x00, 0xF0),
        Some(Piece::S) => (0x00, 0xF0, 0x00),
        Some(Piece::Z) => (0xF0, 0x00, 0x00),
        Some(Piece::J) => (0x00, 0x00, 0xF0),
        Some(Piece::L) => (0xF0, 0xA0, 0x00),
        Some(Piece::None) | None => (0x80, 0x80, 0x80),
    }
}

const FALLBACK_ACCENTS: (Color, Color) = ((0x3C, 0xBC, 0xFC), (0xF8, 0x38, 0x00));

/// RGB color of a locked cell with recognizer color id 1/2/3.
pub fn cell_color(level: Option<i64>, cell_id: u8) -> Color {
    if cell_id == 2 || cell_id == 3 {
        let accents = match level {
            None => FALLBACK_ACCENTS,
            Some(l) => LEVEL_PALETTE[(l.rem_euclid(10)) as usize],
        };
        return if cell_id == 2 { accents.0 } else { accents.1 };
    }
    WHITE
}

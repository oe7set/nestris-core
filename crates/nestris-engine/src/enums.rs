//! NES-Tetris domain enums with the exact wire values of the Python
//! implementation's `core/enums.py` (schema v4 contract).

use serde::{Deserialize, Serialize};

/// Console region (advisory palette/timing tag; layout is region-agnostic).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Region {
    #[serde(rename = "NTSC")]
    Ntsc,
    #[serde(rename = "PAL")]
    Pal,
}

/// High-level screen / game state; every frame maps to exactly one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameState {
    NoSignal,
    #[default]
    Unknown,
    Title,
    TypeSelect,
    LevelSelect,
    InGame,
    Paused,
    GameOver,
    HighscoreEntry,
}

/// The seven tetrominoes; `None` (wire `"-"`) is an empty NEXT box, distinct
/// from "could not read" which is an `Option::None` at the call site.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Piece {
    #[serde(rename = "-")]
    None,
    I,
    O,
    T,
    S,
    Z,
    J,
    L,
}

impl Piece {
    /// `true` for a real tetromino (not [`Piece::None`]).
    pub fn is_piece(self) -> bool {
        self != Piece::None
    }

    /// The canonical letter (wire value).
    pub fn letter(self) -> &'static str {
        match self {
            Piece::None => "-",
            Piece::I => "I",
            Piece::O => "O",
            Piece::T => "T",
            Piece::S => "S",
            Piece::Z => "Z",
            Piece::J => "J",
            Piece::L => "L",
        }
    }

    /// Parse the wire letter.
    pub fn from_letter(s: &str) -> Option<Piece> {
        Some(match s {
            "-" => Piece::None,
            "I" => Piece::I,
            "O" => Piece::O,
            "T" => Piece::T,
            "S" => Piece::S,
            "Z" => Piece::Z,
            "J" => Piece::J,
            "L" => Piece::L,
            _ => return None,
        })
    }
}

//! The versioned JSON output contract (schema v4), field-for-field identical
//! to the Python `output/schema.py` pydantic models: every field serializes
//! (Options as `null`), declaration order matches, enum wire values verbatim.

use serde::Serialize;

use crate::enums::{GameState, Piece, Region};
use crate::stats_ext::ExtendedStats;

pub const SCHEMA_VERSION: u32 = 4;

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct LineClears {
    pub single: u32,
    pub double: u32,
    pub triple: u32,
    pub tetris: u32,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct GameStats {
    pub pps: Option<f64>,
    pub tetris_rate: Option<f64>,
    pub burn: i64,
    pub drought: u32,
    pub max_drought: u32,
    pub clears: LineClears,
    pub score_per_min: Option<f64>,
    pub pieces: i64,
    pub active_seconds: Option<f64>,
}

/// Insertion-ordered STATISTICS map (mirrors the Python dict semantics: keys
/// appear in first-successful-read order and only once known).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatisticsMap(pub Vec<(Piece, Option<i64>)>);

impl StatisticsMap {
    pub fn get(&self, piece: Piece) -> Option<i64> {
        self.0
            .iter()
            .find(|(p, _)| *p == piece)
            .and_then(|(_, v)| *v)
    }
}

impl Serialize for StatisticsMap {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (piece, value) in &self.0 {
            map.serialize_entry(piece.letter(), value)?;
        }
        map.end()
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Fields {
    pub score: Option<i64>,
    pub lines: Option<i64>,
    pub level: Option<i64>,
    pub next_piece: Option<Piece>,
    pub current_piece: Option<Piece>,
    pub current_piece_pos: Option<(u32, u32)>,
    pub current_piece_cells: Option<Vec<(u32, u32)>>,
    pub playfield: Option<Vec<Vec<u8>>>,
    pub statistics: Option<StatisticsMap>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Confidence {
    pub score: f64,
    pub lines: f64,
    pub level: f64,
    pub next_piece: f64,
    pub playfield: f64,
    pub statistics: f64,
    pub current_piece: f64,
    pub geometry: f64,
    pub overall: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Event {
    pub ts: f64,
    pub field: String,
    pub reason: String,
    pub severity: String,
    pub old: Option<serde_json::Value>,
    pub new: Option<serde_json::Value>,
    pub confidence: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct OutputFrame {
    pub schema_version: u32,
    pub seq: i64,
    pub ts: f64,
    pub region: Region,
    pub game_state: GameState,
    pub fields: Fields,
    pub stats: GameStats,
    /// Extended dashboard statistics; attached only when
    /// `output.extended_stats` is enabled so the default serialization stays
    /// byte-identical to the verified schema-v4 wire format.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats_ext: Option<ExtendedStats>,
    pub confidence: Confidence,
    pub events: Vec<Event>,
}

impl OutputFrame {
    /// Compact UTF-8 JSON (transport-ready), like pydantic `model_dump_json`.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("OutputFrame serializes")
    }
}

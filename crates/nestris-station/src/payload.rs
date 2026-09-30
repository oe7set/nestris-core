//! MQTT payloads (the station → host contract, documented in
//! `docs/STATION.md`). All timestamps are RFC 3339 UTC with milliseconds.

use chrono::{DateTime, SecondsFormat, Utc};
use nestris_engine::integrity::validate::{ValidationIssue, ValidationMetrics};
use nestris_engine::output::LineClears;
use serde::Serialize;

use crate::rfid::Player;

/// Version of the `event/game_end` payload layout.
pub const GAME_END_SCHEMA: u32 = 1;

pub fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn now() -> String {
    stamp(Utc::now())
}

/// `<base>/status` (retained; the broker's last will sets `state: offline`).
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Status {
    pub state: &'static str,
    pub station: String,
    pub name: String,
    pub version: &'static str,
    /// `ok`, `opening`, `waiting_for_device`, `reconnecting`, `ended`.
    pub capture: &'static str,
    pub capture_detail: Option<String>,
    /// Geometry lock: `unlocked`, `acquiring`, `locked`, `drift`, `lost`.
    pub lock: String,
    pub game_state: String,
    /// `ok`, `offline`, `disabled`.
    pub rfid: &'static str,
    pub game_id: Option<String>,
    pub fps: f64,
    pub dropped_frames: u64,
    pub uptime_s: u64,
    pub ts: String,
}

/// The offline status used as MQTT last will and on clean shutdown.
#[derive(Serialize)]
pub struct Offline<'a> {
    pub state: &'static str,
    pub station: &'a str,
    pub ts: Option<String>,
}

/// `<base>/player` (retained).
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct PlayerPayload {
    pub present: bool,
    pub player: Option<Player>,
    pub rfid: &'static str,
    pub ts: String,
}

/// `<base>/live` (QoS 0, on change, rate-limited).
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Live {
    pub game_id: Option<String>,
    pub player: Option<Player>,
    pub game_state: String,
    pub score: Option<i64>,
    pub lines: Option<i64>,
    pub level: Option<i64>,
    pub next_piece: Option<String>,
    pub tetris_rate: Option<f64>,
    pub burn: i64,
    pub drought: u32,
    pub max_drought: u32,
    pub pps: Option<f64>,
    pub pieces: i64,
    pub cheated: u32,
    pub confidence: f64,
    pub ts: String,
}

/// `<base>/event/game_start`.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct GameStart {
    pub game_id: String,
    pub station: String,
    pub player: Option<Player>,
    pub started_at: String,
    pub start_level: Option<i64>,
}

/// `<base>/event/cheat`.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Cheat {
    pub game_id: String,
    pub station: String,
    pub player: Option<Player>,
    /// Cheat inputs so far this game.
    pub cheated: u32,
    /// Cheat inputs in this detection.
    pub count: u32,
    pub points: i64,
    pub score_before: i64,
    pub score_after: i64,
    pub lines_delta: i64,
    pub ts: String,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Validation {
    pub issues: Vec<ValidationIssue>,
    pub metrics: ValidationMetrics,
}

/// `<base>/event/game_end` (spooled until the broker acknowledged it).
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct GameEnd {
    pub schema: u32,
    pub game_id: String,
    pub station: String,
    pub player: Option<Player>,
    pub started_at: String,
    pub ended_at: String,
    pub duration_s: f64,
    pub active_seconds: Option<f64>,
    /// `game_over`, `reset`, `signal_lost`, `shutdown`.
    pub end_reason: &'static str,
    pub start_level: Option<i64>,
    pub end_level: Option<i64>,
    pub score: Option<i64>,
    pub lines: Option<i64>,
    pub clears: LineClears,
    pub tetris_rate: Option<f64>,
    pub burn: i64,
    pub max_drought: u32,
    pub pieces: i64,
    pub pps: Option<f64>,
    /// Cheat inputs detected this game (`0` = clean).
    pub cheated: u32,
    pub cheat_points: i64,
    /// `false` when the validation found an error: review before trusting.
    pub valid: bool,
    pub validation: Validation,
}

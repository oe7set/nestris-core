//! MQTT payloads (the station → host contract, documented in
//! `docs/STATION.md`). All timestamps are RFC 3339 UTC with milliseconds.

use chrono::{DateTime, SecondsFormat, Utc};
use nestris_engine::integrity::validate::{ValidationIssue, ValidationMetrics};
use nestris_engine::output::LineClears;
use serde::Serialize;

use crate::rfid::Player;
use crate::telemetry::Perf;

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
    /// `ok`, `offline`, `outdated` (reader firmware with another protocol), `disabled`.
    pub rfid: &'static str,
    /// Firmware version and serial number of the connected reader.
    pub reader_fw: Option<String>,
    pub reader_serial: Option<String>,
    pub game_id: Option<String>,
    /// Processed frames per second (also `perf.fps`).
    pub fps: f64,
    pub dropped_frames: u64,
    /// Pipeline performance of the last window (since 0.3.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub perf: Option<Perf>,
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
    /// The stack, top row first: 20 strings of 10 cell ids (`0` empty,
    /// `1` white, `2`/`3` the level's two accent colors). `null` outside
    /// active play (menus, pause: the console hides the board while paused)
    /// or with `mqtt.live_playfield = false`.
    pub playfield: Option<Vec<String>>,
    /// Running number of this message (since 0.3.0); a gap means `live`
    /// messages were lost on the way.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// Time from reading the frame off the capture to this message, in ms.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_age_ms: Option<f64>,
    pub ts: String,
}

/// `<base>/config` (retained): the remote configuration (`remote.rs`).
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ConfigReport {
    pub station: String,
    pub version: &'static str,
    /// Revision of the remote set in effect, pending or rejected.
    pub rev: Option<u64>,
    /// `none`, `applied`, `pending` (after the running game), `restarting`,
    /// `rejected`.
    pub state: &'static str,
    pub error: Option<String>,
    /// The remote set in effect.
    pub values: serde_json::Value,
    /// The configuration the station runs with (secrets masked).
    pub effective: serde_json::Value,
    /// Keys pinned by the environment or `--set` (remote values lose).
    pub locked: Vec<String>,
    /// Remote-settable keys (exact, or prefixes ending in `.`).
    pub allowed: &'static [&'static str],
    /// Answer to `list_devices`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub devices: Option<Devices>,
    pub ts: String,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Devices {
    pub capture: Vec<crate::remote::CaptureDevice>,
    /// Serial ports for `rfid.port`.
    pub serial: Vec<String>,
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

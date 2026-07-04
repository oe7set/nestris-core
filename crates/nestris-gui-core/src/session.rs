//! Persistent per-game results: finished games are appended and summarized
//! into TODAY / OVERALL personal-best tables for the stats window.
//!
//! Stored as JSON in the per-user config directory (next to the GUI
//! settings), independent from the NGF recordings. Shared by all desktop
//! frontends, so a game recorded in one GUI shows up in the others.

use serde::{Deserialize, Serialize};

/// One finished game.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GameRecord {
    /// Local date the game ended, `YYYY-MM-DD` (for the TODAY table).
    pub date: String,
    /// Local time the game ended, `HH:MM`.
    pub time: String,
    pub start_level: Option<i64>,
    pub end_level: Option<i64>,
    pub score: i64,
    pub lines: i64,
    pub tetris_rate: Option<f64>,
    pub duration_s: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionStore {
    pub games: Vec<GameRecord>,
}

/// Cap the persisted history (the PB tables only need the best rows, but a
/// bounded full history keeps the file small and future features possible).
const MAX_GAMES: usize = 2000;

impl SessionStore {
    pub fn load() -> SessionStore {
        std::fs::read_to_string(store_path())
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = store_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(raw) = serde_json::to_string(self) {
            let _ = std::fs::write(path, raw);
        }
    }

    pub fn push(&mut self, record: GameRecord) {
        self.games.push(record);
        if self.games.len() > MAX_GAMES {
            let excess = self.games.len() - MAX_GAMES;
            self.games.drain(..excess);
        }
        self.save();
    }

    /// Best games (by score, descending) for an optional date filter.
    pub fn best(&self, date: Option<&str>, n: usize) -> Vec<&GameRecord> {
        let mut rows: Vec<&GameRecord> = self
            .games
            .iter()
            .filter(|g| date.is_none_or(|d| g.date == d))
            .collect();
        rows.sort_by_key(|g| std::cmp::Reverse(g.score));
        rows.truncate(n);
        rows
    }
}

fn store_path() -> std::path::PathBuf {
    crate::config_dir().join("session_pbs.json")
}

/// Local `YYYY-MM-DD` / `HH:MM` for a record ending "now".
pub fn local_stamp() -> (String, String) {
    let now = chrono::Local::now();
    (
        now.format("%Y-%m-%d").to_string(),
        now.format("%H:%M").to_string(),
    )
}

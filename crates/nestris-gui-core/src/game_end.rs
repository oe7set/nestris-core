//! Game-end detection shared by the GUIs: watches the play → game-over
//! transition and turns the last in-game snapshot into a session record
//! for the PB tables.

use nestris_engine::enums::GameState;
use nestris_engine::output::OutputFrame;

use crate::session::{self, GameRecord};

/// Tracks the previous game state across frames; reset it (replace with a
/// fresh tracker) when a new source is opened.
#[derive(Default)]
pub struct GameEndTracker {
    /// Previous frame's game state (game-end detection for the PB tables).
    prev_game_state: Option<GameState>,
    /// Last in-game snapshot, recorded into the session on game over.
    last_ingame: Option<(OutputFrame, f64)>,
}

impl GameEndTracker {
    /// Feed one frame; returns a record when a game just ended.
    pub fn update(&mut self, output: &OutputFrame, position_s: f64) -> Option<GameRecord> {
        let state = output.game_state;
        if state == GameState::InGame {
            self.last_ingame = Some((output.clone(), position_s));
        }
        let was_playing = matches!(
            self.prev_game_state,
            Some(GameState::InGame | GameState::Paused)
        );
        self.prev_game_state = Some(state);
        if was_playing
            && state == GameState::GameOver
            && let Some((frame, _)) = self.last_ingame.take()
            && let Some(score) = frame.fields.score
        {
            let (date, time) = session::local_stamp();
            return Some(GameRecord {
                date,
                time,
                start_level: None,
                end_level: frame.fields.level,
                score,
                lines: frame.fields.lines.unwrap_or(0),
                tetris_rate: frame.stats.tetris_rate,
                duration_s: frame.stats.active_seconds.unwrap_or(0.0),
            });
        }
        None
    }
}

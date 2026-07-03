//! Sans-io game recorder: consumes engine output frames and produces one
//! encoded NGF (v3) recording per detected game.
//!
//! Lifecycle:
//! - A recording **starts** on the engine's `new_game` event (or, with
//!   `record_partial`, on the first `InGame` frame when no game is active —
//!   e.g. when capture begins mid-game).
//! - Frames are recorded while the game is in play, paused, or in the
//!   game-over confirmation window, so recordings keep their top-out tail.
//! - A recording **finishes** when `GameOver` (or a menu state) has been
//!   stable for `end_confirm_frames`, when the next `new_game` arrives, or
//!   on [`GameRecorder::finalize`]. Games shorter than `min_frames` are
//!   discarded as noise.
//!
//! The recorder is pure state + bytes: hosts decide where the bytes go
//! (file, browser download) and drive crash-safe streaming via
//! [`GameRecorder::drain_pending`].

use nestris_engine::enums::GameState;
use nestris_engine::output::OutputFrame;

use crate::codec::{self, NgfFrame};

#[derive(Clone, Debug)]
pub struct RecorderConfig {
    /// NGF game type (0 minimal, 1 classic, 2 DAS trainer).
    pub game_type: u8,
    pub player_num: u8,
    /// Consecutive game-over/menu frames before a recording is finalized.
    pub end_confirm_frames: u32,
    /// Also start recording on the first in-game frame when capture begins
    /// mid-game (no `new_game` boundary seen).
    pub record_partial: bool,
    /// Recordings with fewer frames than this are discarded.
    pub min_frames: usize,
}

impl Default for RecorderConfig {
    fn default() -> Self {
        Self {
            game_type: 1,
            player_num: 0,
            end_confirm_frames: 30,
            record_partial: false,
            min_frames: 60,
        }
    }
}

/// Recorder lifecycle notifications for the host.
#[derive(Clone, Debug, PartialEq)]
pub enum RecorderEvent {
    GameStarted {
        gameid: u16,
    },
    /// A game ended; `bytes` is the complete raw (un-gzipped) NGF stream.
    GameFinished {
        gameid: u16,
        frames: usize,
        bytes: Vec<u8>,
    },
}

struct ActiveGame {
    gameid: u16,
    start_ts: f64,
    buf: Vec<u8>,
    /// How much of `buf` the host has already drained (streaming path).
    drained: usize,
    frames: usize,
    end_streak: u32,
    last_field: [u8; 200],
}

/// See the module docs for the lifecycle.
pub struct GameRecorder {
    cfg: RecorderConfig,
    next_gameid: u16,
    active: Option<ActiveGame>,
}

impl GameRecorder {
    pub fn new(cfg: RecorderConfig) -> Self {
        Self {
            cfg,
            next_gameid: 1,
            active: None,
        }
    }

    pub fn recording(&self) -> bool {
        self.active.is_some()
    }

    pub fn active_gameid(&self) -> Option<u16> {
        self.active.as_ref().map(|a| a.gameid)
    }

    /// Feed one engine output frame; returns lifecycle events (a `new_game`
    /// mid-recording yields both a finish and a start).
    pub fn push(&mut self, out: &OutputFrame) -> Vec<RecorderEvent> {
        let mut events = Vec::new();
        let new_game = out
            .events
            .iter()
            .any(|e| e.field == "game" && e.reason == "new_game");

        if new_game {
            if let Some(ev) = self.finish() {
                events.push(ev);
            }
            events.push(self.start(out));
        } else if self.active.is_none()
            && self.cfg.record_partial
            && out.game_state == GameState::InGame
        {
            events.push(self.start(out));
        }

        let Some(active) = &mut self.active else {
            return events;
        };

        match out.game_state {
            GameState::InGame | GameState::Paused => {
                active.end_streak = 0;
                Self::record_frame(&self.cfg, active, out);
            }
            GameState::GameOver => {
                // Keep the top-out visible in the replay, then confirm.
                active.end_streak += 1;
                if active.end_streak <= self.cfg.end_confirm_frames {
                    Self::record_frame(&self.cfg, active, out);
                }
                if active.end_streak >= self.cfg.end_confirm_frames
                    && let Some(ev) = self.finish()
                {
                    events.push(ev);
                }
            }
            _ => {
                // Menus/title/no-signal: don't record; a persistent leave
                // (console reset, source switch) also ends the game.
                active.end_streak += 1;
                if active.end_streak >= self.cfg.end_confirm_frames
                    && let Some(ev) = self.finish()
                {
                    events.push(ev);
                }
            }
        }
        events
    }

    /// Bytes encoded since the last drain (crash-safe streaming to a
    /// `.ngf.part` file). Returns an empty slice when idle.
    pub fn drain_pending(&mut self) -> &[u8] {
        match &mut self.active {
            Some(active) => {
                let new = &active.buf[active.drained..];
                active.drained = active.buf.len();
                new
            }
            None => &[],
        }
    }

    /// Finish the active recording (stream end / shutdown).
    pub fn finalize(&mut self) -> Option<RecorderEvent> {
        self.finish()
    }

    /// Discard the active recording without emitting it (seek/source switch).
    pub fn abort(&mut self) {
        self.active = None;
    }

    fn start(&mut self, out: &OutputFrame) -> RecorderEvent {
        let gameid = self.next_gameid;
        self.next_gameid = self.next_gameid.wrapping_add(1).max(1);
        self.active = Some(ActiveGame {
            gameid,
            start_ts: out.ts,
            buf: Vec::with_capacity(codec::V3_FRAME_SIZE * 4096),
            drained: 0,
            frames: 0,
            end_streak: 0,
            last_field: [0u8; 200],
        });
        RecorderEvent::GameStarted { gameid }
    }

    fn finish(&mut self) -> Option<RecorderEvent> {
        let active = self.active.take()?;
        if active.frames < self.cfg.min_frames {
            return None;
        }
        Some(RecorderEvent::GameFinished {
            gameid: active.gameid,
            frames: active.frames,
            bytes: active.buf,
        })
    }

    fn record_frame(cfg: &RecorderConfig, active: &mut ActiveGame, out: &OutputFrame) {
        // NGF has no "field unknown" sentinel: hold the last known grid
        // (matches how the fused output holds during clear animations).
        if let Some(grid) = &out.fields.playfield {
            let mut flat = [0u8; 200];
            for (r, row) in grid.iter().take(20).enumerate() {
                for (c, &cell) in row.iter().take(10).enumerate() {
                    flat[r * 10 + c] = cell.min(3);
                }
            }
            active.last_field = flat;
        }

        let counts: [Option<u16>; 7] = match &out.fields.statistics {
            Some(stats) => std::array::from_fn(|i| {
                stats
                    .get(codec::NGF_PIECES[i])
                    .map(|v| v.clamp(0, codec::MAX_COUNT as i64) as u16)
            }),
            None => [None; 7],
        };

        let frame = NgfFrame {
            version: 3,
            game_type: cfg.game_type,
            player_num: cfg.player_num,
            gameid: active.gameid,
            ctime_ms: ((out.ts - active.start_ts).max(0.0) * 1000.0).round() as u32,
            lines: out
                .fields
                .lines
                .map(|v| v.clamp(0, codec::MAX_LINES as i64) as u16),
            level: out.fields.level.map(|v| v.clamp(0, 254) as u8),
            score: out
                .fields
                .score
                .map(|v| v.clamp(0, codec::MAX_SCORE as i64) as u32),
            instant_das: None, // not observable via OCR
            preview: out.fields.next_piece.filter(|p| p.is_piece()),
            cur_piece_das: None,
            cur_piece: out.fields.current_piece.filter(|p| p.is_piece()),
            counts,
            field: active.last_field,
        };
        codec::encode_v3(&frame, &mut active.buf);
        active.frames += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nestris_engine::output::{Event, Fields, OutputFrame};

    fn frame(seq: i64, state: GameState, new_game: bool) -> OutputFrame {
        let mut events = Vec::new();
        if new_game {
            events.push(Event {
                ts: seq as f64 / 60.0,
                field: "game".into(),
                reason: "new_game".into(),
                severity: "info".into(),
                old: None,
                new: None,
                confidence: None,
            });
        }
        OutputFrame {
            schema_version: 4,
            seq,
            ts: seq as f64 / 60.0,
            region: nestris_engine::enums::Region::Ntsc,
            game_state: state,
            fields: Fields {
                score: Some(1234),
                lines: Some(10),
                level: Some(5),
                playfield: Some(vec![vec![0u8; 10]; 20]),
                ..Default::default()
            },
            stats: Default::default(),
            stats_ext: None,
            confidence: Default::default(),
            events,
        }
    }

    fn drive(rec: &mut GameRecorder, frames: impl IntoIterator<Item = OutputFrame>) -> Vec<RecorderEvent> {
        frames.into_iter().flat_map(|f| rec.push(&f)).collect()
    }

    #[test]
    fn records_one_game_per_boundary() {
        let mut rec = GameRecorder::new(RecorderConfig {
            min_frames: 10,
            end_confirm_frames: 5,
            ..Default::default()
        });
        let mut seq = 0i64;
        let mut mk = |state, new_game| {
            seq += 1;
            frame(seq, state, new_game)
        };

        let mut events = Vec::new();
        events.extend(rec.push(&mk(GameState::InGame, true)));
        for _ in 0..20 {
            events.extend(rec.push(&mk(GameState::InGame, false)));
        }
        for _ in 0..6 {
            events.extend(rec.push(&mk(GameState::GameOver, false)));
        }

        assert!(matches!(events[0], RecorderEvent::GameStarted { gameid: 1 }));
        let finished = events
            .iter()
            .find_map(|e| match e {
                RecorderEvent::GameFinished { gameid, frames, bytes } => {
                    Some((*gameid, *frames, bytes.len()))
                }
                _ => None,
            })
            .expect("game finished");
        assert_eq!(finished.0, 1);
        // 21 in-game frames + 5 recorded game-over frames.
        assert_eq!(finished.1, 26);
        assert_eq!(finished.2, 26 * codec::V3_FRAME_SIZE);
        assert!(!rec.recording());
    }

    #[test]
    fn new_game_mid_recording_finishes_and_restarts() {
        let mut rec = GameRecorder::new(RecorderConfig {
            min_frames: 5,
            ..Default::default()
        });
        let mut frames: Vec<OutputFrame> = Vec::new();
        frames.push(frame(0, GameState::InGame, true));
        for i in 1..10 {
            frames.push(frame(i, GameState::InGame, false));
        }
        frames.push(frame(10, GameState::InGame, true)); // reset mid-game
        let events = drive(&mut rec, frames);
        let kinds: Vec<_> = events
            .iter()
            .map(|e| match e {
                RecorderEvent::GameStarted { gameid } => format!("start{gameid}"),
                RecorderEvent::GameFinished { gameid, .. } => format!("finish{gameid}"),
            })
            .collect();
        assert_eq!(kinds, ["start1", "finish1", "start2"]);
        assert!(rec.recording());
    }

    #[test]
    fn short_games_are_discarded() {
        let mut rec = GameRecorder::new(RecorderConfig {
            min_frames: 60,
            ..Default::default()
        });
        rec.push(&frame(0, GameState::InGame, true));
        for i in 1..10 {
            rec.push(&frame(i, GameState::InGame, false));
        }
        assert!(rec.finalize().is_none());
    }

    #[test]
    fn partial_recording_starts_without_boundary() {
        let mut rec = GameRecorder::new(RecorderConfig {
            record_partial: true,
            min_frames: 1,
            ..Default::default()
        });
        let events = rec.push(&frame(0, GameState::InGame, false));
        assert!(matches!(events[0], RecorderEvent::GameStarted { .. }));
        // Without record_partial nothing starts.
        let mut rec2 = GameRecorder::new(RecorderConfig::default());
        assert!(rec2.push(&frame(0, GameState::InGame, false)).is_empty());
    }

    #[test]
    fn drain_pending_streams_incrementally() {
        let mut rec = GameRecorder::new(RecorderConfig {
            min_frames: 1,
            ..Default::default()
        });
        rec.push(&frame(0, GameState::InGame, true));
        assert_eq!(rec.drain_pending().len(), codec::V3_FRAME_SIZE);
        assert!(rec.drain_pending().is_empty());
        rec.push(&frame(1, GameState::InGame, false));
        rec.push(&frame(2, GameState::InGame, false));
        assert_eq!(rec.drain_pending().len(), 2 * codec::V3_FRAME_SIZE);
    }

    #[test]
    fn menu_exit_ends_recording() {
        let mut rec = GameRecorder::new(RecorderConfig {
            min_frames: 5,
            end_confirm_frames: 3,
            ..Default::default()
        });
        rec.push(&frame(0, GameState::InGame, true));
        for i in 1..10 {
            rec.push(&frame(i, GameState::InGame, false));
        }
        let mut finished = false;
        for i in 10..14 {
            for e in rec.push(&frame(i, GameState::Title, false)) {
                finished |= matches!(e, RecorderEvent::GameFinished { .. });
            }
        }
        assert!(finished);
    }

    #[test]
    fn recorded_frames_decode_with_held_field() {
        let mut rec = GameRecorder::new(RecorderConfig {
            min_frames: 1,
            ..Default::default()
        });
        rec.push(&frame(0, GameState::InGame, true));
        // Frame with a filled bottom-left cell, then one with no playfield.
        let mut f1 = frame(1, GameState::InGame, false);
        let mut grid = vec![vec![0u8; 10]; 20];
        grid[19][0] = 2;
        f1.fields.playfield = Some(grid);
        rec.push(&f1);
        let mut f2 = frame(2, GameState::InGame, false);
        f2.fields.playfield = None;
        rec.push(&f2);

        let ev = rec.finalize().unwrap();
        let RecorderEvent::GameFinished { bytes, .. } = ev else {
            panic!()
        };
        let frames = crate::io::decode_all(&bytes).unwrap();
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[1].field[190], 2);
        // Field held across the unreadable frame.
        assert_eq!(frames[2].field[190], 2);
        assert_eq!(frames[1].score, Some(1234));
        assert_eq!(frames[1].ctime_ms, 17); // 1/60 s ≈ 16.7 ms rounded
    }
}

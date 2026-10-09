//! Game sessions: turns the per-frame output stream into games with a
//! start, a player, cheat events and a validated end result.
//!
//! - **Start**: the engine's `new_game` event, or the first in-game frame
//!   when capture joins a running game (the validator flags it partial).
//!   A game is only announced after `min_game_frames` in-game frames with a
//!   readable score, so false starts never reach the host.
//! - **Player**: the card on the reader when the game is announced, else a
//!   card removed within the grace period; a card placed mid-game fills a
//!   still-empty slot.
//! - **End**: `end_confirm_frames` of game-over/menu screens (`game_over`
//!   when a game-over screen was seen, else `reset`), a new game
//!   (`reset`), no usable picture for `signal_lost_end_s` (`signal_lost`),
//!   or station shutdown (`shutdown`).

use std::time::Duration;

use chrono::{DateTime, Utc};
use nestris_engine::enums::GameState;
use nestris_engine::integrity::IntegrityConfig;
use nestris_engine::integrity::cheat::{CheatDetector, IntegrityEvent};
use nestris_engine::integrity::validate::{GameValidator, Severity};
use nestris_engine::output::{Fields, GameStats, OutputFrame};
use nestris_engine::state::plausibility::infer_start_level;
use tracing::{info, warn};

/// Discarded false starts up to this length are not logged.
const SHORT_FALSE_START_FRAMES: u64 = 10;

use crate::config::SessionSection;
use crate::payload::{self, Cheat, GameEnd, GameStart, Live, Validation};
use crate::rfid::{Player, RfidSnapshot};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndReason {
    GameOver,
    Reset,
    SignalLost,
    Shutdown,
}

impl EndReason {
    pub fn as_str(self) -> &'static str {
        match self {
            EndReason::GameOver => "game_over",
            EndReason::Reset => "reset",
            EndReason::SignalLost => "signal_lost",
            EndReason::Shutdown => "shutdown",
        }
    }
}

#[derive(Debug)]
pub enum SessionEvent {
    Start(GameStart),
    Cheat(Cheat),
    End(Box<GameEnd>),
}

struct Active {
    game_id: String,
    started_at: DateTime<Utc>,
    player: Option<Player>,
    detector: CheatDetector,
    validator: GameValidator,
    announced: bool,
    ingame_frames: u64,
    start_level: Option<i64>,
    last_fields: Fields,
    last_stats: GameStats,
    end_streak: u32,
    saw_game_over: bool,
    lost_since: Option<f64>,
    /// Cheat detections made before the game was announced.
    early_cheats: Vec<IntegrityEvent>,
}

pub struct SessionTracker {
    cfg: SessionSection,
    integrity: IntegrityConfig,
    station: String,
    grace: Duration,
    active: Option<Active>,
}

impl SessionTracker {
    pub fn new(
        cfg: SessionSection,
        integrity: IntegrityConfig,
        station: String,
        grace: Duration,
    ) -> Self {
        Self {
            cfg,
            integrity,
            station,
            grace,
            active: None,
        }
    }

    /// The announced game's id, if a game is running.
    pub fn game_id(&self) -> Option<&str> {
        self.active
            .as_ref()
            .filter(|a| a.announced)
            .map(|a| a.game_id.as_str())
    }

    /// Feed one output frame.
    pub fn push(&mut self, out: &OutputFrame, rfid: &RfidSnapshot) -> Vec<SessionEvent> {
        let mut events = Vec::new();
        let new_game = out
            .events
            .iter()
            .any(|e| e.field == "game" && e.reason == "new_game");
        if new_game {
            events.extend(self.end(EndReason::Reset));
            self.start();
        } else if self.active.is_none() && out.game_state == GameState::InGame {
            self.start();
        }
        let Some(a) = self.active.as_mut() else {
            return events;
        };

        a.validator.push(out);
        let detected = a.detector.push(out);
        if a.announced {
            events.extend(
                detected
                    .into_iter()
                    .filter_map(|e| cheat_event(a, &self.station, e)),
            );
        } else {
            a.early_cheats.extend(detected);
        }

        let mut end_reason = None;
        match out.game_state {
            GameState::InGame => {
                a.ingame_frames += 1;
                a.end_streak = 0;
                a.saw_game_over = false;
                a.lost_since = None;
                // Keep the last *known* value per field: the final in-game
                // frames (curtain, transition) often read nothing.
                let f = &out.fields;
                a.last_fields.score = f.score.or(a.last_fields.score);
                a.last_fields.lines = f.lines.or(a.last_fields.lines);
                a.last_fields.level = f.level.or(a.last_fields.level);
                a.last_stats = out.stats.clone();
                if a.start_level.is_none()
                    && let Some(level) = out.fields.level
                {
                    a.start_level = Some(infer_start_level(level, out.fields.lines.unwrap_or(0)));
                }
                // A real game shows a readable SCORE; picture noise that only
                // classifies as in-game never does.
                if !a.announced
                    && a.ingame_frames >= self.cfg.min_game_frames
                    && a.last_fields.score.is_some()
                {
                    a.announced = true;
                    a.player = rfid.player_for_game(self.grace);
                    info!(game_id = %a.game_id, player = ?a.player, "game started");
                    events.push(SessionEvent::Start(GameStart {
                        game_id: a.game_id.clone(),
                        station: self.station.clone(),
                        player: a.player.clone(),
                        started_at: payload::stamp(a.started_at),
                        start_level: a.start_level,
                    }));
                    let early = std::mem::take(&mut a.early_cheats);
                    events.extend(
                        early
                            .into_iter()
                            .filter_map(|e| cheat_event(a, &self.station, e)),
                    );
                } else if a.announced && a.player.is_none() && rfid.present.is_some() {
                    a.player = rfid.present.clone();
                    info!(game_id = %a.game_id, player = ?a.player, "player assigned mid-game");
                }
            }
            GameState::Paused => {}
            GameState::NoSignal | GameState::Unknown => {
                let since = *a.lost_since.get_or_insert(out.ts);
                if out.ts - since >= self.cfg.signal_lost_end_s {
                    end_reason = Some(EndReason::SignalLost);
                }
            }
            state => {
                a.lost_since = None;
                a.end_streak += 1;
                a.saw_game_over |= matches!(state, GameState::GameOver | GameState::HighscoreEntry);
                if a.end_streak >= self.cfg.end_confirm_frames {
                    end_reason = Some(if a.saw_game_over {
                        EndReason::GameOver
                    } else {
                        EndReason::Reset
                    });
                }
            }
        }
        if let Some(reason) = end_reason {
            events.extend(self.end(reason));
        }
        events
    }

    /// Capture delivered no frames at all for `seconds` (device outage).
    pub fn capture_gap(&mut self, seconds: f64) {
        if let Some(a) = &mut self.active {
            a.validator.add_signal_gap(seconds);
        }
    }

    /// No frames for `seconds` and counting: closes the game past the limit.
    pub fn capture_down_for(&mut self, seconds: f64) -> Vec<SessionEvent> {
        if self.active.is_some() && seconds >= self.cfg.signal_lost_end_s {
            if let Some(a) = &mut self.active {
                a.validator.add_signal_gap(seconds);
            }
            return self.end(EndReason::SignalLost);
        }
        Vec::new()
    }

    /// The engine was rebuilt after a panic mid-game.
    pub fn engine_restart(&mut self) {
        if let Some(a) = &mut self.active {
            a.validator.add_issue(
                "engine_restart",
                Severity::Error,
                "the recognition engine was restarted during the game",
            );
        }
    }

    /// Close any running game (station shutdown).
    pub fn shutdown(&mut self) -> Vec<SessionEvent> {
        self.end(EndReason::Shutdown)
    }

    /// The live view of the current frame (`with_playfield`: include the
    /// stack while in play).
    pub fn live(&self, out: &OutputFrame, rfid: &RfidSnapshot, with_playfield: bool) -> Live {
        let game = self.active.as_ref().filter(|a| a.announced);
        let s = &out.stats;
        Live {
            game_id: game.map(|a| a.game_id.clone()),
            player: game
                .and_then(|a| a.player.clone())
                .or_else(|| rfid.present.clone()),
            game_state: game_state_str(out.game_state),
            score: out.fields.score,
            lines: out.fields.lines,
            level: out.fields.level,
            next_piece: out.fields.next_piece.map(|p| p.letter().to_string()),
            tetris_rate: s.tetris_rate.map(round4),
            burn: s.burn,
            drought: s.drought,
            max_drought: s.max_drought,
            pps: s.pps.map(round4),
            pieces: s.pieces,
            cheated: game.map_or(0, |a| a.detector.cheated()),
            confidence: round4(out.confidence.overall),
            playfield: if with_playfield && out.game_state == GameState::InGame {
                out.fields.playfield.as_deref().map(playfield_rows)
            } else {
                None
            },
            seq: None,
            frame_age_ms: None,
            ts: payload::now(),
        }
    }

    fn start(&mut self) {
        let started_at = Utc::now();
        self.active = Some(Active {
            game_id: format!("{}-{}", self.station, started_at.timestamp_millis()),
            started_at,
            player: None,
            detector: CheatDetector::new(self.integrity.clone()),
            validator: GameValidator::new(self.integrity.clone()),
            announced: false,
            ingame_frames: 0,
            start_level: None,
            last_fields: Fields::default(),
            last_stats: GameStats::default(),
            end_streak: 0,
            saw_game_over: false,
            lost_since: None,
            early_cheats: Vec::new(),
        });
    }

    fn end(&mut self, reason: EndReason) -> Vec<SessionEvent> {
        let Some(mut a) = self.active.take() else {
            return Vec::new();
        };
        if !a.announced {
            // A few frames are routine: the engine confirms a new game a
            // handful of frames after the first in-game frame.
            if a.ingame_frames > SHORT_FALSE_START_FRAMES {
                info!(
                    frames = a.ingame_frames,
                    "discarding a too-short game (false start)"
                );
            }
            return Vec::new();
        }
        let mut events: Vec<SessionEvent> = a
            .detector
            .finish()
            .into_iter()
            .filter_map(|e| cheat_event(&a, &self.station, e))
            .collect();
        let report = a.validator.report(&a.detector, &a.last_stats);
        let ended_at = Utc::now();
        let s = &a.last_stats;
        let end = GameEnd {
            schema: payload::GAME_END_SCHEMA,
            game_id: a.game_id.clone(),
            station: self.station.clone(),
            player: a.player.clone(),
            started_at: payload::stamp(a.started_at),
            ended_at: payload::stamp(ended_at),
            duration_s: round4((ended_at - a.started_at).num_milliseconds() as f64 / 1000.0),
            active_seconds: s.active_seconds.map(round4),
            end_reason: reason.as_str(),
            start_level: a.start_level,
            end_level: a.last_fields.level,
            score: a.last_fields.score,
            lines: a.last_fields.lines,
            clears: s.clears.clone(),
            tetris_rate: s.tetris_rate.map(round4),
            burn: s.burn,
            max_drought: s.max_drought,
            pieces: s.pieces,
            pps: s.pps.map(round4),
            cheated: a.detector.cheated(),
            cheat_points: a.detector.cheat_points(),
            valid: report.valid,
            validation: Validation {
                issues: report.issues,
                metrics: report.metrics,
            },
        };
        if end.valid {
            info!(game_id = %end.game_id, score = ?end.score, lines = ?end.lines,
                  cheated = end.cheated, reason = end.end_reason, "game ended");
        } else {
            warn!(game_id = %end.game_id, score = ?end.score, cheated = end.cheated,
                  reason = end.end_reason, issues = ?end.validation.issues,
                  "game ended with validation errors");
        }
        a.announced = false;
        events.push(SessionEvent::End(Box::new(end)));
        events
    }
}

fn cheat_event(a: &Active, station: &str, event: IntegrityEvent) -> Option<SessionEvent> {
    match event {
        IntegrityEvent::Cheat {
            count,
            total,
            points,
            score_before,
            score_after,
            lines_delta,
            ..
        } => {
            warn!(game_id = %a.game_id, count, total, score_before, score_after, "cheat detected");
            Some(SessionEvent::Cheat(Cheat {
                game_id: a.game_id.clone(),
                station: station.to_string(),
                player: a.player.clone(),
                cheated: total,
                count,
                points,
                score_before,
                score_after,
                lines_delta,
                ts: payload::now(),
            }))
        }
        IntegrityEvent::ScoreAnomaly {
            score_before,
            score_after,
            lines_delta,
            unexplained,
            ..
        } => {
            warn!(game_id = %a.game_id, score_before, score_after, lines_delta, unexplained,
                  "score gain matches no scoring event");
            None
        }
    }
}

/// Engine playfield (20 rows of 10 cell ids) as 20 digit strings.
pub fn playfield_rows(grid: &[Vec<u8>]) -> Vec<String> {
    grid.iter()
        .map(|row| row.iter().map(|&c| char::from(b'0' + c.min(9))).collect())
        .collect()
}

pub fn game_state_str(state: GameState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use nestris_engine::enums::Region;
    use nestris_engine::output::{Confidence, Event};

    fn frame(seq: i64, state: GameState, score: i64, lines: i64, new_game: bool) -> OutputFrame {
        OutputFrame {
            schema_version: 4,
            seq,
            ts: seq as f64 / 60.0,
            region: Region::Ntsc,
            game_state: state,
            fields: Fields {
                score: Some(score),
                lines: Some(lines),
                level: Some(18),
                ..Default::default()
            },
            stats: GameStats::default(),
            stats_ext: None,
            confidence: Confidence {
                overall: 0.95,
                ..Default::default()
            },
            events: if new_game {
                vec![Event {
                    ts: seq as f64 / 60.0,
                    field: "game".into(),
                    reason: "new_game".into(),
                    severity: "info".into(),
                    old: None,
                    new: None,
                    confidence: None,
                }]
            } else {
                Vec::new()
            },
        }
    }

    fn tracker() -> SessionTracker {
        SessionTracker::new(
            SessionSection::default(),
            IntegrityConfig::default(),
            "st1".into(),
            Duration::from_secs(60),
        )
    }

    fn card() -> RfidSnapshot {
        RfidSnapshot {
            connected: true,
            present: Some(Player {
                uid: "ABCD".into(),
                name: Some("Erv".into()),
            }),
            last_seen: None,
            ..Default::default()
        }
    }

    struct Run {
        t: SessionTracker,
        seq: i64,
        events: Vec<SessionEvent>,
    }

    impl Run {
        fn feed(&mut self, state: GameState, score: i64, lines: i64, n: i64, rfid: &RfidSnapshot) {
            for i in 0..n {
                let new_game = state == GameState::InGame && i == 0 && self.seq == 0;
                let f = frame(self.seq, state, score, lines, new_game);
                self.events.extend(self.t.push(&f, rfid));
                self.seq += 1;
            }
        }
    }

    #[test]
    fn full_game_with_cheat() {
        let mut r = Run {
            t: tracker(),
            seq: 0,
            events: Vec::new(),
        };
        let rfid = card();
        r.feed(GameState::InGame, 0, 0, 200, &rfid);
        r.feed(GameState::InGame, 10_000, 0, 200, &rfid);
        r.feed(GameState::GameOver, 10_000, 0, 40, &rfid);

        let kinds: Vec<&str> = r
            .events
            .iter()
            .map(|e| match e {
                SessionEvent::Start(_) => "start",
                SessionEvent::Cheat(_) => "cheat",
                SessionEvent::End(_) => "end",
            })
            .collect();
        assert_eq!(kinds, vec!["start", "cheat", "end"]);
        let SessionEvent::End(end) = r.events.last().unwrap() else {
            unreachable!()
        };
        assert_eq!(end.end_reason, "game_over");
        assert_eq!(end.cheated, 1);
        assert_eq!(end.score, Some(10_000));
        assert_eq!(end.player.as_ref().unwrap().uid, "ABCD");
        assert!(end.game_id.starts_with("st1-"));
        assert!(r.t.game_id().is_none());
    }

    #[test]
    fn game_without_a_score_reading_is_never_announced() {
        let mut t = tracker();
        let rfid = RfidSnapshot::default();
        let mut events = Vec::new();
        for seq in 0..400 {
            let mut f = frame(seq, GameState::InGame, 0, 0, seq == 0);
            f.fields.score = None;
            events.extend(t.push(&f, &rfid));
        }
        events.extend(t.shutdown());
        assert!(events.is_empty());
    }

    #[test]
    fn end_keeps_the_last_known_values() {
        let mut r = Run {
            t: tracker(),
            seq: 0,
            events: Vec::new(),
        };
        let rfid = RfidSnapshot::default();
        r.feed(GameState::InGame, 1_234, 7, 200, &rfid);
        // Final in-game frames without readings (game-over curtain).
        for _ in 0..10 {
            let mut f = frame(r.seq, GameState::InGame, 0, 0, false);
            f.fields = Fields::default();
            r.events.extend(r.t.push(&f, &rfid));
            r.seq += 1;
        }
        r.feed(GameState::GameOver, 1_234, 7, 40, &rfid);
        let SessionEvent::End(end) = r.events.last().unwrap() else {
            panic!("expected an end event")
        };
        assert_eq!(
            (end.score, end.lines, end.end_level),
            (Some(1_234), Some(7), Some(18))
        );
    }

    #[test]
    fn false_start_is_discarded() {
        let mut r = Run {
            t: tracker(),
            seq: 0,
            events: Vec::new(),
        };
        r.feed(GameState::InGame, 0, 0, 30, &RfidSnapshot::default());
        r.feed(GameState::Title, 0, 0, 60, &RfidSnapshot::default());
        assert!(r.events.is_empty());
    }

    #[test]
    fn lost_signal_closes_the_game() {
        let mut r = Run {
            t: tracker(),
            seq: 0,
            events: Vec::new(),
        };
        r.feed(GameState::InGame, 0, 0, 200, &RfidSnapshot::default());
        r.feed(GameState::NoSignal, 0, 0, 31 * 60, &RfidSnapshot::default());
        let SessionEvent::End(end) = r.events.last().unwrap() else {
            panic!("expected an end event")
        };
        assert_eq!(end.end_reason, "signal_lost");
        assert!(!end.valid);
        assert!(end.player.is_none());
    }

    /// A long pause (the console blanks the screen) never ends the game.
    #[test]
    fn long_pause_keeps_the_game() {
        let mut r = Run {
            t: tracker(),
            seq: 0,
            events: Vec::new(),
        };
        let rfid = RfidSnapshot::default();
        r.feed(GameState::InGame, 0, 0, 200, &rfid);
        r.feed(GameState::Paused, 0, 0, 5 * 60 * 60, &rfid);
        r.feed(GameState::InGame, 800, 2, 200, &rfid);
        assert!(matches!(&r.events[..], [SessionEvent::Start(_)]));
        assert!(r.t.game_id().is_some());
        r.feed(GameState::GameOver, 800, 2, 40, &rfid);
        let SessionEvent::End(end) = r.events.last().unwrap() else {
            panic!("expected an end event")
        };
        assert_eq!(end.end_reason, "game_over");
    }

    /// Menus after the curtain close the game once, as `game_over`.
    #[test]
    fn menus_after_game_over_end_once() {
        let mut r = Run {
            t: tracker(),
            seq: 0,
            events: Vec::new(),
        };
        let rfid = RfidSnapshot::default();
        r.feed(GameState::InGame, 100, 1, 200, &rfid);
        r.feed(GameState::GameOver, 100, 1, 10, &rfid);
        r.feed(GameState::LevelSelect, 100, 1, 100, &rfid);
        r.feed(GameState::TypeSelect, 100, 1, 100, &rfid);
        let ends: Vec<&GameEnd> = r
            .events
            .iter()
            .filter_map(|e| match e {
                SessionEvent::End(end) => Some(end.as_ref()),
                _ => None,
            })
            .collect();
        assert_eq!(ends.len(), 1);
        assert_eq!(ends[0].end_reason, "game_over");
    }

    #[test]
    fn live_carries_the_playfield_only_in_play() {
        let t = tracker();
        let rfid = RfidSnapshot::default();
        let mut f = frame(0, GameState::InGame, 0, 0, false);
        let mut grid = vec![vec![0u8; 10]; 20];
        grid[19] = vec![1, 2, 3, 0, 0, 0, 0, 0, 0, 1];
        f.fields.playfield = Some(grid);
        let live = t.live(&f, &rfid, true);
        let rows = live.playfield.expect("playfield in play");
        assert_eq!(rows.len(), 20);
        assert_eq!(rows[0], "0000000000");
        assert_eq!(rows[19], "1230000001");
        assert!(t.live(&f, &rfid, false).playfield.is_none());
        f.game_state = GameState::Paused;
        assert!(t.live(&f, &rfid, true).playfield.is_none());
    }

    /// Real recorded game (`NESTRIS_SAMPLE_NGF=<file.ngf[.gz]>`, skipped when
    /// unset): clean as recorded, exactly one cheat after injecting +10 000
    /// into the score from mid-game on.
    #[test]
    fn recorded_game_with_injected_cheat() {
        let Ok(path) = std::env::var("NESTRIS_SAMPLE_NGF") else {
            eprintln!("NESTRIS_SAMPLE_NGF unset, skipping");
            return;
        };
        let file = nestris_ngf::replay::ReplayFile::load(std::path::Path::new(&path)).unwrap();
        let mut engine = nestris_ngf::replay::ReplayEngine::new(file);
        let frames: Vec<OutputFrame> = (0..engine.frame_count())
            .map(|i| engine.output_at(i))
            .collect();

        let run = |inject_from: Option<usize>| -> GameEnd {
            let mut t = tracker();
            let mut end = None;
            let mut feed = |t: &mut SessionTracker, out: &OutputFrame| {
                for e in t.push(out, &RfidSnapshot::default()) {
                    if let SessionEvent::End(e) = e {
                        end = Some(*e);
                    }
                }
            };
            for (i, f) in frames.iter().enumerate() {
                let mut f = f.clone();
                if inject_from.is_some_and(|from| i >= from)
                    && let Some(score) = &mut f.fields.score
                {
                    *score += 10_000;
                }
                feed(&mut t, &f);
            }
            let mut tail = frames.last().unwrap().clone();
            tail.game_state = GameState::GameOver;
            tail.events.clear();
            for _ in 0..40 {
                feed(&mut t, &tail);
            }
            end.expect("game ended")
        };

        let clean = run(None);
        assert_eq!(clean.cheated, 0, "{:?}", clean.validation);
        let cheated = run(Some(frames.len() / 2));
        assert_eq!(cheated.cheated, 1, "{:?}", cheated.validation);
        assert_eq!(cheated.cheat_points, 10_000);
    }

    #[test]
    fn shutdown_closes_a_running_game() {
        let mut r = Run {
            t: tracker(),
            seq: 0,
            events: Vec::new(),
        };
        r.feed(GameState::InGame, 0, 0, 200, &card());
        let events = r.t.shutdown();
        assert!(matches!(&events[..], [SessionEvent::End(e)] if e.end_reason == "shutdown"));
    }
}

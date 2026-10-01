//! Temporal fusion (exact port of `state/fusion.py`).
//!
//! Every constant, ordering rule, and float operation mirrors the Python
//! implementation: `Counter.most_common(1)` ties resolve to the
//! first-inserted key, window voting is confidence-weighted, confidences are
//! f64 throughout. The Phase-4 gate diffs this layer's output byte-for-byte
//! (modulo ts) against the Python oracle on replayed readings.

use std::collections::VecDeque;

use crate::config::FusionConfig;
use crate::enums::{GameState, Piece};
use crate::output::StatisticsMap;

const NEXT_HOLD_FRAMES: u32 = 120;
const MONOTONIC_OVERRIDE_FRAMES: u32 = 6;
const RESTART_MAX_LINES: i64 = 4;

fn is_menu_state(state: GameState) -> bool {
    matches!(
        state,
        GameState::Title
            | GameState::TypeSelect
            | GameState::LevelSelect
            | GameState::GameOver
            | GameState::HighscoreEntry
    )
}

/// A single frame's raw recognition output, before fusion.
#[derive(Clone, Debug, Default)]
pub struct RawReading {
    pub seq: i64,
    pub ts: f64,
    pub state: GameState,
    pub state_confidence: f64,
    pub score: Option<i64>,
    pub score_confidence: f64,
    pub lines: Option<i64>,
    pub lines_confidence: f64,
    pub level: Option<i64>,
    pub level_confidence: f64,
    pub next_piece: Option<Piece>,
    pub next_confidence: f64,
    pub playfield: Option<Vec<Vec<u8>>>,
    pub playfield_confidence: f64,
    /// Full-rail STATISTICS counts (all 7, values may be None), if read.
    pub statistics: Option<Vec<(Piece, Option<i64>)>>,
    pub statistics_confidence: f64,
    pub current_piece: Option<Piece>,
    pub current_piece_pos: Option<(u32, u32)>,
    pub current_piece_cells: Option<Vec<(u32, u32)>>,
    pub current_piece_confidence: f64,
    pub geometry_confidence: f64,
}

/// Per-field fused confidences (the Python dict, as a struct).
#[derive(Clone, Debug, Default)]
pub struct FusedConfidence {
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

/// The fused, stable state emitted for a frame.
#[derive(Clone, Debug, Default)]
pub struct FusedState {
    pub seq: i64,
    pub ts: f64,
    pub state: GameState,
    pub score: Option<i64>,
    pub lines: Option<i64>,
    pub level: Option<i64>,
    pub next_piece: Option<Piece>,
    pub playfield: Option<Vec<Vec<u8>>>,
    pub statistics: Option<StatisticsMap>,
    pub current_piece: Option<Piece>,
    pub current_piece_pos: Option<(u32, u32)>,
    pub current_piece_cells: Option<Vec<(u32, u32)>>,
    pub confidence: FusedConfidence,
    pub is_new_game: bool,
}

/// Per-field tracked value with a voting window and held value.
struct Tracked<T: Copy + PartialEq> {
    window: VecDeque<(T, f64)>,
    capacity: usize,
    value: Option<T>,
    confidence: f64,
    challenge_value: Option<T>,
    challenge_count: u32,
}

impl<T: Copy + PartialEq> Tracked<T> {
    fn new(capacity: usize) -> Self {
        Self {
            window: VecDeque::with_capacity(capacity),
            capacity,
            value: None,
            confidence: 0.0,
            challenge_value: None,
            challenge_count: 0,
        }
    }

    fn clear(&mut self) {
        self.window.clear();
        self.value = None;
        self.confidence = 0.0;
        self.challenge_value = None;
        self.challenge_count = 0;
    }

    fn push(&mut self, value: T, conf: f64) {
        if self.window.len() == self.capacity {
            self.window.pop_front();
        }
        self.window.push_back((value, conf));
    }

    /// Confidence-weighted majority `(value, mean_conf_of_winner_votes)`,
    /// ties resolving to the first-inserted key (Python Counter semantics).
    fn majority(&self) -> Option<(T, f64)> {
        if self.window.is_empty() {
            return None;
        }
        let mut keys: Vec<T> = Vec::new();
        let mut weights: Vec<f64> = Vec::new();
        for &(value, conf) in &self.window {
            match keys.iter().position(|k| *k == value) {
                Some(i) => weights[i] += conf,
                None => {
                    keys.push(value);
                    weights.push(conf);
                }
            }
        }
        let mut best = 0usize;
        for i in 1..keys.len() {
            if weights[i] > weights[best] {
                best = i;
            }
        }
        let winner = keys[best];
        let confs: Vec<f64> = self
            .window
            .iter()
            .filter(|(v, _)| *v == winner)
            .map(|(_, c)| *c)
            .collect();
        let mean = confs.iter().sum::<f64>() / confs.len() as f64;
        Some((winner, mean))
    }
}

/// Fuses a stream of [`RawReading`] into stable [`FusedState`]s.
pub struct FusionEngine {
    cfg: FusionConfig,
    score: Tracked<i64>,
    lines: Tracked<i64>,
    level: Tracked<i64>,
    next: Tracked<Piece>,
    next_hold: u32,
    current: Tracked<Piece>,
    state_window: VecDeque<(GameState, f64)>,
    playfield: Option<Vec<Vec<u8>>>,
    playfield_conf: f64,
    /// Monotonic held STATISTICS counts, insertion-ordered like a Python dict.
    stats: Vec<(Piece, i64)>,
    stats_conf: f64,
    current_pos: Option<(u32, u32)>,
    current_cells: Option<Vec<(u32, u32)>>,
    geometry_conf: f64,
    prev_state: GameState,
    menu_streak: u32,
    new_game_armed: bool,
    /// Consecutive in-game frames while armed.
    armed_ingame: u32,
}

impl FusionEngine {
    pub fn new(cfg: FusionConfig) -> Self {
        let w = cfg.vote_window;
        Self {
            score: Tracked::new(w),
            lines: Tracked::new(w),
            level: Tracked::new(w),
            next: Tracked::new(w),
            next_hold: 0,
            current: Tracked::new(w),
            state_window: VecDeque::with_capacity(w),
            playfield: None,
            playfield_conf: 0.0,
            stats: Vec::new(),
            stats_conf: 0.0,
            current_pos: None,
            current_cells: None,
            geometry_conf: 1.0,
            prev_state: GameState::Unknown,
            menu_streak: 0,
            new_game_armed: false,
            armed_ingame: 0,
            cfg,
        }
    }

    /// Clear monotonic baselines and held values for a new game.
    pub fn reset_game(&mut self) {
        self.score.clear();
        self.lines.clear();
        self.level.clear();
        self.next.clear();
        self.current.clear();
        self.playfield = None;
        self.playfield_conf = 0.0;
        self.stats.clear();
        self.stats_conf = 0.0;
        self.current_pos = None;
        self.current_cells = None;
        self.next_hold = 0;
        self.menu_streak = 0;
        self.new_game_armed = false;
        self.armed_ingame = 0;
    }

    /// Fold `reading` into the fused state and return the new state.
    pub fn update(&mut self, reading: &RawReading) -> FusedState {
        let state = self.fuse_state(reading);
        let is_new_game = self.detect_new_game(state, reading);
        if is_new_game {
            self.reset_game();
        }
        self.prev_state = state;
        self.geometry_conf = reading.geometry_confidence;

        let (score, lines, level, next_piece, playfield, statistics, current_piece);
        if state == GameState::Paused {
            self.clear_challenges();
            score = self.held_numeric_score();
            lines = self.held_numeric_lines();
            level = self.held_numeric_level();
            next_piece = self.next.value;
            playfield = self.held_playfield();
            statistics =
                if !self.stats.is_empty() && self.stats_conf >= self.cfg.min_report_confidence {
                    Some(self.stats_map())
                } else {
                    None
                };
            current_piece = self.current.value;
        } else {
            score = fuse_numeric(
                &mut self.score,
                reading.score,
                reading.score_confidence,
                &self.cfg,
            );
            lines = fuse_numeric(
                &mut self.lines,
                reading.lines,
                reading.lines_confidence,
                &self.cfg,
            );
            level = fuse_numeric(
                &mut self.level,
                reading.level,
                reading.level_confidence,
                &self.cfg,
            );
            next_piece = self.fuse_piece(reading);
            playfield = self.fuse_playfield(reading);
            statistics = self.fuse_statistics(reading);
            current_piece = self.fuse_current_piece(reading);
        }

        let g = self.geometry_conf.clamp(0.0, 1.0);
        FusedState {
            seq: reading.seq,
            ts: reading.ts,
            state,
            score,
            lines,
            level,
            next_piece,
            playfield,
            statistics,
            current_piece,
            current_piece_pos: self.current_pos,
            current_piece_cells: self.current_cells.clone(),
            is_new_game,
            confidence: FusedConfidence {
                score: self.score.confidence * g,
                lines: self.lines.confidence * g,
                level: self.level.confidence * g,
                next_piece: self.next.confidence * g,
                playfield: self.playfield_conf * g,
                statistics: self.stats_conf * g,
                current_piece: self.current.confidence * g,
                geometry: g,
                overall: self.overall_confidence() * g,
            },
        }
    }

    fn stats_map(&self) -> StatisticsMap {
        StatisticsMap(self.stats.iter().map(|&(p, v)| (p, Some(v))).collect())
    }

    fn fuse_state(&mut self, reading: &RawReading) -> GameState {
        if self.state_window.len() == self.cfg.vote_window {
            self.state_window.pop_front();
        }
        self.state_window
            .push_back((reading.state, reading.state_confidence));
        // Confidence-weighted vote with first-inserted tie preference.
        let mut keys: Vec<GameState> = Vec::new();
        let mut weights: Vec<f64> = Vec::new();
        for &(st, conf) in &self.state_window {
            if st == GameState::Unknown {
                continue;
            }
            match keys.iter().position(|k| *k == st) {
                Some(i) => weights[i] += conf,
                None => {
                    keys.push(st);
                    weights.push(conf);
                }
            }
        }
        if keys.is_empty() {
            return if self.prev_state != GameState::Unknown {
                self.prev_state
            } else {
                reading.state
            };
        }
        let mut best = 0usize;
        for i in 1..keys.len() {
            if weights[i] > weights[best] {
                best = i;
            }
        }
        keys[best]
    }

    fn detect_new_game(&mut self, state: GameState, reading: &RawReading) -> bool {
        if is_menu_state(state) {
            self.armed_ingame = 0;
            self.menu_streak += 1;
            if self.menu_streak >= self.cfg.new_game_menu_frames {
                self.new_game_armed = true;
            }
        } else if matches!(state, GameState::InGame | GameState::Paused) {
            self.menu_streak = 0;
        }
        if state == GameState::InGame {
            if self.new_game_armed {
                self.armed_ingame += 1;
                if self.armed_ingame > self.cfg.new_game_confirm_frames {
                    self.new_game_armed = false;
                    self.armed_ingame = 0;
                    return true;
                }
                return false;
            }
            if self.restart_pending(reading) {
                return true;
            }
        }
        false
    }

    fn restart_pending(&self, reading: &RawReading) -> bool {
        let completes = |tracked: &Tracked<i64>, value: Option<i64>, conf: f64| -> bool {
            match (value, tracked.value) {
                (Some(v), Some(held)) => {
                    conf > 0.0
                        && v < held
                        && Some(v) == tracked.challenge_value
                        && tracked.challenge_count + 1 >= MONOTONIC_OVERRIDE_FRAMES
                }
                _ => false,
            }
        };
        reading.score == Some(0)
            && reading.lines.unwrap_or(0) <= RESTART_MAX_LINES
            && completes(&self.score, reading.score, reading.score_confidence)
            && completes(&self.lines, reading.lines, reading.lines_confidence)
    }

    fn clear_challenges(&mut self) {
        for tracked in [&mut self.score, &mut self.lines, &mut self.level] {
            tracked.challenge_value = None;
            tracked.challenge_count = 0;
        }
    }

    fn held_numeric_score(&self) -> Option<i64> {
        held_numeric(&self.score, &self.cfg)
    }

    fn held_numeric_lines(&self) -> Option<i64> {
        held_numeric(&self.lines, &self.cfg)
    }

    fn held_numeric_level(&self) -> Option<i64> {
        held_numeric(&self.level, &self.cfg)
    }

    fn held_playfield(&self) -> Option<Vec<Vec<u8>>> {
        if self.playfield_conf < self.cfg.min_report_confidence {
            return None;
        }
        self.playfield.clone()
    }

    fn fuse_piece(&mut self, reading: &RawReading) -> Option<Piece> {
        let fresh = reading.next_piece.is_some() && reading.next_confidence > 0.0;
        if fresh {
            self.next
                .push(reading.next_piece.unwrap(), reading.next_confidence);
            if let Some((value, conf)) = self.next.majority() {
                self.next.value = Some(value);
                self.next.confidence = conf;
            }
            self.next_hold = 0;
        } else {
            self.next_hold += 1;
            if self.next_hold > NEXT_HOLD_FRAMES {
                self.next.value = None;
                self.next.confidence = 0.0;
            }
        }
        self.next.value
    }

    fn fuse_playfield(&mut self, reading: &RawReading) -> Option<Vec<Vec<u8>>> {
        if reading.playfield.is_some()
            && reading.playfield_confidence >= self.cfg.min_report_confidence
        {
            self.playfield = reading.playfield.clone();
            self.playfield_conf = reading.playfield_confidence;
        } else {
            self.playfield_conf *= self.cfg.confidence_decay;
        }
        if self.playfield_conf < self.cfg.min_report_confidence {
            return None;
        }
        self.playfield.clone()
    }

    fn fuse_statistics(&mut self, reading: &RawReading) -> Option<StatisticsMap> {
        if let Some(stats) = &reading.statistics
            && reading.statistics_confidence > 0.0
        {
            for &(piece, value) in stats {
                let Some(value) = value else { continue };
                match self.stats.iter_mut().find(|(p, _)| *p == piece) {
                    Some((_, prev)) => {
                        if value >= *prev {
                            *prev = value;
                        }
                    }
                    None => self.stats.push((piece, value)),
                }
            }
            self.stats_conf = reading.statistics_confidence;
        } else {
            self.stats_conf *= self.cfg.confidence_decay;
        }
        if self.stats.is_empty() || self.stats_conf < self.cfg.min_report_confidence {
            return None;
        }
        Some(self.stats_map())
    }

    fn fuse_current_piece(&mut self, reading: &RawReading) -> Option<Piece> {
        let fresh = reading.current_piece.is_some() && reading.current_piece_confidence > 0.0;
        if fresh {
            self.current.push(
                reading.current_piece.unwrap(),
                reading.current_piece_confidence,
            );
            if let Some((value, conf)) = self.current.majority() {
                self.current.value = Some(value);
                self.current.confidence = conf;
            }
            if reading.current_piece_pos.is_some() {
                self.current_pos = reading.current_piece_pos;
            }
            self.current_cells = reading.current_piece_cells.clone();
        } else {
            self.current.confidence *= self.cfg.confidence_decay;
        }
        if self.current.confidence < self.cfg.min_report_confidence {
            self.current_cells = None;
            return None;
        }
        self.current.value
    }

    fn overall_confidence(&self) -> f64 {
        0.30 * self.score.confidence
            + 0.15 * self.lines.confidence
            + 0.15 * self.level.confidence
            + 0.10 * self.next.confidence
            + 0.30 * self.playfield_conf
    }
}

fn held_numeric(tracked: &Tracked<i64>, cfg: &FusionConfig) -> Option<i64> {
    if tracked.confidence < cfg.min_report_confidence {
        return None;
    }
    tracked.value
}

fn fuse_numeric(
    tracked: &mut Tracked<i64>,
    value: Option<i64>,
    conf: f64,
    cfg: &FusionConfig,
) -> Option<i64> {
    let mut fresh = false;
    if let Some(value) = value
        && conf > 0.0
    {
        let decreasing =
            cfg.enforce_monotonic && matches!(tracked.value, Some(held) if value < held);
        if decreasing {
            if Some(value) == tracked.challenge_value {
                tracked.challenge_count += 1;
            } else {
                tracked.challenge_value = Some(value);
                tracked.challenge_count = 1;
            }
            if tracked.challenge_count >= MONOTONIC_OVERRIDE_FRAMES {
                tracked.window.clear();
                tracked.push(value, conf);
                tracked.challenge_count = 0;
                tracked.challenge_value = None;
                fresh = true;
            }
        } else {
            tracked.challenge_count = 0;
            tracked.challenge_value = None;
            tracked.push(value, conf);
            fresh = true;
        }
    }

    if fresh {
        if let Some((voted, vconf)) = tracked.majority() {
            tracked.value = Some(voted);
            tracked.confidence = vconf;
        }
    } else {
        tracked.confidence *= cfg.confidence_decay;
    }

    if tracked.confidence < cfg.min_report_confidence {
        return None;
    }
    tracked.value
}

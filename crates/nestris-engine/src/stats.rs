//! Incremental community statistics (exact port of `stats/engine.py`).

use crate::enums::{GameState, Piece};
use crate::output::{GameStats, LineClears};
use crate::state::fusion::FusedState;
use crate::stats_ext::{ExtTracker, ExtendedStats, PIECE_ORDER};

/// Droughts at or above this length count as "real" droughts (flag/count).
const DROUGHT_FLAG: u32 = 13;

const GAP_CAP_S: f64 = 0.5;
const SCORE_SLACK: i64 = 150;
const MAX_LINES_DELTA: i64 = 8;
const MAX_RAIL_STEP: i64 = 8;
const MIN_RAIL_CONFIDENCE: f64 = 0.5;

fn clear_base_score(size: i64) -> i64 {
    match size {
        1 => 40,
        2 => 100,
        3 => 300,
        4 => 1200,
        _ => unreachable!("clear size 1..=4"),
    }
}

fn clear_reason(size: i64) -> &'static str {
    match size {
        1 => "clear_single",
        2 => "clear_double",
        3 => "clear_triple",
        _ => "clear_tetris",
    }
}

/// A notable observation made by the stats engine (for the event stream).
#[derive(Clone, Debug, PartialEq)]
pub struct StatsEvent {
    pub ts: f64,
    pub field: &'static str,
    pub reason: &'static str,
    pub severity: &'static str,
    pub old: Option<i64>,
    pub new: Option<i64>,
}

/// Attribute a LINES jump to clear sizes using the concurrent SCORE delta.
/// Mirrors the Python enumeration order (non-increasing compositions, sizes
/// descending) and its unique-smallest-residual rule.
pub fn attribute_line_delta(
    delta_lines: i64,
    delta_score: Option<i64>,
    level: Option<i64>,
) -> Option<Vec<i64>> {
    let delta_score = delta_score?;
    let level = level?;
    if delta_score < 0 {
        return None;
    }

    fn compositions(remaining: i64) -> Vec<Vec<i64>> {
        if remaining == 0 {
            return vec![vec![]];
        }
        let mut result = Vec::new();
        for size in (1..=remaining.min(4)).rev() {
            for rest in compositions(remaining - size) {
                if rest.first().is_none_or(|&f| f <= size) {
                    let mut combo = Vec::with_capacity(rest.len() + 1);
                    combo.push(size);
                    combo.extend(rest);
                    result.push(combo);
                }
            }
        }
        result
    }

    let multiplier = level + 1;
    let mut scored: Vec<(i64, Vec<i64>)> = Vec::new();
    for combo in compositions(delta_lines) {
        let base: i64 = combo.iter().map(|&s| clear_base_score(s)).sum();
        let residual = delta_score - base * multiplier;
        if (0..=SCORE_SLACK).contains(&residual) {
            scored.push((residual, combo));
        }
    }
    if scored.is_empty() {
        return None;
    }
    scored.sort_by_key(|(r, _)| *r);
    if scored.len() > 1 && scored[0].0 == scored[1].0 {
        return None;
    }
    Some(scored.swap_remove(0).1)
}

/// Derives [`GameStats`] incrementally from fused states.
pub struct StatsEngine {
    clears: LineClears,
    total_lines: i64,
    tetris_lines: i64,
    pieces: i64,
    drought: u32,
    max_drought: u32,
    prev_lines: Option<i64>,
    prev_score: Option<i64>,
    prev_next: Option<Piece>,
    score_at_prev_lines: Option<i64>,
    lines_fresh: bool,
    rail_total: Option<i64>,
    rail_i_count: Option<i64>,
    active_seconds: f64,
    last_in_game_ts: Option<f64>,
    score: i64,
    level: Option<i64>,
    pending_events: Vec<StatsEvent>,
    /// Extended dashboard statistics (piece distribution, points breakdown,
    /// board metrics, trends). Kept in lockstep with the base observations.
    ext: ExtTracker,
}

impl Default for StatsEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl StatsEngine {
    pub fn new() -> Self {
        Self {
            clears: LineClears::default(),
            total_lines: 0,
            tetris_lines: 0,
            pieces: 0,
            drought: 0,
            max_drought: 0,
            prev_lines: None,
            prev_score: None,
            prev_next: None,
            score_at_prev_lines: None,
            lines_fresh: false,
            rail_total: None,
            rail_i_count: None,
            active_seconds: 0.0,
            last_in_game_ts: None,
            score: 0,
            level: None,
            pending_events: Vec::new(),
            ext: ExtTracker::new(),
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Return and clear the events accumulated since the last drain.
    pub fn drain_events(&mut self) -> Vec<StatsEvent> {
        std::mem::take(&mut self.pending_events)
    }

    pub fn update(&mut self, state: &FusedState) -> GameStats {
        if state.state == GameState::InGame {
            self.observe(state);
        }
        self.snapshot()
    }

    fn observe(&mut self, state: &FusedState) {
        if let Some(prev_ts) = self.last_in_game_ts {
            let gap = state.ts - prev_ts;
            if gap > 0.0 {
                self.active_seconds += gap.min(GAP_CAP_S);
            }
        }
        self.last_in_game_ts = Some(state.ts);

        if state.level.is_some() {
            self.level = state.level;
        }
        self.observe_lines(state);
        self.observe_score(state.score);
        self.observe_piece(state.next_piece);
        self.reconcile_statistics(state);
        self.observe_board(state);
    }

    /// Feed the settled stack (falling piece masked out) to the extended
    /// tracker for board-shape metrics and the height timeline.
    fn observe_board(&mut self, state: &FusedState) {
        let Some(playfield) = &state.playfield else {
            return;
        };
        let mut grid = playfield.clone();
        if let Some(cells) = &state.current_piece_cells {
            for &(r, c) in cells {
                if let Some(cell) = grid.get_mut(r as usize).and_then(|row| row.get_mut(c as usize))
                {
                    *cell = 0;
                }
            }
        }
        self.ext
            .on_board(&grid, state.ts, self.drought >= DROUGHT_FLAG);
    }

    fn observe_lines(&mut self, state: &FusedState) {
        let Some(lines) = state.lines else {
            self.lines_fresh = false;
            return;
        };
        let Some(prev) = self.prev_lines else {
            self.set_lines_baseline(lines, state.score);
            return;
        };

        let delta = lines - prev;
        let was_fresh = self.lines_fresh;
        self.lines_fresh = true;
        if delta == 0 {
            if let Some(score) = state.score.filter(|&s| s != 0) {
                self.score_at_prev_lines = Some(score);
            }
            return;
        }
        if delta < 0 {
            return;
        }
        if delta > MAX_LINES_DELTA {
            self.pending_events.push(StatsEvent {
                ts: state.ts,
                field: "lines",
                reason: "lines_jump_ignored",
                severity: "warn",
                old: Some(prev),
                new: Some(lines),
            });
            return;
        }

        let delta_score = match (state.score, self.score_at_prev_lines) {
            (Some(s), Some(base)) => Some(s - base),
            _ => None,
        };
        let combo = attribute_line_delta(delta, delta_score, self.level).unwrap_or_else(|| {
            if delta > 4 || (!was_fresh && delta > 1) {
                self.pending_events.push(StatsEvent {
                    ts: state.ts,
                    field: "lines",
                    reason: "lines_attribution_uncertain",
                    severity: "warn",
                    old: Some(prev),
                    new: Some(lines),
                });
            }
            let mut combo = Vec::new();
            let mut remaining = delta;
            while remaining > 0 {
                let step = remaining.min(4);
                combo.push(step);
                remaining -= step;
            }
            combo
        });
        for step in combo {
            self.record_clear(step, state.ts);
        }
        self.set_lines_baseline(lines, state.score);
    }

    fn set_lines_baseline(&mut self, lines: i64, score: Option<i64>) {
        self.prev_lines = Some(lines);
        self.lines_fresh = true;
        if let Some(score) = score {
            self.score_at_prev_lines = Some(score);
        }
    }

    fn record_clear(&mut self, count: i64, ts: f64) {
        self.total_lines += count;
        match count {
            1 => self.clears.single += 1,
            2 => self.clears.double += 1,
            3 => self.clears.triple += 1,
            4 => {
                self.clears.tetris += 1;
                self.tetris_lines += 4;
            }
            _ => {}
        }
        self.ext
            .on_clear(count, self.level, self.total_lines, self.tetris_lines);
        self.pending_events.push(StatsEvent {
            ts,
            field: "lines",
            reason: clear_reason(count),
            severity: "info",
            old: None,
            new: Some(count),
        });
    }

    fn observe_score(&mut self, score: Option<i64>) {
        let Some(score) = score else { return };
        if self.prev_score.is_none_or(|prev| score >= prev) {
            self.score = score;
            self.prev_score = Some(score);
        }
    }

    fn observe_piece(&mut self, next_piece: Option<Piece>) {
        let Some(next_piece) = next_piece else { return };
        if !next_piece.is_piece() {
            return;
        }
        let Some(prev) = self.prev_next else {
            self.prev_next = Some(next_piece);
            return;
        };
        if next_piece == prev {
            return;
        }
        let spawned = prev;
        self.prev_next = Some(next_piece);
        self.pieces += 1;
        self.ext.on_spawn(spawned, self.drought);
        if spawned == Piece::I {
            self.drought = 0;
        } else {
            self.drought += 1;
            self.max_drought = self.max_drought.max(self.drought);
        }
    }

    fn reconcile_statistics(&mut self, state: &FusedState) {
        let Some(stats) = &state.statistics else {
            return;
        };
        if state.confidence.statistics < MIN_RAIL_CONFIDENCE {
            return;
        }
        let counts: Vec<i64> = stats.0.iter().filter_map(|(_, v)| *v).collect();
        if counts.len() != 7 {
            return;
        }
        let total: i64 = counts.iter().sum();
        let i_count = stats.get(Piece::I);

        if let Some(rail) = self.rail_total {
            if total > rail {
                let step = total - rail;
                if step <= MAX_RAIL_STEP && total > self.pieces {
                    self.pieces = total;
                }
            }
        } else if total > 0 && total <= MAX_RAIL_STEP {
            self.pieces = self.pieces.max(total);
        }
        self.rail_total = Some(total);

        // A confident full rail read: adopt it as the piece distribution.
        let rail: [i64; 7] = std::array::from_fn(|i| stats.get(PIECE_ORDER[i]).unwrap_or(0));
        self.ext.on_rail(rail);

        if let (Some(i), Some(prev_i)) = (i_count, self.rail_i_count)
            && i > prev_i
        {
            self.ext.on_rail_i_reset(self.drought);
            self.drought = 0;
        }
        if i_count.is_some() {
            self.rail_i_count = i_count;
        }
    }

    /// The extended dashboard block for the current game. Bounded clone
    /// (small structs plus two capped series).
    pub fn extended(&self) -> ExtendedStats {
        let mut ext = self.ext.snapshot(
            self.score,
            self.prev_lines,
            self.level,
            &self.clears,
            self.total_lines,
            self.tetris_lines,
            self.drought,
        );
        ext.i_drought.max = self.max_drought;
        ext
    }

    fn snapshot(&self) -> GameStats {
        let seconds = self.active_seconds;
        let (pps, score_per_min) = if seconds > 0.0 {
            (
                Some(self.pieces as f64 / seconds),
                Some(self.score as f64 / (seconds / 60.0)),
            )
        } else {
            (None, None)
        };
        let tetris_rate = if self.total_lines > 0 {
            Some(self.tetris_lines as f64 / self.total_lines as f64)
        } else {
            None
        };
        GameStats {
            pps,
            tetris_rate,
            burn: self.total_lines - self.tetris_lines,
            drought: self.drought,
            max_drought: self.max_drought,
            clears: self.clears.clone(),
            score_per_min,
            pieces: self.pieces,
            active_seconds: if seconds > 0.0 { Some(seconds) } else { None },
        }
    }
}

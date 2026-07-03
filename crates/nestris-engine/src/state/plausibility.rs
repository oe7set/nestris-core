//! NES-rules plausibility filter (exact port of `state/plausibility.py`).

use crate::config::PlausibilityConfig;
use crate::enums::GameState;

/// Lines needed for the first level-up from `start_level` (NES A-type).
pub fn first_level_threshold(start_level: i64) -> i64 {
    (start_level * 10 + 10).min((start_level * 10 - 50).max(100))
}

/// Expected level for a game started at `start_level` after `lines`.
pub fn expected_level(start_level: i64, lines: i64) -> i64 {
    let threshold = first_level_threshold(start_level);
    if lines < threshold {
        start_level
    } else {
        start_level + 1 + (lines - threshold).div_euclid(10)
    }
}

/// Largest start level consistent with an observed `(level, lines)` pair.
pub fn infer_start_level(level: i64, lines: i64) -> i64 {
    let mut best: Option<i64> = None;
    for s in 0..=level {
        if expected_level(s, lines) == level {
            best = Some(s);
        }
    }
    best.unwrap_or(level)
}

/// A rejection/correction produced by the filter for one field.
#[derive(Clone, Debug, PartialEq)]
pub struct PlausibilityEvent {
    pub ts: f64,
    pub seq: i64,
    pub field: &'static str,
    pub reason: &'static str,
    pub old: Option<i64>,
    pub new: Option<i64>,
    pub severity: &'static str,
    pub confidence: Option<f64>,
}

#[derive(Default)]
struct FieldGuard {
    value: Option<i64>,
    challenge_value: Option<i64>,
    challenge_count: u32,
}

impl FieldGuard {
    fn reset(&mut self) {
        *self = FieldGuard::default();
    }
}

/// The filtered numeric values plus any events generated this frame.
#[derive(Clone, Debug, Default)]
pub struct PlausibilityResult {
    pub score: Option<i64>,
    pub lines: Option<i64>,
    pub level: Option<i64>,
    pub events: Vec<PlausibilityEvent>,
}

/// One frame's inputs to [`PlausibilityFilter::filter`].
pub struct PlausibilityInput {
    pub seq: i64,
    pub ts: f64,
    pub state: GameState,
    pub score: Option<i64>,
    pub lines: Option<i64>,
    pub level: Option<i64>,
    pub score_conf: Option<f64>,
    pub lines_conf: Option<f64>,
    pub level_conf: Option<f64>,
}

/// Validates the fused score/lines/level stream against NES Tetris rules.
pub struct PlausibilityFilter {
    cfg: PlausibilityConfig,
    score: FieldGuard,
    lines: FieldGuard,
    level: FieldGuard,
    start_level: Option<i64>,
    lines_fresh: bool,
}

impl PlausibilityFilter {
    pub fn new(cfg: PlausibilityConfig) -> Self {
        Self {
            cfg,
            score: FieldGuard::default(),
            lines: FieldGuard::default(),
            level: FieldGuard::default(),
            start_level: None,
            lines_fresh: false,
        }
    }

    pub fn reset(&mut self) {
        self.score.reset();
        self.lines.reset();
        self.level.reset();
        self.start_level = None;
        self.lines_fresh = false;
    }

    pub fn filter(&mut self, input: &PlausibilityInput) -> PlausibilityResult {
        if input.state == GameState::Paused {
            return PlausibilityResult {
                score: self.score.value,
                lines: self.lines.value,
                level: self.level.value,
                events: Vec::new(),
            };
        }
        let mut events: Vec<PlausibilityEvent> = Vec::new();
        let out_lines = self.filter_lines(input, &mut events);
        let out_score = self.filter_score(input, &mut events);
        let out_level = self.filter_level(input, out_lines, &mut events);

        if self.start_level.is_none()
            && let (Some(level), Some(lines)) = (out_level, out_lines)
        {
            self.start_level = Some(infer_start_level(level, lines));
        }

        PlausibilityResult {
            score: out_score,
            lines: out_lines,
            level: out_level,
            events,
        }
    }

    fn filter_lines(
        &mut self,
        input: &PlausibilityInput,
        events: &mut Vec<PlausibilityEvent>,
    ) -> Option<i64> {
        let Some(value) = input.lines else {
            self.lines_fresh = false;
            return None;
        };
        let was_fresh = self.lines_fresh;
        self.lines_fresh = true;
        if self.lines.value.is_none() {
            self.lines.value = Some(value);
            return Some(value);
        }
        let delta = value - self.lines.value.unwrap();
        let max_step = if was_fresh {
            self.cfg.max_lines_step
        } else {
            self.cfg.max_lines_skip
        };
        let violation = if delta < 0 {
            Some("monotonic")
        } else if delta > max_step {
            Some("lines_step")
        } else {
            None
        };
        resolve(
            &mut self.lines,
            &self.cfg,
            input.seq,
            input.ts,
            "lines",
            value,
            input.lines_conf,
            violation,
            events,
        )
    }

    fn filter_score(
        &mut self,
        input: &PlausibilityInput,
        events: &mut Vec<PlausibilityEvent>,
    ) -> Option<i64> {
        let value = input.score?;
        if self.score.value.is_none() {
            self.score.value = Some(value);
            return Some(value);
        }
        let delta = value - self.score.value.unwrap();
        let violation = if delta < 0 {
            Some("monotonic")
        } else if delta > self.cfg.max_score_jump {
            Some("score_jump")
        } else {
            None
        };
        resolve(
            &mut self.score,
            &self.cfg,
            input.seq,
            input.ts,
            "score",
            value,
            input.score_conf,
            violation,
            events,
        )
    }

    fn filter_level(
        &mut self,
        input: &PlausibilityInput,
        current_lines: Option<i64>,
        events: &mut Vec<PlausibilityEvent>,
    ) -> Option<i64> {
        let value = input.level?;
        if self.level.value.is_none() {
            self.level.value = Some(value);
            return Some(value);
        }
        let violation = if value < self.level.value.unwrap() {
            Some("monotonic")
        } else if let (Some(start), Some(lines)) = (self.start_level, current_lines)
            && (value - expected_level(start, lines)).abs() > self.cfg.level_tolerance
        {
            Some("level_lines_mismatch")
        } else {
            None
        };
        let accepted = resolve(
            &mut self.level,
            &self.cfg,
            input.seq,
            input.ts,
            "level",
            value,
            input.level_conf,
            violation,
            events,
        );
        if violation.is_some()
            && accepted == Some(value)
            && let Some(lines) = current_lines
        {
            self.start_level = Some(infer_start_level(value, lines));
        }
        accepted
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve(
    guard: &mut FieldGuard,
    cfg: &PlausibilityConfig,
    seq: i64,
    ts: f64,
    field: &'static str,
    value: i64,
    conf: Option<f64>,
    violation: Option<&'static str>,
    events: &mut Vec<PlausibilityEvent>,
) -> Option<i64> {
    let Some(violation) = violation else {
        guard.value = Some(value);
        guard.challenge_value = None;
        guard.challenge_count = 0;
        return Some(value);
    };

    if Some(value) == guard.challenge_value {
        guard.challenge_count += 1;
    } else {
        guard.challenge_value = Some(value);
        guard.challenge_count = 1;
    }

    if guard.challenge_count >= cfg.confirm_frames {
        events.push(PlausibilityEvent {
            ts,
            seq,
            field,
            reason: violation,
            old: guard.value,
            new: Some(value),
            severity: "correct",
            confidence: conf,
        });
        guard.value = Some(value);
        guard.challenge_value = None;
        guard.challenge_count = 0;
        return Some(value);
    }

    events.push(PlausibilityEvent {
        ts,
        seq,
        field,
        reason: violation,
        old: Some(value),
        new: guard.value,
        severity: "reject",
        confidence: conf,
    });
    guard.value
}

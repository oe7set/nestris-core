//! Score-cheat detection.
//!
//! Legal NES-Tetris score gains come from exactly two sources: line clears
//! (base 40/100/300/1200 × (level+1)) and push-down (soft-drop) points, which
//! are small. The Select trick awards a flat +10 000 while a piece falls. The
//! detector therefore explains every settled score gain as
//!
//! ```text
//! ΔSCORE = clear_points(ΔLINES, level) + k · cheat_points + push-down
//! ```
//!
//! and counts `k` as cheat inputs. Gains nothing explains (neither legally
//! nor as whole cheat multiples) are reported as anomalies instead: they are
//! recognition problems, not proof of a cheat.
//!
//! Robustness against OCR errors (a misread ten-thousands digit is exactly a
//! ±10 000 step):
//! - values only count after holding for `confirm_frames` in-game frames;
//! - a detected cheat stays *provisional* for [`CHEAT_HOLD_FRAMES`] and is
//!   retracted if the score falls back below the cheated value;
//! - at most [`MAX_CHEATS_PER_EVENT`] inputs are accepted per scoring step,
//!   bigger jumps are anomalies.

use serde::Serialize;

use crate::enums::GameState;
use crate::output::OutputFrame;

use super::IntegrityConfig;

/// Frames a detected cheat stays provisional before it is confirmed.
const CHEAT_HOLD_FRAMES: u32 = 90;
/// More cheat inputs than this in one scoring step is treated as a misread.
const MAX_CHEATS_PER_EVENT: i64 = 2;
/// Lines deltas above this are not attributed (a recognition gap).
const MAX_ATTRIBUTED_LINES: i64 = 16;

/// A detector observation for the host's event stream.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IntegrityEvent {
    /// Confirmed cheat input(s). `count` is this step, `total` the game total.
    Cheat {
        seq: i64,
        ts: f64,
        count: u32,
        total: u32,
        points: i64,
        score_before: i64,
        score_after: i64,
        lines_delta: i64,
    },
    /// A score gain that neither legal scoring nor cheat multiples explain.
    ScoreAnomaly {
        seq: i64,
        ts: f64,
        score_before: i64,
        score_after: i64,
        lines_delta: i64,
        unexplained: i64,
    },
}

/// A value that must hold for N frames before it replaces the settled one.
#[derive(Default)]
struct Settle {
    value: Option<i64>,
    candidate: Option<i64>,
    count: u32,
}

impl Settle {
    /// Feed one reading; returns the new value when it settles.
    fn feed(&mut self, v: i64, need: u32) -> Option<i64> {
        if self.value == Some(v) {
            self.candidate = None;
            self.count = 0;
            return None;
        }
        if self.candidate == Some(v) {
            self.count += 1;
        } else {
            self.candidate = Some(v);
            self.count = 1;
        }
        if self.count >= need.max(1) {
            self.value = Some(v);
            self.candidate = None;
            self.count = 0;
            return Some(v);
        }
        None
    }
}

struct Provisional {
    seq: i64,
    ts: f64,
    count: u32,
    score_before: i64,
    score_after: i64,
    lines_delta: i64,
    age: u32,
}

enum Verdict {
    /// `level_offset`: only explained with the level misread by one.
    Legit {
        clear_points: i64,
        level_offset: bool,
    },
    Cheat {
        count: i64,
        clear_points: i64,
    },
    Unexplained {
        amount: i64,
        clear_points: i64,
    },
}

/// Per-game cheat detector; feed every output frame of one game, call
/// [`CheatDetector::finish`] at the game end, [`CheatDetector::reset`] for
/// the next game.
pub struct CheatDetector {
    cfg: IntegrityConfig,
    score: Settle,
    lines: Settle,
    level: Settle,
    eval_score: Option<i64>,
    eval_lines: Option<i64>,
    eval_level: Option<i64>,
    /// Frames a settled score gain has waited for its LINES change.
    pending: Option<u32>,
    provisional: Option<Provisional>,
    last_seq: i64,
    last_ts: f64,
    cheated: u32,
    cheat_points: i64,
    clear_points: i64,
    unexplained: i64,
    anomalies: u32,
    level_offset_steps: u32,
}

impl CheatDetector {
    pub fn new(cfg: IntegrityConfig) -> Self {
        Self {
            cfg,
            score: Settle::default(),
            lines: Settle::default(),
            level: Settle::default(),
            eval_score: None,
            eval_lines: None,
            eval_level: None,
            pending: None,
            provisional: None,
            last_seq: 0,
            last_ts: 0.0,
            cheated: 0,
            cheat_points: 0,
            clear_points: 0,
            unexplained: 0,
            anomalies: 0,
            level_offset_steps: 0,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new(self.cfg.clone());
    }

    /// Confirmed cheat inputs this game (`0` = clean).
    pub fn cheated(&self) -> u32 {
        self.cheated
    }

    /// Points attributed to confirmed cheats.
    pub fn cheat_points(&self) -> i64 {
        self.cheat_points
    }

    /// Points attributed to legal line clears.
    pub fn clear_points(&self) -> i64 {
        self.clear_points
    }

    /// Sum of gains beyond push-down slack that nothing explained.
    pub fn unexplained_points(&self) -> i64 {
        self.unexplained
    }

    /// Number of [`IntegrityEvent::ScoreAnomaly`] events raised.
    pub fn anomalies(&self) -> u32 {
        self.anomalies
    }

    /// Scoring steps that only fit with the LEVEL reading off by one (a
    /// misread level digit; the step itself is legal).
    pub fn level_offset_steps(&self) -> u32 {
        self.level_offset_steps
    }

    /// The currently settled score, if any.
    pub fn settled_score(&self) -> Option<i64> {
        self.score.value
    }

    /// Feed one output frame. Only in-game frames are evaluated.
    pub fn push(&mut self, frame: &OutputFrame) -> Vec<IntegrityEvent> {
        let mut events = Vec::new();
        if frame.game_state != GameState::InGame {
            return events;
        }
        self.last_seq = frame.seq;
        self.last_ts = frame.ts;
        let need = self.cfg.confirm_frames;

        if let Some(level) = frame.fields.level
            && let Some(settled) = self.level.feed(level, need)
            && self.eval_level.is_none()
        {
            self.eval_level = Some(settled);
        }

        let lines_settled = frame.fields.lines.and_then(|l| self.lines.feed(l, need));
        let score_settled = frame.fields.score.and_then(|s| self.score.feed(s, need));

        if let Some(score) = score_settled {
            self.on_score(score, &mut events);
        }
        if let Some(lines) = lines_settled {
            self.on_lines(lines, score_settled.is_some(), &mut events);
        }

        if let Some(age) = &mut self.pending {
            *age += 1;
            if *age >= self.cfg.pair_window_frames {
                self.evaluate(&mut events);
            }
        }
        if let Some(p) = &mut self.provisional {
            p.age += 1;
            if p.age >= CHEAT_HOLD_FRAMES {
                self.confirm_provisional(&mut events);
            }
        }
        events
    }

    /// Evaluate anything still pending (game end / shutdown).
    pub fn finish(&mut self) -> Vec<IntegrityEvent> {
        let mut events = Vec::new();
        if self.pending.is_some() {
            self.evaluate(&mut events);
        }
        self.confirm_provisional(&mut events);
        events
    }

    fn on_score(&mut self, score: i64, events: &mut Vec<IntegrityEvent>) {
        // A fall-back below a provisional cheat means the jump was a misread.
        if let Some(p) = &self.provisional
            && score < p.score_after
        {
            self.provisional = None;
        }
        let Some(base) = self.eval_score else {
            self.eval_score = Some(score);
            self.eval_lines = self.lines.value;
            self.eval_level = self.level.value;
            return;
        };
        if score < base {
            // Correction of an earlier misread: rebase, nothing to judge.
            self.eval_score = Some(score);
            self.pending = None;
            return;
        }
        if score == base {
            return;
        }
        self.pending.get_or_insert(0);
        // LINES already moved (it settled first): judge immediately.
        if self.lines.value.is_some() && self.lines.value != self.eval_lines {
            self.evaluate(events);
        }
    }

    fn on_lines(&mut self, lines: i64, score_moved: bool, events: &mut Vec<IntegrityEvent>) {
        let Some(base) = self.eval_lines else {
            self.eval_lines = Some(lines);
            return;
        };
        if lines < base {
            self.eval_lines = Some(lines);
            return;
        }
        if self.pending.is_some() && !score_moved {
            self.evaluate(events);
        }
        // Otherwise the LINES change waits for its score gain (or is a
        // capped-score game where SCORE never moves; the next evaluation
        // pairs it, which is harmless because the gain is zero).
    }

    fn evaluate(&mut self, events: &mut Vec<IntegrityEvent>) {
        self.pending = None;
        let (Some(before), Some(after)) = (self.eval_score, self.score.value) else {
            return;
        };
        let lines_delta = match (self.eval_lines, self.lines.value) {
            (Some(a), Some(b)) => (b - a).max(0),
            _ => 0,
        };
        let mut levels: Vec<i64> = [self.eval_level, self.level.value]
            .into_iter()
            .flatten()
            .collect();
        levels.dedup();

        self.eval_score = Some(after);
        self.eval_lines = self.lines.value.or(self.eval_lines);
        self.eval_level = self.level.value.or(self.eval_level);

        let delta = after - before;
        if delta <= 0 {
            return;
        }
        if lines_delta > 0 && levels.is_empty() {
            // No multiplier known: cannot judge a clear. Skip rather than guess.
            return;
        }
        match self.classify(delta, lines_delta, &levels) {
            Verdict::Legit {
                clear_points,
                level_offset,
            } => {
                self.clear_points += clear_points;
                self.level_offset_steps += u32::from(level_offset);
            }
            Verdict::Cheat {
                count,
                clear_points,
            } => {
                self.clear_points += clear_points;
                // A second detection while one is provisional confirms the first.
                self.confirm_provisional(events);
                self.provisional = Some(Provisional {
                    seq: self.last_seq,
                    ts: self.last_ts,
                    count: count as u32,
                    score_before: before,
                    score_after: after,
                    lines_delta,
                    age: 0,
                });
            }
            Verdict::Unexplained {
                amount,
                clear_points,
            } => {
                self.clear_points += clear_points;
                self.unexplained += amount;
                if amount > self.cfg.anomaly_threshold {
                    self.anomalies += 1;
                    events.push(IntegrityEvent::ScoreAnomaly {
                        seq: self.last_seq,
                        ts: self.last_ts,
                        score_before: before,
                        score_after: after,
                        lines_delta,
                        unexplained: amount,
                    });
                }
            }
        }
    }

    fn confirm_provisional(&mut self, events: &mut Vec<IntegrityEvent>) {
        let Some(p) = self.provisional.take() else {
            return;
        };
        self.cheated += p.count;
        let points = i64::from(p.count) * self.cfg.cheat_points;
        self.cheat_points += points;
        events.push(IntegrityEvent::Cheat {
            seq: p.seq,
            ts: p.ts,
            count: p.count,
            total: self.cheated,
            points,
            score_before: p.score_before,
            score_after: p.score_after,
            lines_delta: p.lines_delta,
        });
    }

    fn classify(&self, delta: i64, lines_delta: i64, levels: &[i64]) -> Verdict {
        let candidates = clear_point_candidates(lines_delta, levels);
        // A level digit misread by one (5↔6, 8↔9) shifts the multiplier by
        // one; the shift (at most 1200 per tetris) is far below a cheat, so
        // tolerating it never hides one.
        let widened: Vec<i64> = levels
            .iter()
            .flat_map(|&lv| [lv - 1, lv, lv + 1])
            .filter(|&lv| lv >= 0)
            .collect();
        let wide = clear_point_candidates(lines_delta, &widened);
        let soft = self.cfg.softdrop_slack;

        // Legal: a clear total plus push-down points.
        if let Some(p) = best_fit(&candidates, delta, 0, soft) {
            return Verdict::Legit {
                clear_points: p,
                level_offset: false,
            };
        }
        if let Some(p) = best_fit(&wide, delta, 0, soft) {
            return Verdict::Legit {
                clear_points: p,
                level_offset: true,
            };
        }

        // Cheat: the remainder after whole cheat multiples is legal.
        let c = self.cfg.cheat_points;
        if c > 0 {
            let tol = self.cfg.cheat_slack;
            for k in 1..=MAX_CHEATS_PER_EVENT {
                if let Some(p) = best_fit(&wide, delta - k * c, -tol, soft + tol) {
                    return Verdict::Cheat {
                        count: k,
                        clear_points: p,
                    };
                }
            }
        }

        // Unexplained: attribute the closest clear total not above the gain.
        let clear_points = candidates
            .iter()
            .copied()
            .filter(|&p| p <= delta)
            .max()
            .unwrap_or(0);
        Verdict::Unexplained {
            amount: delta - clear_points - soft.min(delta - clear_points),
            clear_points,
        }
    }
}

/// Clear totals for `lines` cleared lines at any of `levels` (both the level
/// before and after a step are tried: the multiplier at a level-up is the
/// one ambiguity NES scoring has).
fn clear_point_candidates(lines: i64, levels: &[i64]) -> Vec<i64> {
    if lines <= 0 {
        return vec![0];
    }
    if lines > MAX_ATTRIBUTED_LINES {
        return Vec::new();
    }
    let bases = clear_base_sums(lines as usize);
    let mut out: Vec<i64> = levels
        .iter()
        .flat_map(|&lv| bases.iter().map(move |&b| b * (lv.max(0) + 1)))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Every base-score sum reachable by clearing exactly `n` lines in steps of
/// 1–4 lines (40/100/300/1200).
fn clear_base_sums(n: usize) -> Vec<i64> {
    const BASE: [i64; 5] = [0, 40, 100, 300, 1200];
    let mut reach: Vec<Vec<i64>> = vec![Vec::new(); n + 1];
    reach[0].push(0);
    for i in 1..=n {
        let mut sums = Vec::new();
        for size in 1..=4.min(i) {
            for &prev in &reach[i - size] {
                sums.push(prev + BASE[size]);
            }
        }
        sums.sort_unstable();
        sums.dedup();
        reach[i] = sums;
    }
    reach.swap_remove(n)
}

/// The candidate leaving the smallest residual inside `[lo, hi]`.
fn best_fit(candidates: &[i64], amount: i64, lo: i64, hi: i64) -> Option<i64> {
    candidates
        .iter()
        .copied()
        .filter(|&p| (lo..=hi).contains(&(amount - p)))
        .min_by_key(|&p| (amount - p).abs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::{Confidence, Fields, GameStats};

    /// Builds in-game frames holding each (score, lines, level) for `hold`
    /// frames, like a settled OCR stream.
    struct Feed {
        det: CheatDetector,
        seq: i64,
        events: Vec<IntegrityEvent>,
    }

    impl Feed {
        fn new() -> Self {
            Self {
                det: CheatDetector::new(IntegrityConfig::default()),
                seq: 0,
                events: Vec::new(),
            }
        }

        fn hold(&mut self, score: i64, lines: i64, level: i64, frames: u32) -> &mut Self {
            for _ in 0..frames {
                let f = frame(self.seq, GameState::InGame, score, lines, level);
                self.events.extend(self.det.push(&f));
                self.seq += 1;
            }
            self
        }

        fn finish(&mut self) -> u32 {
            self.events.extend(self.det.finish());
            self.det.cheated()
        }
    }

    fn frame(seq: i64, state: GameState, score: i64, lines: i64, level: i64) -> OutputFrame {
        OutputFrame {
            schema_version: 4,
            seq,
            ts: seq as f64 / 60.0,
            region: crate::enums::Region::Ntsc,
            game_state: state,
            fields: Fields {
                score: Some(score),
                lines: Some(lines),
                level: Some(level),
                ..Default::default()
            },
            stats: GameStats::default(),
            stats_ext: None,
            confidence: Confidence {
                overall: 0.95,
                ..Default::default()
            },
            events: Vec::new(),
        }
    }

    #[test]
    fn cheat_without_lines_counts_once() {
        let mut f = Feed::new();
        f.hold(0, 0, 18, 30).hold(10_000, 0, 18, 200);
        assert_eq!(f.finish(), 1);
        assert_eq!(f.det.cheat_points(), 10_000);
        assert!(matches!(
            f.events[0],
            IntegrityEvent::Cheat {
                count: 1,
                total: 1,
                ..
            }
        ));
    }

    #[test]
    fn cheat_with_pushdown_points() {
        let mut f = Feed::new();
        f.hold(1_234, 10, 18, 30)
            .hold(1_234 + 10_000 + 14, 10, 18, 200);
        assert_eq!(f.finish(), 1);
    }

    #[test]
    fn cheat_merged_with_tetris_counts_once() {
        let mut f = Feed::new();
        // Tetris at level 18: 1200 × 19 = 22 800, plus the cheat.
        f.hold(5_000, 20, 18, 30)
            .hold(5_000 + 22_800 + 10_000, 20, 18, 5)
            .hold(5_000 + 22_800 + 10_000, 24, 18, 200);
        assert_eq!(f.finish(), 1);
        assert_eq!(f.det.clear_points(), 22_800);
    }

    #[test]
    fn two_cheats_count_two() {
        let mut f = Feed::new();
        f.hold(0, 0, 9, 30)
            .hold(10_000, 0, 9, 120)
            .hold(10_006, 0, 9, 60)
            .hold(20_006, 0, 9, 120);
        assert_eq!(f.finish(), 2);
    }

    #[test]
    fn short_ocr_glitch_is_ignored() {
        let mut f = Feed::new();
        f.hold(4_000, 5, 18, 30)
            .hold(14_000, 5, 18, 5)
            .hold(4_000, 5, 18, 100);
        assert_eq!(f.finish(), 0);
    }

    #[test]
    fn persistent_misread_that_falls_back_is_retracted() {
        let mut f = Feed::new();
        f.hold(4_000, 5, 18, 30)
            .hold(14_000, 5, 18, 40)
            .hold(4_000, 5, 18, 100);
        assert_eq!(f.finish(), 0);
    }

    #[test]
    fn legal_clears_and_pushdown_are_clean() {
        let mut f = Feed::new();
        f.hold(0, 0, 18, 30)
            .hold(12, 0, 18, 30) // push-down
            .hold(12 + 760, 1, 18, 30) // single at 18: 40 × 19
            .hold(12 + 760 + 22_800, 5, 18, 30) // tetris
            .hold(12 + 760 + 22_800 + 5_700, 8, 19, 60); // triple across level-up: 300 × 19
        assert_eq!(f.finish(), 0);
        assert!(f.events.is_empty(), "{:?}", f.events);
        assert_eq!(f.det.clear_points(), 760 + 22_800 + 5_700);
    }

    #[test]
    fn score_lags_lines_by_a_few_frames() {
        let mut f = Feed::new();
        f.hold(1_000, 10, 18, 30)
            .hold(1_000, 14, 18, 15)
            .hold(1_000 + 22_800, 14, 18, 60);
        assert_eq!(f.finish(), 0);
        assert!(f.events.is_empty(), "{:?}", f.events);
    }

    #[test]
    fn level_misread_by_one_is_legal_but_counted() {
        // Real level 5 (multiplier 6) read as 6: tetris = 7 200.
        let mut f = Feed::new();
        f.hold(1_000, 10, 6, 30)
            .hold(8_210, 14, 6, 60)
            .hold(15_410, 18, 6, 60);
        assert_eq!(f.finish(), 0);
        assert!(f.events.is_empty(), "{:?}", f.events);
        assert_eq!(f.det.level_offset_steps(), 2);
        assert_eq!(f.det.clear_points(), 14_400);
    }

    #[test]
    fn cheat_is_found_despite_a_level_misread() {
        let mut f = Feed::new();
        f.hold(1_000, 10, 6, 30)
            .hold(1_000 + 7_200 + 10_000, 14, 6, 200);
        assert_eq!(f.finish(), 1);
    }

    #[test]
    fn unexplained_gain_is_an_anomaly_not_a_cheat() {
        let mut f = Feed::new();
        f.hold(1_000, 10, 18, 30).hold(6_000, 10, 18, 60);
        assert_eq!(f.finish(), 0);
        assert!(matches!(
            f.events[0],
            IntegrityEvent::ScoreAnomaly {
                unexplained: 4_800,
                ..
            }
        ));
    }

    #[test]
    fn reset_clears_the_count() {
        let mut f = Feed::new();
        f.hold(0, 0, 18, 30).hold(10_000, 0, 18, 200);
        assert_eq!(f.finish(), 1);
        f.det.reset();
        assert_eq!(f.det.cheated(), 0);
    }

    #[test]
    fn non_ingame_frames_are_ignored() {
        let mut det = CheatDetector::new(IntegrityConfig::default());
        for seq in 0..100 {
            det.push(&frame(seq, GameState::Paused, seq * 1_000, 0, 0));
        }
        det.finish();
        assert_eq!(det.cheated(), 0);
        assert_eq!(det.settled_score(), None);
    }

    #[test]
    fn base_sums_cover_mixed_clears() {
        assert_eq!(clear_base_sums(1), vec![40]);
        assert_eq!(clear_base_sums(2), vec![80, 100]);
        assert!(clear_base_sums(5).contains(&1240)); // tetris + single
        assert!(clear_base_sums(5).contains(&200)); // five singles
    }
}

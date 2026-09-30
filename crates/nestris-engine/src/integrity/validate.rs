//! End-of-game validation: cross-checks the values a game produced against
//! each other and against NES rules, and summarizes recognition quality.
//!
//! The per-frame plausibility filter already rejects impossible *steps*;
//! this report judges the *whole game* once it is over, so a host can decide
//! whether a result is safe to publish unattended. `valid == false` means at
//! least one error-severity issue: the numbers should be reviewed.

use serde::Serialize;

use crate::enums::GameState;
use crate::output::{GameStats, OutputFrame};
use crate::state::plausibility::{expected_level, infer_start_level};

use super::IntegrityConfig;
use super::cheat::CheatDetector;

/// Longest single frame gap counted toward lost signal (a stalled host
/// reports its outages through [`GameValidator::add_signal_gap`]).
const MAX_FRAME_GAP_S: f64 = 5.0;
/// Scores at or above this may be capped by the game; reconciliation is skipped.
const SCORE_CAP: i64 = 999_999;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ValidationIssue {
    pub code: String,
    pub severity: Severity,
    pub detail: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ValidationMetrics {
    pub frames: u64,
    pub ingame_frames: u64,
    pub mean_confidence: Option<f64>,
    pub low_confidence_ratio: f64,
    pub signal_lost_s: f64,
    pub plausibility_rejects: u32,
    pub plausibility_corrections: u32,
    pub start_score: Option<i64>,
    pub start_lines: Option<i64>,
    pub start_level: Option<i64>,
    pub clear_points: i64,
    pub cheat_points: i64,
    pub unexplained_points: i64,
    pub score_anomalies: u32,
    pub level_offset_steps: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ValidationReport {
    pub valid: bool,
    pub issues: Vec<ValidationIssue>,
    pub metrics: ValidationMetrics,
}

/// Accumulates one game's output frames; see the module docs.
pub struct GameValidator {
    cfg: IntegrityConfig,
    frames: u64,
    ingame_frames: u64,
    confidence_sum: f64,
    low_confidence: u64,
    signal_lost_s: f64,
    last_ts: Option<f64>,
    rejects: u32,
    corrections: u32,
    start: (Option<i64>, Option<i64>, Option<i64>),
    end: (Option<i64>, Option<i64>, Option<i64>),
    out_of_range: Option<String>,
    extra: Vec<ValidationIssue>,
}

impl GameValidator {
    pub fn new(cfg: IntegrityConfig) -> Self {
        Self {
            cfg,
            frames: 0,
            ingame_frames: 0,
            confidence_sum: 0.0,
            low_confidence: 0,
            signal_lost_s: 0.0,
            last_ts: None,
            rejects: 0,
            corrections: 0,
            start: (None, None, None),
            end: (None, None, None),
            out_of_range: None,
            extra: Vec::new(),
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new(self.cfg.clone());
    }

    /// Feed one output frame of the game.
    pub fn push(&mut self, frame: &OutputFrame) {
        self.frames += 1;
        if let Some(prev) = self.last_ts {
            let dt = (frame.ts - prev).clamp(0.0, MAX_FRAME_GAP_S);
            if matches!(frame.game_state, GameState::NoSignal | GameState::Unknown) {
                self.signal_lost_s += dt;
            }
        }
        self.last_ts = Some(frame.ts);

        for event in &frame.events {
            if !matches!(event.field.as_str(), "score" | "lines" | "level") {
                continue;
            }
            match event.severity.as_str() {
                "reject" => self.rejects += 1,
                "correct" => self.corrections += 1,
                _ => {}
            }
        }

        if frame.game_state != GameState::InGame {
            return;
        }
        self.ingame_frames += 1;
        let conf = frame.confidence.overall;
        self.confidence_sum += conf;
        if conf < self.cfg.low_confidence {
            self.low_confidence += 1;
        }

        let f = &frame.fields;
        if self.start.0.is_none() {
            self.start.0 = f.score;
        }
        if self.start.1.is_none() {
            self.start.1 = f.lines;
        }
        if self.start.2.is_none() {
            self.start.2 = f.level;
        }
        self.end.0 = f.score.or(self.end.0);
        self.end.1 = f.lines.or(self.end.1);
        self.end.2 = f.level.or(self.end.2);

        if self.out_of_range.is_none() {
            if let Some(level) = f.level.filter(|&l| !(0..=255).contains(&l)) {
                self.out_of_range = Some(format!("level {level}"));
            } else if let Some(lines) = f.lines.filter(|&l| !(0..=9_999).contains(&l)) {
                self.out_of_range = Some(format!("lines {lines}"));
            } else if let Some(score) = f.score.filter(|&s| !(0..=16_777_215).contains(&s)) {
                self.out_of_range = Some(format!("score {score}"));
            }
        }
    }

    /// Record a host-side capture outage (no frames at all) during the game.
    pub fn add_signal_gap(&mut self, seconds: f64) {
        if seconds > 0.0 {
            self.signal_lost_s += seconds;
        }
    }

    /// Record a host-side issue (e.g. an engine restart mid-game).
    pub fn add_issue(&mut self, code: &str, severity: Severity, detail: impl Into<String>) {
        self.extra.push(ValidationIssue {
            code: code.to_string(),
            severity,
            detail: detail.into(),
        });
    }

    /// Build the report. `detector` must have seen the same frames (and
    /// been `finish`ed); `stats` is the game's last in-game stats block.
    pub fn report(&self, detector: &CheatDetector, stats: &GameStats) -> ValidationReport {
        let mut issues = self.extra.clone();
        let mut issue = |code: &str, severity: Severity, detail: String| {
            issues.push(ValidationIssue {
                code: code.to_string(),
                severity,
                detail,
            })
        };

        let (start_score, start_lines, start_level) = self.start;
        let (end_score, end_lines, end_level) = self.end;

        if self.ingame_frames == 0 {
            issue(
                "no_gameplay",
                Severity::Error,
                "no in-game frames were recognized".into(),
            );
        }

        if start_score.is_some_and(|s| s > 0) || start_lines.is_some_and(|l| l > 0) {
            issue(
                "partial_game",
                Severity::Error,
                format!(
                    "capture joined a running game (score {}, lines {})",
                    start_score.unwrap_or(0),
                    start_lines.unwrap_or(0)
                ),
            );
        }

        if let (Some(a), Some(b)) = (start_lines, end_lines) {
            let c = &stats.clears;
            let counted = i64::from(c.single + 2 * c.double + 3 * c.triple + 4 * c.tetris);
            let played = b - a;
            if (counted - played).abs() > self.cfg.lines_tolerance {
                issue(
                    "lines_clears_mismatch",
                    Severity::Error,
                    format!("LINES advanced by {played}, counted clears add up to {counted}"),
                );
            }
        }

        if let (Some(level0), Some(lines0), Some(lines), Some(level)) =
            (start_level, start_lines, end_lines, end_level)
        {
            let expected = expected_level(infer_start_level(level0, lines0), lines);
            if level != expected {
                issue(
                    "level_mismatch",
                    Severity::Warning,
                    format!("level {level} after {lines} lines, expected {expected}"),
                );
            }
        }

        if let (Some(a), Some(b)) = (start_score, end_score)
            && b < SCORE_CAP
        {
            let unexplained = b - a - detector.clear_points() - detector.cheat_points();
            let budget = stats.pieces.max(0) * self.cfg.softdrop_per_piece;
            let tol = self.cfg.reconcile_tolerance;
            if unexplained < -tol || unexplained > budget + tol {
                issue(
                    "score_reconcile",
                    Severity::Error,
                    format!(
                        "score {b} is {unexplained:+} off the recognized clears \
                         ({} points) and cheats ({} points)",
                        detector.clear_points(),
                        detector.cheat_points()
                    ),
                );
            }
        }

        if detector.anomalies() > 0 {
            issue(
                "score_unexplained",
                Severity::Warning,
                format!(
                    "{} score gain(s) matched no scoring event ({} points)",
                    detector.anomalies(),
                    detector.unexplained_points()
                ),
            );
        }

        if detector.level_offset_steps() > 0 {
            issue(
                "level_score_mismatch",
                Severity::Warning,
                format!(
                    "{} scoring step(s) only fit with the level off by one                      (LEVEL probably misread)",
                    detector.level_offset_steps()
                ),
            );
        }

        let low_ratio = if self.ingame_frames > 0 {
            self.low_confidence as f64 / self.ingame_frames as f64
        } else {
            0.0
        };
        if low_ratio >= self.cfg.low_confidence_error_ratio {
            issue(
                "low_confidence",
                Severity::Error,
                format!(
                    "{:.0}% of in-game frames had low confidence",
                    low_ratio * 100.0
                ),
            );
        } else if low_ratio >= self.cfg.low_confidence_warn_ratio {
            issue(
                "low_confidence",
                Severity::Warning,
                format!(
                    "{:.0}% of in-game frames had low confidence",
                    low_ratio * 100.0
                ),
            );
        }

        if self.signal_lost_s >= self.cfg.signal_lost_error_s {
            issue(
                "signal_lost",
                Severity::Error,
                format!("signal lost for {:.1}s during the game", self.signal_lost_s),
            );
        } else if self.signal_lost_s >= self.cfg.signal_lost_warn_s {
            issue(
                "signal_lost",
                Severity::Warning,
                format!("signal lost for {:.1}s during the game", self.signal_lost_s),
            );
        }

        if self.rejects >= self.cfg.plausibility_warn_count {
            issue(
                "plausibility_corrections",
                Severity::Warning,
                format!(
                    "{} rejected and {} corrected readings",
                    self.rejects, self.corrections
                ),
            );
        }

        if let Some(what) = &self.out_of_range {
            issue(
                "value_range",
                Severity::Error,
                format!("impossible value: {what}"),
            );
        }

        let valid = !issues.iter().any(|i| i.severity == Severity::Error);
        ValidationReport {
            valid,
            issues,
            metrics: ValidationMetrics {
                frames: self.frames,
                ingame_frames: self.ingame_frames,
                mean_confidence: (self.ingame_frames > 0)
                    .then(|| self.confidence_sum / self.ingame_frames as f64),
                low_confidence_ratio: low_ratio,
                signal_lost_s: self.signal_lost_s,
                plausibility_rejects: self.rejects,
                plausibility_corrections: self.corrections,
                start_score,
                start_lines,
                start_level,
                clear_points: detector.clear_points(),
                cheat_points: detector.cheat_points(),
                unexplained_points: detector.unexplained_points(),
                score_anomalies: detector.anomalies(),
                level_offset_steps: detector.level_offset_steps(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enums::Region;
    use crate::output::{Confidence, Fields, LineClears};

    fn frame(seq: i64, state: GameState, score: i64, lines: i64, level: i64) -> OutputFrame {
        OutputFrame {
            schema_version: 4,
            seq,
            ts: seq as f64 / 60.0,
            region: Region::Ntsc,
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

    struct Game {
        det: CheatDetector,
        val: GameValidator,
        seq: i64,
    }

    impl Game {
        fn new() -> Self {
            let cfg = IntegrityConfig::default();
            Self {
                det: CheatDetector::new(cfg.clone()),
                val: GameValidator::new(cfg),
                seq: 0,
            }
        }

        fn feed(&mut self, state: GameState, score: i64, lines: i64, level: i64, n: u32) {
            for _ in 0..n {
                let f = frame(self.seq, state, score, lines, level);
                self.det.push(&f);
                self.val.push(&f);
                self.seq += 1;
            }
        }

        fn report(&mut self, clears: LineClears, pieces: i64) -> ValidationReport {
            self.det.finish();
            let stats = GameStats {
                clears,
                pieces,
                ..Default::default()
            };
            self.val.report(&self.det, &stats)
        }
    }

    fn codes(r: &ValidationReport) -> Vec<&str> {
        r.issues.iter().map(|i| i.code.as_str()).collect()
    }

    fn tetrises(n: u32) -> LineClears {
        LineClears {
            tetris: n,
            ..Default::default()
        }
    }

    #[test]
    fn clean_game_is_valid() {
        let mut g = Game::new();
        g.feed(GameState::InGame, 0, 0, 18, 60);
        g.feed(GameState::InGame, 22_800, 4, 18, 60);
        g.feed(GameState::InGame, 45_600, 8, 18, 60);
        g.feed(GameState::GameOver, 45_600, 8, 18, 30);
        let r = g.report(tetrises(2), 20);
        assert!(r.valid, "{:?}", r.issues);
        assert!(r.issues.is_empty(), "{:?}", r.issues);
        assert_eq!(r.metrics.clear_points, 45_600);
    }

    #[test]
    fn cheated_game_stays_valid_and_reconciles() {
        let mut g = Game::new();
        g.feed(GameState::InGame, 0, 0, 18, 60);
        g.feed(GameState::InGame, 10_000, 0, 18, 200);
        let r = g.report(LineClears::default(), 5);
        assert_eq!(g.det.cheated(), 1);
        assert!(r.valid, "{:?}", r.issues);
        assert_eq!(r.metrics.cheat_points, 10_000);
    }

    #[test]
    fn partial_game_is_an_error() {
        let mut g = Game::new();
        g.feed(GameState::InGame, 50_000, 30, 18, 60);
        let r = g.report(LineClears::default(), 0);
        assert!(!r.valid);
        assert!(codes(&r).contains(&"partial_game"));
    }

    #[test]
    fn lines_clears_mismatch_is_an_error() {
        let mut g = Game::new();
        g.feed(GameState::InGame, 0, 0, 18, 60);
        g.feed(GameState::InGame, 22_800, 4, 18, 60);
        g.feed(GameState::InGame, 45_600, 8, 18, 60);
        let r = g.report(tetrises(1), 20);
        assert!(!r.valid);
        assert!(codes(&r).contains(&"lines_clears_mismatch"));
    }

    #[test]
    fn level_mismatch_is_a_warning() {
        let mut g = Game::new();
        g.feed(GameState::InGame, 0, 0, 0, 60);
        g.feed(GameState::InGame, 1_200, 4, 5, 60);
        let r = g.report(tetrises(1), 10);
        assert!(codes(&r).contains(&"level_mismatch"));
        assert!(
            r.issues
                .iter()
                .all(|i| i.code != "level_mismatch" || i.severity == Severity::Warning)
        );
    }

    #[test]
    fn lost_signal_is_reported() {
        let mut g = Game::new();
        g.feed(GameState::InGame, 0, 0, 18, 60);
        g.feed(GameState::NoSignal, 0, 0, 18, 180); // 3 s
        g.feed(GameState::InGame, 0, 0, 18, 60);
        g.val.add_signal_gap(10.0);
        let r = g.report(LineClears::default(), 5);
        assert!(!r.valid);
        let lost = r.issues.iter().find(|i| i.code == "signal_lost").unwrap();
        assert_eq!(lost.severity, Severity::Error);
        assert!(r.metrics.signal_lost_s > 12.9);
    }

    #[test]
    fn unexplained_score_fails_reconciliation() {
        let mut g = Game::new();
        g.feed(GameState::InGame, 0, 0, 18, 60);
        g.feed(GameState::InGame, 5_000, 0, 18, 60);
        let r = g.report(LineClears::default(), 5);
        assert!(!r.valid);
        assert!(codes(&r).contains(&"score_reconcile"));
        assert!(codes(&r).contains(&"score_unexplained"));
    }

    #[test]
    fn host_issue_is_included() {
        let mut g = Game::new();
        g.feed(GameState::InGame, 0, 0, 18, 60);
        g.val
            .add_issue("engine_restart", Severity::Error, "recovered from a panic");
        let r = g.report(LineClears::default(), 1);
        assert!(!r.valid);
        assert_eq!(codes(&r), vec!["engine_restart"]);
    }
}

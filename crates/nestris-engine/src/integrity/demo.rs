//! Attract-mode demo detection.
//!
//! Left on the title screen, NES Tetris plays a demo game: the board,
//! SCORE and LINES look exactly like a real game, so a host would record it
//! as one. The demo is deterministic (fixed RNG seed, recorded inputs), so
//! every run shows the same sequence of SCORE/LINES values after each piece
//! and the same NEXT pieces. A [`Fingerprint`] stores that sequence; the
//! [`DemoDetector`] compares a running game against it.
//!
//! Two independent signals must agree before a game counts as a demo:
//!
//! 1. **How the game was entered** ([`EntryTracker`]): a real game always
//!    starts from the type/level select menus; the demo starts straight from
//!    the title screen. A game entered through the menus is real at once.
//! 2. **The fingerprint**: every confirmed SCORE/LINES step, LEVEL and the
//!    NEXT piece must match a known demo. One stable contradiction makes the
//!    game real.
//!
//! The detector fails safe: unknown entry, no fingerprints, unreadable data
//! or running past the demo's end all end up [`DemoVerdict::Real`]. A real
//! game is never discarded on a guess.
//!
//! Like the other integrity checks this is a pure consumer of
//! [`OutputFrame`]s and does not touch the verified output path.

use serde::{Deserialize, Serialize};

use crate::enums::{GameState, Piece};
use crate::output::OutputFrame;

/// The built-in fingerprints (`assets/demo/fingerprints.json`).
const BUILTIN: &str = include_str!("../../assets/demo/fingerprints.json");

/// Tuning for [`DemoDetector`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DemoConfig {
    /// Master switch; `false` treats every game as real.
    pub enabled: bool,
    /// Matched steps for a game entered from the title screen.
    pub min_match_steps: u32,
    /// Matched steps when the entry is unknown (capture joined mid-game).
    pub min_match_steps_unknown_entry: u32,
    /// In-game frames a SCORE/LINES/LEVEL value must hold to count.
    pub confirm_frames: u32,
    /// In-game frames a NEXT piece must hold to count.
    pub next_confirm_frames: u32,
    /// Fingerprint steps a confirmed value may skip (missed OCR updates).
    pub max_skip: usize,
    /// In-game frames without a verdict before the game counts as real.
    pub max_pending_frames: u64,
    /// Keep the recording of a skipped demo for diagnosis (hosts that
    /// record; the station keeps a few in `<state_dir>/demo`).
    pub keep_recordings: bool,
}

impl Default for DemoConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_match_steps: 6,
            min_match_steps_unknown_entry: 10,
            confirm_frames: 4,
            next_confirm_frames: 30,
            max_skip: 2,
            max_pending_frames: 60 * 60,
            keep_recordings: false,
        }
    }
}

/// One SCORE/LINES value the demo shows after a piece locks.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DemoStep {
    pub score: i64,
    pub lines: i64,
    pub level: i64,
    /// The NEXT piece shown longest while this value was on screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<Piece>,
}

/// A known demo game.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Fingerprint {
    pub id: String,
    /// Free text: which ROM / region this demo comes from.
    #[serde(default)]
    pub rom: String,
    pub steps: Vec<DemoStep>,
}

/// Parse a fingerprint file: one object or an array of them.
pub fn parse_fingerprints(json: &str) -> Result<Vec<Fingerprint>, serde_json::Error> {
    let value: serde_json::Value = serde_json::from_str(json)?;
    if value.is_array() {
        serde_json::from_value(value)
    } else {
        Ok(vec![serde_json::from_value(value)?])
    }
}

/// The fingerprints compiled into the engine.
pub fn builtin_fingerprints() -> Vec<Fingerprint> {
    parse_fingerprints(BUILTIN).expect("built-in demo fingerprints parse")
}

/// How the current game was entered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Entry {
    /// Through the type or level select menu: a real game.
    Menu,
    /// Straight from the title screen: a demo candidate.
    Title,
    /// No menus seen (capture joined mid-game, came back from game over…).
    Unknown,
}

/// Type/level select frames needed to count as "entered through the menu".
const SELECT_FRAMES: u32 = 2;
/// In-game frames after which the next menu screen starts a new stretch.
const INGAME_RESET_FRAMES: u32 = 30;
/// Frames without a picture after which the entry is unknown again.
const LOST_RESET_FRAMES: u32 = 300;
/// Consecutive title frames that void earlier menu frames (backing out of
/// the level select to the title, then waiting for the demo).
const TITLE_RESET_FRAMES: u32 = 10;

/// Remembers which menu screens were shown before a game started.
///
/// Feed it **every** output frame, menus included.
#[derive(Clone, Debug, Default)]
pub struct EntryTracker {
    select_frames: u32,
    title_frames: u32,
    title_run: u32,
    ingame_run: u32,
    lost_run: u32,
}

impl EntryTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, state: GameState) {
        match state {
            GameState::InGame | GameState::Paused => {
                self.ingame_run = self.ingame_run.saturating_add(1);
                self.lost_run = 0;
            }
            GameState::NoSignal | GameState::Unknown => {
                self.lost_run = self.lost_run.saturating_add(1);
                if self.lost_run >= LOST_RESET_FRAMES {
                    self.clear();
                }
            }
            menu => {
                if self.ingame_run >= INGAME_RESET_FRAMES {
                    self.clear();
                }
                self.ingame_run = 0;
                self.lost_run = 0;
                if menu == GameState::Title {
                    self.title_frames = self.title_frames.saturating_add(1);
                    self.title_run = self.title_run.saturating_add(1);
                    if self.title_run >= TITLE_RESET_FRAMES {
                        self.select_frames = 0;
                    }
                } else {
                    self.title_run = 0;
                    if matches!(menu, GameState::TypeSelect | GameState::LevelSelect) {
                        self.select_frames = self.select_frames.saturating_add(1);
                    }
                }
            }
        }
    }

    /// The entry of a game starting now.
    pub fn entry(&self) -> Entry {
        if self.select_frames >= SELECT_FRAMES {
            Entry::Menu
        } else if self.title_frames > 0 {
            Entry::Title
        } else {
            Entry::Unknown
        }
    }

    fn clear(&mut self) {
        self.select_frames = 0;
        self.title_frames = 0;
        self.title_run = 0;
        self.ingame_run = 0;
        self.lost_run = 0;
    }
}

/// Why a game counts as real.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RealReason {
    Disabled,
    NoFingerprints,
    EnteredFromMenu,
    Mismatch,
    Timeout,
    EndedUnclear,
}

impl RealReason {
    pub fn as_str(self) -> &'static str {
        match self {
            RealReason::Disabled => "disabled",
            RealReason::NoFingerprints => "no_fingerprints",
            RealReason::EnteredFromMenu => "entered_from_menu",
            RealReason::Mismatch => "mismatch",
            RealReason::Timeout => "timeout",
            RealReason::EndedUnclear => "ended_unclear",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DemoVerdict {
    /// Still comparing.
    Pending,
    Real(RealReason),
    /// A demo game: the fingerprint id that matched.
    Demo(String),
}

/// Stable-value filter: a value counts once it held for `need` frames.
#[derive(Clone, Debug)]
struct Stable<T: Copy + PartialEq> {
    candidate: Option<T>,
    count: u32,
    value: Option<T>,
}

impl<T: Copy + PartialEq> Stable<T> {
    fn new() -> Self {
        Self {
            candidate: None,
            count: 0,
            value: None,
        }
    }

    /// Feed a reading; returns the value when it just became stable.
    fn push(&mut self, v: T, need: u32) -> Option<T> {
        if self.candidate == Some(v) {
            self.count = self.count.saturating_add(1);
        } else {
            self.candidate = Some(v);
            self.count = 1;
        }
        if self.count >= need && self.value != Some(v) {
            self.value = Some(v);
            return Some(v);
        }
        None
    }
}

/// Progress against one fingerprint.
#[derive(Clone, Debug)]
struct Track {
    fp: usize,
    pos: Option<usize>,
    matched: u32,
    next_confirmed: u32,
    failed: bool,
}

/// Compares one game against the known demos.
pub struct DemoDetector {
    cfg: DemoConfig,
    fingerprints: Vec<Fingerprint>,
    entry: Entry,
    tracks: Vec<Track>,
    verdict: DemoVerdict,
    pair: Stable<(i64, i64)>,
    level: Stable<i64>,
    next: Stable<Piece>,
    ingame_frames: u64,
}

impl DemoDetector {
    pub fn new(cfg: DemoConfig, fingerprints: Vec<Fingerprint>, entry: Entry) -> Self {
        let verdict = if !cfg.enabled {
            DemoVerdict::Real(RealReason::Disabled)
        } else if entry == Entry::Menu {
            DemoVerdict::Real(RealReason::EnteredFromMenu)
        } else if fingerprints.iter().all(|f| f.steps.is_empty()) {
            DemoVerdict::Real(RealReason::NoFingerprints)
        } else {
            DemoVerdict::Pending
        };
        let tracks = (0..fingerprints.len())
            .filter(|&i| !fingerprints[i].steps.is_empty())
            .map(|fp| Track {
                fp,
                pos: None,
                matched: 0,
                next_confirmed: 0,
                failed: false,
            })
            .collect();
        Self {
            cfg,
            fingerprints,
            entry,
            tracks,
            verdict,
            pair: Stable::new(),
            level: Stable::new(),
            next: Stable::new(),
            ingame_frames: 0,
        }
    }

    pub fn entry(&self) -> Entry {
        self.entry
    }

    pub fn verdict(&self) -> &DemoVerdict {
        &self.verdict
    }

    /// `true` while the game may still be a demo, or is one.
    pub fn is_candidate(&self) -> bool {
        !matches!(self.verdict, DemoVerdict::Real(_))
    }

    /// Steps matched by the best fingerprint so far.
    pub fn matched_steps(&self) -> u32 {
        self.tracks
            .iter()
            .filter(|t| !t.failed)
            .map(|t| t.matched)
            .max()
            .unwrap_or(0)
    }

    /// Feed one output frame of the game.
    pub fn push(&mut self, out: &OutputFrame) -> &DemoVerdict {
        if self.verdict != DemoVerdict::Pending || out.game_state != GameState::InGame {
            return &self.verdict;
        }
        self.ingame_frames += 1;
        let f = &out.fields;
        if let (Some(score), Some(lines)) = (f.score, f.lines)
            && let Some(pair) = self.pair.push((score, lines), self.cfg.confirm_frames)
        {
            self.on_pair(pair);
        }
        if let Some(level) = f.level
            && let Some(level) = self.level.push(level, self.cfg.confirm_frames)
        {
            self.on_level(level);
        }
        if let Some(next) = f.next_piece.filter(|p| p.is_piece())
            && let Some(next) = self.next.push(next, self.cfg.next_confirm_frames)
        {
            self.on_next(next);
        }
        self.decide();
        &self.verdict
    }

    /// The game ended; settle a still pending verdict.
    pub fn finish(&mut self) -> &DemoVerdict {
        if self.verdict == DemoVerdict::Pending {
            // An aborted demo: from the title screen, nothing contradicted
            // the fingerprint, and it showed at least one exact step beyond
            // the start or a matching NEXT piece.
            let best = self
                .tracks
                .iter()
                .filter(|t| !t.failed && (t.matched >= 2 || t.next_confirmed >= 1))
                .max_by_key(|t| (t.matched, t.next_confirmed));
            self.verdict = match (self.entry, best) {
                (Entry::Title, Some(t)) => DemoVerdict::Demo(self.fingerprints[t.fp].id.clone()),
                _ => DemoVerdict::Real(RealReason::EndedUnclear),
            };
        }
        &self.verdict
    }

    fn on_pair(&mut self, (score, lines): (i64, i64)) {
        let max_skip = self.cfg.max_skip;
        for t in self.tracks.iter_mut().filter(|t| !t.failed) {
            let steps = &self.fingerprints[t.fp].steps;
            let is = |i: usize| steps[i].score == score && steps[i].lines == lines;
            match t.pos {
                None => match (0..steps.len()).find(|&i| is(i)) {
                    Some(i) => {
                        t.pos = Some(i);
                        t.matched = 1;
                    }
                    None => t.failed = true,
                },
                Some(pos) if is(pos) => {}
                Some(pos) => {
                    let ahead =
                        (pos + 1..=(pos + 1 + max_skip).min(steps.len() - 1)).find(|&i| is(i));
                    if let Some(i) = ahead {
                        t.pos = Some(i);
                        t.matched += 1;
                        t.next_confirmed = 0;
                    } else if pos + 1 < steps.len()
                        && is_mixed(&steps[pos], &steps[pos + 1], score, lines)
                    {
                        // SCORE and LINES settle on different frames.
                    } else {
                        t.failed = true;
                    }
                }
            }
        }
    }

    fn on_level(&mut self, level: i64) {
        for t in self.tracks.iter_mut().filter(|t| !t.failed) {
            let steps = &self.fingerprints[t.fp].steps;
            let ok = match t.pos {
                // Before alignment only the start level can be checked.
                None => steps.iter().any(|s| s.level == level),
                Some(pos) => near(steps, pos).any(|s| s.level == level),
            };
            if !ok {
                t.failed = true;
            }
        }
    }

    fn on_next(&mut self, next: Piece) {
        for t in self.tracks.iter_mut().filter(|t| !t.failed) {
            let steps = &self.fingerprints[t.fp].steps;
            let Some(pos) = t.pos else { continue };
            let known: Vec<Piece> = near(steps, pos).filter_map(|s| s.next).collect();
            if known.is_empty() {
                continue;
            }
            if known.contains(&next) {
                if steps[pos].next == Some(next) {
                    t.next_confirmed += 1;
                }
            } else {
                t.failed = true;
            }
        }
    }

    fn decide(&mut self) {
        if self.tracks.iter().all(|t| t.failed) {
            self.verdict = DemoVerdict::Real(RealReason::Mismatch);
            return;
        }
        let need = match self.entry {
            Entry::Title => self.cfg.min_match_steps,
            _ => self.cfg.min_match_steps_unknown_entry,
        };
        if let Some(t) = self.tracks.iter().find(|t| !t.failed && t.matched >= need) {
            self.verdict = DemoVerdict::Demo(self.fingerprints[t.fp].id.clone());
            return;
        }
        if self.ingame_frames >= self.cfg.max_pending_frames {
            self.verdict = DemoVerdict::Real(RealReason::Timeout);
        }
    }
}

/// The steps around `pos` (previous, current, next).
fn near(steps: &[DemoStep], pos: usize) -> impl Iterator<Item = &DemoStep> {
    steps[pos.saturating_sub(1)..(pos + 2).min(steps.len())].iter()
}

/// A SCORE/LINES pair halfway between two steps.
fn is_mixed(a: &DemoStep, b: &DemoStep, score: i64, lines: i64) -> bool {
    (score == a.score || score == b.score) && (lines == a.lines || lines == b.lines)
}

/// Builds the step sequence of one recording, for [`consensus`].
#[derive(Default)]
pub struct StepRecorder {
    cfg: DemoConfig,
    pair: Option<Stable<(i64, i64)>>,
    level: Option<i64>,
    steps: Vec<DemoStep>,
    /// NEXT readings during the current step.
    next_counts: Vec<(Piece, u32)>,
}

impl StepRecorder {
    pub fn new() -> Self {
        Self {
            cfg: DemoConfig::default(),
            pair: Some(Stable::new()),
            ..Default::default()
        }
    }

    pub fn push(&mut self, out: &OutputFrame) {
        if out.game_state != GameState::InGame {
            return;
        }
        let f = &out.fields;
        if let Some(level) = f.level {
            self.level = Some(level);
        }
        if let (Some(score), Some(lines)) = (f.score, f.lines) {
            let pair = self.pair.get_or_insert_with(Stable::new);
            if let Some((score, lines)) = pair.push((score, lines), self.cfg.confirm_frames) {
                self.close_step();
                self.steps.push(DemoStep {
                    score,
                    lines,
                    level: self.level.unwrap_or(0),
                    next: None,
                });
            }
        }
        if let Some(next) = f.next_piece.filter(|p| p.is_piece()) {
            match self.next_counts.iter_mut().find(|(p, _)| *p == next) {
                Some((_, n)) => *n += 1,
                None => self.next_counts.push((next, 1)),
            }
        }
    }

    fn close_step(&mut self) {
        if let Some(last) = self.steps.last_mut() {
            last.next = self
                .next_counts
                .iter()
                .max_by_key(|(_, n)| *n)
                .map(|(p, _)| *p);
        }
        self.next_counts.clear();
    }

    pub fn finish(mut self) -> Vec<DemoStep> {
        self.close_step();
        // Drop the transient SCORE/LINES pairs that only exist because the
        // two counters settle on different frames.
        let steps = std::mem::take(&mut self.steps);
        let mut out: Vec<DemoStep> = Vec::new();
        for (i, s) in steps.iter().enumerate() {
            if i > 0
                && i + 1 < steps.len()
                && is_mixed(&steps[i - 1], &steps[i + 1], s.score, s.lines)
                && (s.score, s.lines) != (steps[i - 1].score, steps[i - 1].lines)
                && (s.score, s.lines) != (steps[i + 1].score, steps[i + 1].lines)
            {
                continue;
            }
            out.push(s.clone());
        }
        out
    }
}

/// Combine the step sequences of several recordings of the same demo into
/// a fingerprint. The longest recording is the base; a step is kept when at
/// least half of the recordings show it in the same order (OCR gaps make
/// single recordings miss a step). The NEXT piece is kept where at least
/// two thirds of the recordings that read it agree. Recordings that show
/// steps the others don't are not the same demo and are rejected.
pub fn consensus(id: &str, rom: &str, runs: &[Vec<DemoStep>]) -> Result<Fingerprint, String> {
    if runs.len() < 2 {
        return Err("need at least two recordings of the demo".into());
    }
    let key = |s: &DemoStep| (s.score, s.lines, s.level);
    let base = runs.iter().max_by_key(|r| r.len()).expect("non-empty");
    let mut support = vec![0usize; base.len()];
    let mut nexts: Vec<Vec<Piece>> = vec![Vec::new(); base.len()];
    for (ri, run) in runs.iter().enumerate() {
        // Walk the run along the base; steps of the run the base lacks are
        // disagreements.
        let mut bi = 0;
        let mut extra = 0;
        for step in run {
            match (bi..base.len()).find(|&i| key(&base[i]) == key(step)) {
                Some(i) => {
                    support[i] += 1;
                    if let Some(n) = step.next {
                        nexts[i].push(n);
                    }
                    bi = i + 1;
                }
                None => extra += 1,
            }
        }
        if extra > 2 {
            return Err(format!(
                "recording {} shows {extra} steps the longest recording lacks; is it the same demo?",
                ri + 1
            ));
        }
    }
    let need = runs.len().div_ceil(2).max(2);
    let steps: Vec<DemoStep> = base
        .iter()
        .enumerate()
        .filter(|(i, _)| support[*i] >= need)
        .map(|(i, s)| {
            let reads = &nexts[i];
            let next = reads
                .iter()
                .copied()
                .max_by_key(|p| reads.iter().filter(|q| *q == p).count())
                .filter(|p| reads.iter().filter(|q| *q == p).count() * 3 >= reads.len() * 2);
            DemoStep {
                score: s.score,
                lines: s.lines,
                level: s.level,
                next,
            }
        })
        .collect();
    if steps.len() < 10 {
        return Err(format!(
            "the recordings agree on only {} steps; are they all the same demo?",
            steps.len()
        ));
    }
    Ok(Fingerprint {
        id: id.into(),
        rom: rom.into(),
        steps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enums::Region;
    use crate::output::{Confidence, Fields, GameStats, SCHEMA_VERSION};

    fn frame(
        state: GameState,
        score: Option<i64>,
        lines: Option<i64>,
        level: i64,
        next: Option<Piece>,
    ) -> OutputFrame {
        OutputFrame {
            schema_version: SCHEMA_VERSION,
            seq: 0,
            ts: 0.0,
            region: Region::Ntsc,
            game_state: state,
            fields: Fields {
                score,
                lines,
                level: Some(level),
                next_piece: next,
                ..Default::default()
            },
            stats: GameStats::default(),
            stats_ext: None,
            confidence: Confidence::default(),
            events: Vec::new(),
        }
    }

    fn fp() -> Fingerprint {
        let vals = [
            (0, 0, Piece::J),
            (11, 0, Piece::T),
            (23, 0, Piece::S),
            (35, 0, Piece::Z),
            (45, 0, Piece::J),
            (96, 1, Piece::T),
            (100, 1, Piece::S),
            (111, 1, Piece::Z),
            (120, 1, Piece::J),
            (171, 2, Piece::S),
            (182, 2, Piece::Z),
            (190, 2, Piece::L),
        ];
        Fingerprint {
            id: "test".into(),
            rom: String::new(),
            steps: vals
                .iter()
                .map(|&(score, lines, next)| DemoStep {
                    score,
                    lines,
                    level: 0,
                    next: Some(next),
                })
                .collect(),
        }
    }

    /// Feed `steps` for 60 frames each.
    fn play(d: &mut DemoDetector, steps: &[(i64, i64, Piece)]) -> DemoVerdict {
        for &(s, l, n) in steps {
            for _ in 0..60 {
                d.push(&frame(GameState::InGame, Some(s), Some(l), 0, Some(n)));
            }
        }
        d.verdict().clone()
    }

    fn demo_steps(n: usize) -> Vec<(i64, i64, Piece)> {
        fp().steps
            .iter()
            .take(n)
            .map(|s| (s.score, s.lines, s.next.unwrap()))
            .collect()
    }

    #[test]
    fn builtin_parses() {
        let fps = builtin_fingerprints();
        assert!(!fps.is_empty());
        assert!(fps.iter().all(|f| f.steps.len() >= 10));
    }

    #[test]
    fn demo_from_title_is_detected() {
        let mut d = DemoDetector::new(DemoConfig::default(), vec![fp()], Entry::Title);
        assert_eq!(
            play(&mut d, &demo_steps(6)),
            DemoVerdict::Demo("test".into())
        );
    }

    #[test]
    fn menu_entry_is_real_at_once() {
        let d = DemoDetector::new(DemoConfig::default(), vec![fp()], Entry::Menu);
        assert_eq!(d.verdict(), &DemoVerdict::Real(RealReason::EnteredFromMenu));
    }

    #[test]
    fn disabled_is_real() {
        let cfg = DemoConfig {
            enabled: false,
            ..Default::default()
        };
        let d = DemoDetector::new(cfg, vec![fp()], Entry::Title);
        assert_eq!(d.verdict(), &DemoVerdict::Real(RealReason::Disabled));
    }

    #[test]
    fn different_score_is_real() {
        let mut d = DemoDetector::new(DemoConfig::default(), vec![fp()], Entry::Title);
        let mut steps = demo_steps(3);
        steps.push((40, 0, Piece::Z));
        assert_eq!(
            play(&mut d, &steps),
            DemoVerdict::Real(RealReason::Mismatch)
        );
    }

    #[test]
    fn different_next_piece_is_real() {
        let mut d = DemoDetector::new(DemoConfig::default(), vec![fp()], Entry::Title);
        let steps = [(0, 0, Piece::I)];
        assert_eq!(
            play(&mut d, &steps),
            DemoVerdict::Real(RealReason::Mismatch)
        );
    }

    #[test]
    fn wrong_start_level_is_real() {
        let mut d = DemoDetector::new(DemoConfig::default(), vec![fp()], Entry::Title);
        for _ in 0..60 {
            d.push(&frame(
                GameState::InGame,
                Some(0),
                Some(0),
                18,
                Some(Piece::J),
            ));
        }
        assert_eq!(d.verdict(), &DemoVerdict::Real(RealReason::Mismatch));
    }

    #[test]
    fn ocr_gaps_and_glitches_are_tolerated() {
        let mut d = DemoDetector::new(DemoConfig::default(), vec![fp()], Entry::Title);
        for (s, l, n) in demo_steps(7) {
            for i in 0..60 {
                let f = match i % 10 {
                    // Unreadable frames.
                    0 => frame(GameState::InGame, None, None, 0, None),
                    // A one-frame misread.
                    5 => frame(GameState::InGame, Some(s + 3), Some(l), 0, Some(Piece::O)),
                    _ => frame(GameState::InGame, Some(s), Some(l), 0, Some(n)),
                };
                d.push(&f);
            }
        }
        assert_eq!(d.verdict(), &DemoVerdict::Demo("test".into()));
    }

    #[test]
    fn score_and_lines_settling_apart_is_tolerated() {
        let mut d = DemoDetector::new(DemoConfig::default(), vec![fp()], Entry::Title);
        play(&mut d, &demo_steps(5));
        // LINES updates before SCORE: (45, 1) for a few frames.
        for _ in 0..8 {
            d.push(&frame(
                GameState::InGame,
                Some(45),
                Some(1),
                0,
                Some(Piece::J),
            ));
        }
        let rest: Vec<_> = demo_steps(7).into_iter().skip(5).collect();
        assert_eq!(play(&mut d, &rest), DemoVerdict::Demo("test".into()));
    }

    #[test]
    fn joining_mid_demo_needs_more_steps() {
        let mut d = DemoDetector::new(DemoConfig::default(), vec![fp()], Entry::Unknown);
        let steps: Vec<_> = demo_steps(12).into_iter().skip(2).collect();
        assert_eq!(play(&mut d, &steps[..9]), DemoVerdict::Pending);
        assert_eq!(play(&mut d, &steps[9..]), DemoVerdict::Demo("test".into()));
    }

    #[test]
    fn running_past_the_demo_is_real() {
        let cfg = DemoConfig {
            min_match_steps: 100,
            ..Default::default()
        };
        let mut d = DemoDetector::new(cfg, vec![fp()], Entry::Title);
        let mut steps = demo_steps(12);
        steps.push((5000, 20, Piece::I));
        assert_eq!(
            play(&mut d, &steps),
            DemoVerdict::Real(RealReason::Mismatch)
        );
    }

    #[test]
    fn aborted_demo_from_title_is_a_demo() {
        let mut d = DemoDetector::new(DemoConfig::default(), vec![fp()], Entry::Title);
        play(&mut d, &demo_steps(2));
        assert_eq!(d.finish(), &DemoVerdict::Demo("test".into()));
    }

    #[test]
    fn aborted_unknown_entry_is_real() {
        let mut d = DemoDetector::new(DemoConfig::default(), vec![fp()], Entry::Unknown);
        play(&mut d, &demo_steps(3));
        assert_eq!(d.finish(), &DemoVerdict::Real(RealReason::EndedUnclear));
    }

    #[test]
    fn aborted_without_evidence_is_real() {
        let mut d = DemoDetector::new(DemoConfig::default(), vec![fp()], Entry::Title);
        // Nothing readable at all.
        for _ in 0..100 {
            d.push(&frame(GameState::InGame, None, None, 0, None));
        }
        assert_eq!(d.finish(), &DemoVerdict::Real(RealReason::EndedUnclear));
    }

    #[test]
    fn entry_tracker_paths() {
        let mut e = EntryTracker::new();
        assert_eq!(e.entry(), Entry::Unknown);
        for _ in 0..50 {
            e.push(GameState::Title);
        }
        assert_eq!(e.entry(), Entry::Title);
        for _ in 0..10 {
            e.push(GameState::TypeSelect);
        }
        for _ in 0..10 {
            e.push(GameState::LevelSelect);
        }
        assert_eq!(e.entry(), Entry::Menu);
        // The game runs; the next menu stretch starts fresh.
        for _ in 0..600 {
            e.push(GameState::InGame);
        }
        assert_eq!(
            e.entry(),
            Entry::Menu,
            "still the entry of the running game"
        );
        for _ in 0..30 {
            e.push(GameState::Title);
        }
        assert_eq!(e.entry(), Entry::Title);
        // A one-frame in-game blip inside the menus doesn't reset.
        e.push(GameState::InGame);
        e.push(GameState::LevelSelect);
        e.push(GameState::LevelSelect);
        assert_eq!(e.entry(), Entry::Menu);
        // Backing out to the title and waiting: the demo is not "from the menu".
        for _ in 0..TITLE_RESET_FRAMES {
            e.push(GameState::Title);
        }
        assert_eq!(e.entry(), Entry::Title);
        // A title blip inside the menus doesn't void them.
        for _ in 0..5 {
            e.push(GameState::TypeSelect);
        }
        e.push(GameState::Title);
        e.push(GameState::LevelSelect);
        assert_eq!(e.entry(), Entry::Menu);
        // A long outage forgets everything.
        for _ in 0..LOST_RESET_FRAMES {
            e.push(GameState::NoSignal);
        }
        assert_eq!(e.entry(), Entry::Unknown);
    }

    #[test]
    fn recorder_and_consensus() {
        let mut runs = Vec::new();
        for cut in [12, 11] {
            let mut r = StepRecorder::new();
            for (s, l, n) in demo_steps(cut) {
                for _ in 0..60 {
                    r.push(&frame(GameState::InGame, Some(s), Some(l), 0, Some(n)));
                }
            }
            runs.push(r.finish());
        }
        let f = consensus("x", "", &runs).unwrap();
        assert_eq!(f.steps.len(), 11);
        assert_eq!(f.steps[5].score, 96);
        assert_eq!(f.steps[5].next, Some(Piece::T));
    }
}

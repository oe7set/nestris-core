//! Per-frame processing: raw frame in, structured [`OutputFrame`] out
//! (port of `processor.py`, sans event-log I/O — events ride the output).

use nestris_vision::Image;

use crate::config::EngineConfig;
use crate::enums::GameState;
use crate::frame::Frame;
use crate::geometry_cal::calibration::Rectifier;
use crate::geometry_cal::lock::{CalibrationLock, LockState};
use crate::layout::{LayoutTable, get_layout};
use crate::output::{
    Confidence, Event, Fields, GameStats, OutputFrame, SCHEMA_VERSION, StatisticsMap,
};
use crate::palette::to_luma;
use crate::recognition::clear_anim::ClearAnimationDetector;
use crate::recognition::current_piece::CurrentPieceReader;
use crate::recognition::digits::{BaseMode, DigitReader, ScoreBaseLatch};
use crate::recognition::next_piece::NextPieceReader;
use crate::recognition::playfield::{ColorTuning, Grid, PlayfieldReader};
use crate::recognition::stabilizer::PlayfieldStabilizer;
use crate::recognition::statistics::StatisticsReader;
use crate::state::fusion::{FusedState, FusionEngine, RawReading};
use crate::state::plausibility::{PlausibilityFilter, PlausibilityInput, PlausibilityResult};
use crate::state::screen::ScreenClassifier;
use crate::stats::StatsEngine;

fn is_gameplay(state: GameState) -> bool {
    matches!(state, GameState::InGame | GameState::Paused)
}

fn is_lock_hold_state(state: Option<GameState>) -> bool {
    matches!(
        state,
        Some(GameState::Paused)
            | Some(GameState::GameOver)
            | Some(GameState::Title)
            | Some(GameState::TypeSelect)
            | Some(GameState::LevelSelect)
            | Some(GameState::HighscoreEntry)
            | Some(GameState::NoSignal)
    )
}

/// Stateful processor turning frames into fused, enriched output frames.
pub struct FrameProcessor {
    config: EngineConfig,
    layout: &'static LayoutTable,
    classifier: ScreenClassifier,
    digits: DigitReader,
    score_latch: Option<ScoreBaseLatch>,
    statistics: StatisticsReader,
    current: CurrentPieceReader,
    clear_anim: ClearAnimationDetector,
    stabilizer: PlayfieldStabilizer,
    fusion: FusionEngine,
    stats: StatsEngine,
    plausibility: Option<PlausibilityFilter>,
    lock: CalibrationLock,
    last_lock_state: LockState,
    last_level: Option<i64>,
    last_lines: Option<i64>,
    /// Palette hint for the frames right after a level-crossing clear
    /// (value, frames remaining) — `recognition.level_hint_on_clear`.
    level_hint: Option<(i64, u32)>,
    last_state: Option<GameState>,
    last_next: Option<crate::enums::Piece>,
    pending_occupancy: Option<Grid<bool>>,
    prev_occupancy: Option<Grid<bool>>,
    frame_events: Vec<Event>,
    last_canonical: Option<Image>,
    /// The last usable geometry, kept after the lock drops so menus and the
    /// blanked pause stay recognizable (signature screen mode; capture
    /// geometry rarely changes between games).
    remembered: Option<Rectifier>,
    // Presentation-only live state.
    live_playfield: Option<Vec<Vec<u8>>>,
    live_anim: bool,
    live_flash: bool,
}

impl FrameProcessor {
    pub fn new(config: EngineConfig) -> Self {
        let base = match config.recognition.score_base.as_str() {
            "dec" => BaseMode::Dec,
            "hex" => BaseMode::Hex,
            _ => BaseMode::Auto,
        };
        let score_latch =
            if base == BaseMode::Auto && config.recognition.score_base_latch_frames > 0 {
                Some(ScoreBaseLatch::new(
                    config.recognition.score_base_latch_frames,
                ))
            } else {
                None
            };
        let plausibility = if config.plausibility.enabled {
            Some(PlausibilityFilter::new(config.plausibility.clone()))
        } else {
            None
        };
        Self {
            layout: get_layout(),
            classifier: ScreenClassifier::with_config(config.screen.clone()),
            digits: DigitReader::new(base),
            score_latch,
            statistics: StatisticsReader::new(),
            current: CurrentPieceReader::new(),
            clear_anim: ClearAnimationDetector::new(),
            stabilizer: PlayfieldStabilizer::new_with_voting(config.recognition.color_voting),
            fusion: FusionEngine::new(config.fusion.clone()),
            stats: StatsEngine::new(),
            plausibility,
            lock: CalibrationLock::new_with_tracking(
                config.calibration.clone(),
                config.tracking.clone(),
            ),
            last_lock_state: LockState::Unlocked,
            last_level: None,
            last_lines: None,
            level_hint: None,
            last_state: None,
            last_next: None,
            pending_occupancy: None,
            prev_occupancy: None,
            frame_events: Vec::new(),
            last_canonical: None,
            remembered: None,
            live_playfield: None,
            live_anim: false,
            live_flash: false,
            config,
        }
    }

    pub fn lock_state(&self) -> LockState {
        self.last_lock_state
    }

    pub fn lock(&mut self) -> &mut CalibrationLock {
        &mut self.lock
    }

    pub fn last_canonical(&self) -> Option<&Image> {
        self.last_canonical.as_ref()
    }

    pub fn live_playfield(&self) -> Option<&Vec<Vec<u8>>> {
        self.live_playfield.as_ref()
    }

    pub fn clear_animation_active(&self) -> bool {
        self.live_anim
    }

    pub fn clear_flash_active(&self) -> bool {
        self.live_flash
    }

    /// Extended dashboard statistics for the current game, regardless of the
    /// `output.extended_stats` wire toggle.
    pub fn extended_stats(&self) -> crate::stats_ext::ExtendedStats {
        self.stats.extended()
    }

    /// Re-synchronize temporal state after a discontinuity (keeps the lock).
    pub fn reset_tracking(&mut self) {
        self.fusion.reset_game();
        self.stats.reset();
        self.classifier.reset();
        self.current.reset();
        self.clear_anim.reset();
        self.stabilizer.reset();
        if let Some(latch) = &mut self.score_latch {
            latch.reset();
        }
        self.last_level = None;
        self.last_lines = None;
        self.level_hint = None;
        self.last_state = None;
        self.last_next = None;
        self.prev_occupancy = None;
        self.pending_occupancy = None;
        if let Some(p) = &mut self.plausibility {
            p.reset();
        }
    }

    pub fn reset_lock(&mut self) {
        self.lock.reset();
        self.remembered = None;
        self.last_lock_state = LockState::Unlocked;
    }

    /// Process one captured frame into an [`OutputFrame`].
    pub fn process(&mut self, frame: &Frame) -> OutputFrame {
        if frame.discontinuity {
            self.reset_tracking();
        }
        self.live_playfield = None;
        self.live_anim = false;
        self.live_flash = false;

        let hold_drift =
            self.config.calibration.menu_drift_hold && is_lock_hold_state(self.last_state);
        let status = self.lock.prepare(&frame.image, hold_drift);
        self.last_lock_state = status.state;

        let mut canon = self
            .lock
            .rectifier()
            .map(|rectifier| rectifier.rectify(&frame.image));
        let mut canon_gray = canon.as_ref().map(to_luma);

        // Phase 2 of the lock update reuses this frame's rectified luma.
        let status = self.lock.confirm(canon_gray.as_ref(), hold_drift);
        self.last_lock_state = status.state;
        let geometry_conf = status.geometry_confidence;
        if !status.usable {
            canon = None;
            canon_gray = None;
        }
        self.last_canonical = canon.clone();

        let aligned_gray = if self.config.screen.legacy() {
            None
        } else if canon.is_some() {
            self.remember_geometry();
            None
        } else {
            self.remembered
                .as_ref()
                .map(|r| to_luma(&r.rectify(&frame.image)))
        };
        let classification = self.classifier.classify_frame(
            &frame.image,
            canon.as_ref(),
            self.last_state,
            canon_gray.as_ref(),
            aligned_gray.as_ref(),
        );

        let reading = match (&canon, &canon_gray) {
            (Some(canon), Some(gray)) => self.read_fields(
                frame,
                canon,
                gray,
                classification.state,
                classification.confidence,
                geometry_conf,
            ),
            _ => RawReading {
                seq: frame.seq,
                ts: frame.ts,
                state: classification.state,
                state_confidence: classification.confidence,
                geometry_confidence: 0.0,
                ..Default::default()
            },
        };

        let mut fused = self.fusion.update(&reading);
        if fused.is_new_game {
            self.on_new_game(frame);
        }
        self.track_spawn(&fused);
        let plausible = self.apply_plausibility(frame, &fused);
        if let Some(p) = &plausible {
            fused.score = p.score;
            fused.lines = p.lines;
            fused.level = p.level;
        }
        if fused.level.is_some() {
            self.last_level = fused.level;
            // A caught-up fused level supersedes the transition hint.
            if let Some((hint, _)) = self.level_hint
                && fused.level == Some(hint)
            {
                self.level_hint = None;
            }
        }
        if fused.lines.is_some() {
            self.last_lines = fused.lines;
        }
        if let Some((_, frames_left)) = &mut self.level_hint {
            *frames_left = frames_left.saturating_sub(1);
            if *frames_left == 0 {
                self.level_hint = None;
            }
        }
        self.last_state = Some(fused.state);
        let stats = self.stats.update(&fused);
        self.build_output(frame, fused, stats, plausible)
    }

    /// Keep a copy of the current lock geometry (rebuilt only when it moved).
    fn remember_geometry(&mut self) {
        let Some(current) = self.lock.rectifier() else {
            return;
        };
        if self
            .remembered
            .as_ref()
            .is_some_and(|r| r.matrix() == current.matrix())
        {
            return;
        }
        self.remembered = Rectifier::new(*current.matrix(), current.undistort().cloned());
    }

    fn track_spawn(&mut self, fused: &FusedState) {
        if fused.state == GameState::InGame {
            let spawned = fused.next_piece.is_some()
                && self.last_next.is_some()
                && fused.next_piece != self.last_next;
            if spawned && let Some(prev) = &self.prev_occupancy {
                self.current.commit(prev);
            }
            self.last_next = fused.next_piece;
        }
        if let Some(pending) = self.pending_occupancy.take() {
            self.prev_occupancy = Some(pending);
        }
    }

    fn on_new_game(&mut self, frame: &Frame) {
        if let Some(p) = &mut self.plausibility {
            p.reset();
        }
        self.stats.reset();
        self.current.reset();
        self.clear_anim.reset();
        self.stabilizer.reset();
        self.last_level = None;
        self.last_lines = None;
        self.level_hint = None;
        self.last_next = None;
        self.prev_occupancy = None;
        self.pending_occupancy = None;
        self.frame_events.push(Event {
            ts: frame.ts,
            field: "game".into(),
            reason: "new_game".into(),
            severity: "info".into(),
            old: None,
            new: None,
            confidence: None,
        });
    }

    fn apply_plausibility(
        &mut self,
        frame: &Frame,
        fused: &FusedState,
    ) -> Option<PlausibilityResult> {
        let filter = self.plausibility.as_mut()?;
        Some(filter.filter(&PlausibilityInput {
            seq: frame.seq,
            ts: frame.ts,
            state: fused.state,
            score: fused.score,
            lines: fused.lines,
            level: fused.level,
            score_conf: Some(fused.confidence.score),
            lines_conf: Some(fused.confidence.lines),
            level_conf: Some(fused.confidence.level),
        }))
    }

    fn read_fields(
        &mut self,
        frame: &Frame,
        canon: &Image,
        gray: &Image,
        state: GameState,
        state_conf: f64,
        geometry_conf: f64,
    ) -> RawReading {
        let mut reading = RawReading {
            seq: frame.seq,
            ts: frame.ts,
            state,
            state_confidence: state_conf,
            geometry_confidence: geometry_conf,
            ..Default::default()
        };
        if !is_gameplay(state) {
            return reading;
        }
        let layout = self.layout;

        let score = match &mut self.score_latch {
            Some(latch) => {
                self.digits
                    .read_field_latched(latch, gray, &layout.score, layout.score_digits)
            }
            None => self
                .digits
                .read_field(gray, &layout.score, layout.score_digits, None),
        };
        let lines = self.digits.read_field(
            gray,
            &layout.lines,
            layout.lines_digits,
            Some(BaseMode::Dec),
        );
        let level = self.digits.read_field(
            gray,
            &layout.level,
            layout.level_digits,
            Some(BaseMode::Dec),
        );
        let next_piece = NextPieceReader::read(canon, &layout.next_box);
        // The level-transition hint colors the first post-clear frames with
        // the next level's palette until fusion catches up.
        let palette_level = self
            .level_hint
            .filter(|_| self.config.recognition.level_hint_on_clear)
            .map(|(level, _)| level)
            .or(self.last_level);
        let mut playfield = PlayfieldReader::read_with(
            canon,
            gray,
            layout,
            palette_level,
            ColorTuning::from_config(&self.config.recognition),
        );
        if self.config.recognition.playfield_stabilizer {
            playfield = self.stabilizer.update(&playfield);
        }

        let robust = self.config.recognition.clear_prediction;
        let mut animating = false;
        let mut curtain = false;
        if self.config.recognition.freeze_on_clear_animation {
            let frame_luma_mean =
                gray.data.iter().map(|&v| v as f64).sum::<f64>() / gray.data.len() as f64;
            // Robust mode: never *start* an animation while paused (a menu
            // flash is not a clear) or while the game-over curtain sweeps.
            let suppress_entry = robust
                && (self.last_state == Some(GameState::Paused) || self.clear_anim.curtain_active());
            let was_animating = self.clear_anim.animating();
            animating =
                self.clear_anim
                    .update(&playfield.occupancy, frame_luma_mean, suppress_entry);
            self.live_flash = self.clear_anim.flash_active();
            curtain = robust && self.clear_anim.curtain_active();

            // Validate the finished animation against its prediction.
            if robust
                && was_animating
                && !animating
                && let Some(prediction) = self.clear_anim.take_finished_prediction()
            {
                self.validate_clear_prediction(frame, &prediction, &playfield.occupancy);
            }
        }
        self.live_playfield = Some(playfield.grid_as_rows());
        self.live_anim = animating;
        // The curtain withholds the playfield exactly like a clear animation:
        // fusion holds the last real stack instead of ingesting the sweep.
        let animating = animating || curtain;

        reading.score = score.value;
        reading.score_confidence = score.confidence as f64;
        reading.lines = lines.value;
        reading.lines_confidence = lines.confidence as f64;
        reading.level = level.value;
        reading.level_confidence = level.confidence as f64;
        reading.next_piece = next_piece.piece;
        reading.next_confidence = next_piece.confidence as f64;
        if !animating {
            reading.playfield = Some(playfield.grid_as_rows());
            reading.playfield_confidence = playfield.confidence as f64;
        }

        let every_n = self.config.recognition.statistics_every_n.max(1) as i64;
        if self.config.recognition.read_statistics && frame.seq % every_n == 0 {
            let stats_reading = self.statistics.read(gray, layout);
            reading.statistics = Some(stats_reading.counts.map(|(p, v)| (p, v)).to_vec());
            reading.statistics_confidence = stats_reading.confidence as f64;
        }

        if self.config.recognition.read_current_piece && !animating {
            let cp = self.current.read(&playfield.occupancy);
            reading.current_piece = cp.piece;
            reading.current_piece_confidence = cp.confidence as f64;
            if let (Some(row), Some(col)) = (cp.row, cp.col) {
                reading.current_piece_pos = Some((row as u32, col as u32));
            }
            if !cp.cells.is_empty() {
                // A falling piece is a single color by construction: force
                // its cells to their majority id (fixes single-cell color
                // misreads on levels with close accents).
                if self.config.recognition.piece_color_uniform && cp.cells.len() >= 3 {
                    uniform_piece_color(reading.playfield.as_mut(), &cp.cells);
                }
                reading.current_piece_cells = Some(
                    cp.cells
                        .iter()
                        .map(|&(r, c)| (r as u32, c as u32))
                        .collect(),
                );
            }
        }

        if !animating {
            self.pending_occupancy = Some(playfield.occupancy);
        }
        reading
    }

    /// Compare a finished clear animation's predicted board against the
    /// first fresh reading; emit an observability event and arm the level
    /// hint when the clear crosses a x10 line boundary.
    fn validate_clear_prediction(
        &mut self,
        frame: &Frame,
        prediction: &crate::recognition::clear_anim::ClearPrediction,
        observed: &Grid<bool>,
    ) {
        const MAX_MISMATCHES: usize = 3;
        const HINT_FRAMES: u32 = 30;
        let mismatches =
            crate::recognition::clear_anim::prediction_mismatches(&prediction.occupancy, observed);
        let ok = mismatches <= MAX_MISMATCHES;
        self.frame_events.push(Event {
            ts: frame.ts,
            field: "playfield".into(),
            reason: if ok {
                "clear_prediction_ok".into()
            } else {
                "clear_prediction_mismatch".into()
            },
            severity: if ok { "info".into() } else { "warn".into() },
            old: Some(serde_json::Value::from(prediction.cleared_rows)),
            new: Some(serde_json::Value::from(mismatches)),
            confidence: None,
        });

        if self.config.recognition.level_hint_on_clear
            && let (Some(lines), Some(level)) = (self.last_lines, self.last_level)
        {
            let after = lines + prediction.cleared_rows as i64;
            if after / 10 > lines / 10 {
                self.level_hint = Some((level + 1, HINT_FRAMES));
            }
        }
    }

    fn build_output(
        &mut self,
        frame: &Frame,
        fused: FusedState,
        stats: GameStats,
        plausible: Option<PlausibilityResult>,
    ) -> OutputFrame {
        let mut events: Vec<Event> = plausible
            .map(|p| {
                p.events
                    .into_iter()
                    .map(|ev| Event {
                        ts: ev.ts,
                        field: ev.field.into(),
                        reason: ev.reason.into(),
                        severity: ev.severity.into(),
                        old: ev.old.map(serde_json::Value::from),
                        new: ev.new.map(serde_json::Value::from),
                        confidence: ev.confidence,
                    })
                    .collect()
            })
            .unwrap_or_default();
        for sev in self.stats.drain_events() {
            events.push(Event {
                ts: sev.ts,
                field: sev.field.into(),
                reason: sev.reason.into(),
                severity: sev.severity.into(),
                old: sev.old.map(serde_json::Value::from),
                new: sev.new.map(serde_json::Value::from),
                confidence: None,
            });
        }
        events.append(&mut self.frame_events);

        let stats_ext = self
            .config
            .output
            .extended_stats
            .then(|| self.stats.extended());

        OutputFrame {
            schema_version: SCHEMA_VERSION,
            seq: frame.seq,
            ts: frame.ts,
            region: self.config.region(),
            game_state: fused.state,
            fields: Fields {
                score: fused.score,
                lines: fused.lines,
                level: fused.level,
                next_piece: fused.next_piece,
                current_piece: fused.current_piece,
                current_piece_pos: fused.current_piece_pos,
                current_piece_cells: fused.current_piece_cells,
                playfield: fused.playfield,
                statistics: fused.statistics.map(|s| StatisticsMap(s.0)),
            },
            stats,
            stats_ext,
            confidence: Confidence {
                score: fused.confidence.score,
                lines: fused.confidence.lines,
                level: fused.confidence.level,
                next_piece: fused.confidence.next_piece,
                playfield: fused.confidence.playfield,
                statistics: fused.confidence.statistics,
                current_piece: fused.confidence.current_piece,
                geometry: fused.confidence.geometry,
                overall: fused.confidence.overall,
            },
            events,
        }
    }
}

/// Force the falling piece's cells to their majority color id in the raw
/// reading (a tetromino is one color by construction; single-cell misreads
/// on close-accent levels get corrected before fusion sees them).
fn uniform_piece_color(playfield: Option<&mut Vec<Vec<u8>>>, cells: &[(usize, usize)]) {
    let Some(grid) = playfield else { return };
    let mut counts = [0usize; 4];
    for &(r, c) in cells {
        if let Some(&id) = grid.get(r).and_then(|row| row.get(c))
            && id > 0
        {
            counts[id as usize] += 1;
        }
    }
    let (majority, votes) = counts
        .iter()
        .enumerate()
        .skip(1)
        .max_by_key(|&(_, &n)| n)
        .map(|(id, &n)| (id as u8, n))
        .unwrap_or((0, 0));
    // Require a real majority (not a 2/2 split of a 4-cell piece).
    if votes * 2 <= cells.len() {
        return;
    }
    for &(r, c) in cells {
        if let Some(cell) = grid.get_mut(r).and_then(|row| row.get_mut(c))
            && *cell > 0
        {
            *cell = majority;
        }
    }
}

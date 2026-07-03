//! NGF replay: turn a recorded game back into full engine output frames,
//! statistics included.
//!
//! A recording stores the *fields* (score, lines, level, pieces, board);
//! everything derived — clears breakdown, TRT, droughts, PPS, extended
//! dashboard stats — is recomputed by running the recorded fields through
//! the same [`StatsEngine`] the live pipeline uses. Replay output is
//! deterministic: the same file always produces the same frames.

use nestris_engine::enums::{GameState, Region};
use nestris_engine::output::{
    Confidence, Event, Fields, OutputFrame, SCHEMA_VERSION, StatisticsMap,
};
use nestris_engine::state::fusion::{FusedConfidence, FusedState};
use nestris_engine::stats::StatsEngine;

use crate::codec::{NGF_PIECES, NgfFrame};

/// A fully decoded NGF recording.
#[derive(Clone, Debug)]
pub struct ReplayFile {
    pub frames: Vec<NgfFrame>,
}

impl ReplayFile {
    /// Decode from raw or gzipped NGF bytes (content-sniffed).
    #[cfg(feature = "std")]
    pub fn from_bytes(bytes: &[u8]) -> std::io::Result<Self> {
        let frames = crate::io::decode_all(bytes)?;
        if frames.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "NGF file contains no frames",
            ));
        }
        Ok(Self { frames })
    }

    #[cfg(feature = "std")]
    pub fn load(path: &std::path::Path) -> std::io::Result<Self> {
        Self::from_bytes(&std::fs::read(path)?)
    }

    pub fn duration_ms(&self) -> u32 {
        self.frames.last().map(|f| f.ctime_ms).unwrap_or(0)
    }

    /// Index of the frame at or before `ctime_ms` (for time-based seeks).
    pub fn index_at_ms(&self, ctime_ms: u32) -> usize {
        match self.frames.binary_search_by_key(&ctime_ms, |f| f.ctime_ms) {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) => i - 1,
        }
    }
}

/// Replays a [`ReplayFile`] through a [`StatsEngine`], producing schema-v4
/// [`OutputFrame`]s for arbitrary frame indices.
///
/// Random access works by keeping stats in lockstep with a cursor: forward
/// steps are incremental; a backward seek re-runs the (pure integer) stats
/// from the start of the game, which is fast enough for interactive
/// scrubbing.
pub struct ReplayEngine {
    file: ReplayFile,
    stats: StatsEngine,
    /// Frames [0, cursor) have been fed to `stats`.
    cursor: usize,
    /// Attach the extended dashboard block to produced frames.
    pub extended_stats: bool,
}

impl ReplayEngine {
    pub fn new(file: ReplayFile) -> Self {
        Self {
            file,
            stats: StatsEngine::new(),
            cursor: 0,
            extended_stats: true,
        }
    }

    pub fn file(&self) -> &ReplayFile {
        &self.file
    }

    pub fn frame_count(&self) -> usize {
        self.file.frames.len()
    }

    pub fn duration_ms(&self) -> u32 {
        self.file.duration_ms()
    }

    pub fn ctime_ms_at(&self, index: usize) -> u32 {
        self.file.frames[index.min(self.file.frames.len() - 1)].ctime_ms
    }

    pub fn index_at_ms(&self, ctime_ms: u32) -> usize {
        self.file.index_at_ms(ctime_ms)
    }

    /// The output frame at `index` (clamped), with statistics reflecting the
    /// game up to and including that frame.
    pub fn output_at(&mut self, index: usize) -> OutputFrame {
        let index = index.min(self.file.frames.len() - 1);
        if index + 1 < self.cursor {
            // Backward seek: replay the pure stats pipeline from the start.
            self.stats.reset();
            self.cursor = 0;
        }
        while self.cursor <= index {
            let fused = fused_state(&self.file.frames[self.cursor], self.cursor);
            self.stats.update(&fused);
            // Drop events of skipped-over frames so a long forward seek
            // doesn't dump a backlog into the target frame.
            if self.cursor != index {
                let _ = self.stats.drain_events();
            }
            self.cursor += 1;
        }
        self.build_output(index)
    }

    fn build_output(&mut self, index: usize) -> OutputFrame {
        let frame = &self.file.frames[index];
        let fused = fused_state(frame, index);
        let stats = self.stats.update(&fused); // idempotent snapshot source
        let events = self
            .stats
            .drain_events()
            .into_iter()
            .map(|sev| Event {
                ts: sev.ts,
                field: sev.field.into(),
                reason: sev.reason.into(),
                severity: sev.severity.into(),
                old: sev.old.map(serde_json::Value::from),
                new: sev.new.map(serde_json::Value::from),
                confidence: None,
            })
            .collect();

        OutputFrame {
            schema_version: SCHEMA_VERSION,
            seq: index as i64,
            ts: fused.ts,
            region: Region::Ntsc,
            game_state: GameState::InGame,
            fields: Fields {
                score: fused.score,
                lines: fused.lines,
                level: fused.level,
                next_piece: fused.next_piece,
                current_piece: fused.current_piece,
                current_piece_pos: None,
                current_piece_cells: None,
                playfield: fused.playfield,
                statistics: fused.statistics,
            },
            stats,
            stats_ext: self.extended_stats.then(|| self.stats.extended()),
            confidence: Confidence {
                score: 1.0,
                lines: 1.0,
                level: 1.0,
                next_piece: 1.0,
                playfield: 1.0,
                statistics: 1.0,
                current_piece: 1.0,
                geometry: 1.0,
                overall: 1.0,
            },
            events,
        }
    }
}

/// Map a recorded frame onto the fused-state shape the stats engine expects.
/// Recorded values are trusted (confidence 1.0).
fn fused_state(frame: &NgfFrame, index: usize) -> FusedState {
    let playfield: Vec<Vec<u8>> = (0..20)
        .map(|r| frame.field[r * 10..(r + 1) * 10].to_vec())
        .collect();
    let statistics = frame.counts.iter().any(Option::is_some).then(|| {
        StatisticsMap(
            NGF_PIECES
                .iter()
                .zip(frame.counts.iter())
                .map(|(&p, &c)| (p, c.map(i64::from)))
                .collect(),
        )
    });

    FusedState {
        seq: index as i64,
        ts: f64::from(frame.ctime_ms) / 1000.0,
        state: GameState::InGame,
        score: frame.score.map(i64::from),
        lines: frame.lines.map(i64::from),
        level: frame.level.map(i64::from),
        next_piece: frame.preview,
        playfield: Some(playfield),
        statistics,
        current_piece: frame.cur_piece,
        current_piece_pos: None,
        current_piece_cells: None,
        confidence: FusedConfidence {
            score: 1.0,
            lines: 1.0,
            level: 1.0,
            next_piece: 1.0,
            playfield: 1.0,
            statistics: 1.0,
            current_piece: 1.0,
            geometry: 1.0,
            overall: 1.0,
        },
        is_new_game: index == 0,
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use crate::codec::encode_v3;
    use nestris_engine::enums::Piece;

    /// A tiny synthetic game: 120 frames, one single clear at frame 60,
    /// a tetris at frame 100.
    fn synthetic_game() -> Vec<u8> {
        let mut buf = Vec::new();
        let mut score: u32 = 0;
        let mut lines: u16 = 0;
        for i in 0u32..120 {
            if i == 60 {
                lines += 1;
                score += 40 * 6; // single at level 5
            }
            if i == 100 {
                lines += 4;
                score += 1200 * 6; // tetris at level 5
            }
            let frame = NgfFrame {
                gameid: 1,
                ctime_ms: i * 17,
                lines: Some(lines),
                level: Some(5),
                score: Some(score),
                preview: Some(if i % 8 < 4 { Piece::T } else { Piece::I }),
                cur_piece: Some(Piece::J),
                ..NgfFrame::default()
            };
            encode_v3(&frame, &mut buf);
        }
        buf
    }

    #[test]
    fn replay_recomputes_stats() {
        let file = ReplayFile::from_bytes(&synthetic_game()).unwrap();
        let mut replay = ReplayEngine::new(file);
        assert_eq!(replay.frame_count(), 120);

        let last = replay.output_at(usize::MAX);
        assert_eq!(last.fields.score, Some(40 * 6 + 1200 * 6));
        assert_eq!(last.fields.lines, Some(5));
        assert_eq!(last.stats.clears.single, 1);
        assert_eq!(last.stats.clears.tetris, 1);
        assert_eq!(last.stats.burn, 1);
        let ext = last.stats_ext.expect("extended stats attached");
        assert_eq!(ext.points.tetrises, 7200);
        assert_eq!(ext.points.singles, 240);
    }

    #[test]
    fn backward_seek_matches_forward_pass() {
        let file = ReplayFile::from_bytes(&synthetic_game()).unwrap();
        let mut replay = ReplayEngine::new(file);

        let fwd = replay.output_at(70);
        let fwd_json = fwd.to_json();
        let _ = replay.output_at(119);
        let back = replay.output_at(70); // backward seek re-runs stats
        assert_eq!(back.to_json(), fwd_json);
    }

    #[test]
    fn replay_is_deterministic() {
        let bytes = synthetic_game();
        let mut a = ReplayEngine::new(ReplayFile::from_bytes(&bytes).unwrap());
        let mut b = ReplayEngine::new(ReplayFile::from_bytes(&bytes).unwrap());
        for i in (0..120).step_by(7) {
            assert_eq!(a.output_at(i).to_json(), b.output_at(i).to_json());
        }
    }

    #[test]
    fn time_seek_maps_to_frame_index() {
        let file = ReplayFile::from_bytes(&synthetic_game()).unwrap();
        assert_eq!(file.index_at_ms(0), 0);
        assert_eq!(file.index_at_ms(17), 1);
        assert_eq!(file.index_at_ms(25), 1); // between frames -> earlier
        assert_eq!(file.index_at_ms(u32::MAX), 119);
    }

    #[test]
    fn empty_file_rejected() {
        assert!(ReplayFile::from_bytes(&[]).is_err());
    }
}

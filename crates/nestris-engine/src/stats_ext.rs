//! Extended statistics for dashboards: points breakdown, efficiency, pace,
//! per-piece distribution and droughts, board-shape metrics, and small
//! bounded time series (TRT trend, height timeline).
//!
//! Everything here is derived from the same fused inputs the base
//! [`crate::stats::StatsEngine`] consumes and is deterministic. The block is
//! attached to the serialized output only when `output.extended_stats` is
//! enabled, so the default wire format stays byte-identical to the verified
//! schema. Definitions for every value live in `docs/STATS.md`.

use serde::Serialize;

use crate::enums::Piece;
use crate::output::LineClears;

/// Piece order used for all per-piece arrays (the STATISTICS rail order).
pub const PIECE_ORDER: [Piece; 7] = [
    Piece::T,
    Piece::J,
    Piece::Z,
    Piece::O,
    Piece::S,
    Piece::L,
    Piece::I,
];

/// Index of a piece in [`PIECE_ORDER`], if it is a real piece.
pub fn piece_index(piece: Piece) -> Option<usize> {
    PIECE_ORDER.iter().position(|&p| p == piece)
}

/// Time-series caps: when a series reaches its cap it is thinned by keeping
/// every second sample, preserving overall shape at half the resolution.
const TRT_TREND_CAP: usize = 512;
const HEIGHT_TIMELINE_CAP: usize = 2048;
/// Minimum spacing between height-timeline samples (seconds, ~4 Hz).
const HEIGHT_SAMPLE_MIN_GAP_S: f64 = 0.25;
/// A drought is "real" (counted, flagged) from this many I-less pieces on.
const DROUGHT_FLAG_THRESHOLD: u32 = 13;
/// NES line-clear base scores by clear size (level multiplier applied on top).
const CLEAR_BASE: [i64; 5] = [0, 40, 100, 300, 1200];
/// Pace projects the score to this line count (the pre-killscreen target).
const PACE_TARGET_LINES: i64 = 230;

/// Height-timeline flag bits.
pub const FLAG_TETRIS_READY: u8 = 1 << 0;
pub const FLAG_DOUBLE_WELL: u8 = 1 << 1;
pub const FLAG_CLEAN_SLOPE: u8 = 1 << 2;
pub const FLAG_IN_DROUGHT: u8 = 1 << 3;

/// Score contributions by source (NES scoring, level multiplier included).
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct PointsBreakdown {
    /// Everything not attributed to a line clear: soft-drop points plus any
    /// residual (e.g. clears scored while the level was still unknown).
    pub drops: i64,
    pub singles: i64,
    pub doubles: i64,
    pub triples: i64,
    pub tetrises: i64,
}

impl PointsBreakdown {
    pub fn attributed(&self) -> i64 {
        self.singles + self.doubles + self.triples + self.tetrises
    }
}

/// I-piece drought summary (current run, previous run, all-time max, and how
/// many droughts of at least 13 pieces have ended).
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct DroughtStats {
    pub current: u32,
    pub last: u32,
    pub max: u32,
    pub count: u32,
}

/// Shape metrics of the settled stack (falling piece masked out).
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct BoardMetrics {
    /// Tallest column height in rows (0..=20).
    pub max_height: u8,
    pub avg_height: f32,
    /// Empty cells with at least one filled cell above them in the column.
    pub holes: u16,
    /// At least one column is a ready tetris well (>=4 stacked rows that are
    /// full except for that single column).
    pub tetris_ready: bool,
    /// At least two well columns (>=3 rows deeper than both neighbors).
    pub double_well: bool,
    /// No holes and monotone column heights across the board.
    pub clean_slope: bool,
}

/// Per-piece spawn distribution.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct PieceDistribution {
    /// Counts in [`PIECE_ORDER`] (T, J, Z, O, S, L, I). Prefers the on-screen
    /// STATISTICS rail when it has been read; falls back to spawn counting.
    pub counts: [i64; 7],
    /// Pieces since each type last spawned, same order.
    pub drought: [u32; 7],
    /// Coefficient of variation of `counts` (stddev / mean); 0 = perfectly
    /// even distribution.
    pub deviation: f64,
}

/// The full extended block. Cheap to clone relative to frame processing; the
/// two time series are bounded (see the `*_CAP` constants).
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct ExtendedStats {
    pub points: PointsBreakdown,
    /// Level-independent clear points per line (`sum(base(size)) / lines`),
    /// 300 = tetris-only. `None` before the first clear.
    pub efficiency: Option<f64>,
    /// Projected score at line 230 assuming the current clear distribution
    /// continues (documented approximation, not NestrisChamps-exact).
    pub pace_score: Option<i64>,
    pub i_drought: DroughtStats,
    pub board: BoardMetrics,
    /// `(total_lines, tetris_rate)` sampled after every clear.
    pub trt_trend: Vec<(i64, f64)>,
    /// `(ts, max_height, flag bits)` sampled at most ~4 Hz during play.
    pub height_timeline: Vec<(f64, u8, u8)>,
    pub piece_dist: PieceDistribution,
}

/// Internal tracker owned by `StatsEngine`; hooks are called from the base
/// engine's observation points and `snapshot` assembles the public block.
#[derive(Clone, Debug, Default)]
pub struct ExtTracker {
    points: PointsBreakdown,
    /// Level-independent base points from attributed clears (for EFF).
    base_points: i64,
    i_drought_last: u32,
    i_drought_count: u32,
    spawn_counts: [i64; 7],
    rail_counts: Option<[i64; 7]>,
    piece_drought: [u32; 7],
    board: BoardMetrics,
    trt_trend: Vec<(i64, f64)>,
    height_timeline: Vec<(f64, u8, u8)>,
    last_height_sample_ts: Option<f64>,
    height_sample_gap: f64,
}

impl ExtTracker {
    pub fn new() -> Self {
        Self {
            height_sample_gap: HEIGHT_SAMPLE_MIN_GAP_S,
            ..Self::default()
        }
    }

    /// A clear of `size` lines was recorded. `level` is the fused level at
    /// clear time (multiplier `level+1`), `total`/`tetris` are the base
    /// engine's line totals *after* this clear.
    pub fn on_clear(&mut self, size: i64, level: Option<i64>, total_lines: i64, tetris_lines: i64) {
        let base = CLEAR_BASE[size.clamp(1, 4) as usize];
        self.base_points += base;
        if let Some(level) = level {
            let points = base * (level + 1);
            match size {
                1 => self.points.singles += points,
                2 => self.points.doubles += points,
                3 => self.points.triples += points,
                _ => self.points.tetrises += points,
            }
        }
        let trt = if total_lines > 0 {
            tetris_lines as f64 / total_lines as f64
        } else {
            0.0
        };
        push_capped(&mut self.trt_trend, (total_lines, trt), TRT_TREND_CAP);
    }

    /// A piece spawned (identified via the NEXT transition), with the base
    /// engine's I-drought value *before* it was reset/incremented.
    pub fn on_spawn(&mut self, spawned: Piece, drought_before: u32) {
        let Some(idx) = piece_index(spawned) else {
            return;
        };
        self.spawn_counts[idx] += 1;
        for (i, d) in self.piece_drought.iter_mut().enumerate() {
            if i == idx {
                *d = 0;
            } else {
                *d = d.saturating_add(1);
            }
        }
        if spawned == Piece::I {
            self.end_i_drought(drought_before);
        }
    }

    /// The STATISTICS rail proved an I spawn the NEXT tracking missed.
    pub fn on_rail_i_reset(&mut self, drought_before: u32) {
        self.end_i_drought(drought_before);
        self.piece_drought[6] = 0;
    }

    fn end_i_drought(&mut self, length: u32) {
        self.i_drought_last = length;
        if length >= DROUGHT_FLAG_THRESHOLD {
            self.i_drought_count += 1;
        }
    }

    /// A full, confident STATISTICS rail read (counts in [`PIECE_ORDER`]).
    pub fn on_rail(&mut self, counts: [i64; 7]) {
        self.rail_counts = Some(counts);
    }

    /// Per-frame board observation. `grid` is the fused 20x10 playfield with
    /// the falling piece already masked out; `ts` paces the timeline.
    pub fn on_board(&mut self, grid: &[Vec<u8>], ts: f64, in_drought: bool) {
        self.board = board_metrics(grid);
        let due = self
            .last_height_sample_ts
            .is_none_or(|last| ts - last >= self.height_sample_gap);
        if !due {
            return;
        }
        self.last_height_sample_ts = Some(ts);
        let mut flags = 0u8;
        if self.board.tetris_ready {
            flags |= FLAG_TETRIS_READY;
        }
        if self.board.double_well {
            flags |= FLAG_DOUBLE_WELL;
        }
        if self.board.clean_slope {
            flags |= FLAG_CLEAN_SLOPE;
        }
        if in_drought {
            flags |= FLAG_IN_DROUGHT;
        }
        let sample = (ts, self.board.max_height, flags);
        if self.height_timeline.len() >= HEIGHT_TIMELINE_CAP {
            thin(&mut self.height_timeline);
            // Halve the sampling rate too, so the series keeps covering the
            // whole game instead of re-filling immediately.
            self.height_sample_gap *= 2.0;
        }
        self.height_timeline.push(sample);
    }

    /// Assemble the public snapshot from tracker state plus the base
    /// engine's current values.
    #[allow(clippy::too_many_arguments)]
    pub fn snapshot(
        &self,
        score: i64,
        lines: Option<i64>,
        level: Option<i64>,
        clears: &LineClears,
        total_lines: i64,
        tetris_lines: i64,
        drought: u32,
    ) -> ExtendedStats {
        let mut points = self.points.clone();
        points.drops = (score - points.attributed()).max(0);

        let efficiency = if total_lines > 0 {
            Some(self.base_points as f64 / total_lines as f64)
        } else {
            None
        };

        let counts = self.rail_counts.unwrap_or(self.spawn_counts);
        let piece_dist = PieceDistribution {
            counts,
            drought: self.piece_drought,
            deviation: coefficient_of_variation(&counts),
        };

        ExtendedStats {
            points,
            efficiency,
            pace_score: project_pace(score, lines, level, clears, total_lines, tetris_lines),
            i_drought: DroughtStats {
                current: drought,
                last: self.i_drought_last,
                max: 0, // filled by the caller (base engine owns max_drought)
                count: self.i_drought_count,
            },
            board: self.board.clone(),
            trt_trend: self.trt_trend.clone(),
            height_timeline: self.height_timeline.clone(),
            piece_dist,
        }
    }
}

/// Project the final score at line 230 by extrapolating the observed clear
/// distribution with NES level progression (level +1 per 10 lines).
///
/// Documented approximation (`docs/STATS.md`): assumes the per-line clear
/// mix stays constant and ignores future soft-drop points. `None` until at
/// least one clear has been attributed and score/lines/level are all known.
pub fn project_pace(
    score: i64,
    lines: Option<i64>,
    level: Option<i64>,
    clears: &LineClears,
    total_lines: i64,
    tetris_lines: i64,
) -> Option<i64> {
    let lines = lines?;
    let level = level?;
    if total_lines <= 0 {
        return None;
    }
    if lines >= PACE_TARGET_LINES {
        return Some(score);
    }

    // Fraction of cleared lines contributed by each clear size.
    let single_lines = clears.single as i64;
    let double_lines = clears.double as i64 * 2;
    let triple_lines = clears.triple as i64 * 3;
    debug_assert_eq!(
        single_lines + double_lines + triple_lines + tetris_lines,
        total_lines
    );
    let total = total_lines as f64;
    // Level-independent base points per future line for the observed mix:
    // a clear of size s yields base(s) points across s lines.
    let base_per_line = (single_lines as f64 * (CLEAR_BASE[1] as f64)
        + double_lines as f64 * (CLEAR_BASE[2] as f64 / 2.0)
        + triple_lines as f64 * (CLEAR_BASE[3] as f64 / 3.0)
        + tetris_lines as f64 * (CLEAR_BASE[4] as f64 / 4.0))
        / total;

    // Walk future lines in 10-line chunks; NES bumps the level every 10
    // lines once past the start threshold, approximated as +1 per chunk.
    let mut projected = score as f64;
    let mut at_lines = lines;
    let mut at_level = level;
    while at_lines < PACE_TARGET_LINES {
        let chunk_end = ((at_lines / 10) + 1) * 10;
        let chunk = chunk_end.min(PACE_TARGET_LINES) - at_lines;
        projected += chunk as f64 * base_per_line * (at_level + 1) as f64;
        at_lines += chunk;
        at_level += 1;
    }
    Some(projected.round() as i64)
}

/// Column heights, holes, and shape flags for a 20x10 grid of cell ids
/// (0 = empty). Rows are top-to-bottom.
pub fn board_metrics(grid: &[Vec<u8>]) -> BoardMetrics {
    let rows = grid.len();
    let cols = grid.first().map(|r| r.len()).unwrap_or(0);
    if rows == 0 || cols == 0 {
        return BoardMetrics::default();
    }

    let mut heights = vec![0usize; cols];
    let mut holes = 0u16;
    for c in 0..cols {
        let mut top: Option<usize> = None;
        for (r, row) in grid.iter().enumerate() {
            if row[c] != 0 {
                top = Some(r);
                break;
            }
        }
        if let Some(top) = top {
            heights[c] = rows - top;
            for row in grid.iter().skip(top + 1) {
                if row[c] == 0 {
                    holes += 1;
                }
            }
        }
    }

    // Tetris-ready: some column c has >=4 consecutive rows that are full
    // except for c itself.
    let mut tetris_ready = false;
    'columns: for c in 0..cols {
        let mut run = 0;
        for row in grid.iter() {
            let full_except_c =
                row[c] == 0 && row.iter().enumerate().all(|(i, &v)| i == c || v != 0);
            run = if full_except_c { run + 1 } else { 0 };
            if run >= 4 {
                tetris_ready = true;
                break 'columns;
            }
        }
    }

    // Wells: columns at least 3 rows deeper than every neighbor.
    let well_count = (0..cols)
        .filter(|&c| {
            let h = heights[c] as i64;
            let left = c.checked_sub(1).map(|l| heights[l] as i64);
            let right = (c + 1 < cols).then(|| heights[c + 1] as i64);
            left.is_none_or(|l| h + 3 <= l) && right.is_none_or(|r| h + 3 <= r)
        })
        .count();

    let monotone =
        heights.windows(2).all(|w| w[0] <= w[1]) || heights.windows(2).all(|w| w[0] >= w[1]);

    let max_height = *heights.iter().max().unwrap_or(&0) as u8;
    let avg_height = heights.iter().sum::<usize>() as f32 / cols as f32;
    BoardMetrics {
        max_height,
        avg_height,
        holes,
        tetris_ready,
        double_well: well_count >= 2,
        clean_slope: holes == 0 && monotone,
    }
}

fn coefficient_of_variation(counts: &[i64; 7]) -> f64 {
    let total: i64 = counts.iter().sum();
    if total == 0 {
        return 0.0;
    }
    let mean = total as f64 / 7.0;
    let var = counts
        .iter()
        .map(|&c| {
            let d = c as f64 - mean;
            d * d
        })
        .sum::<f64>()
        / 7.0;
    var.sqrt() / mean
}

fn push_capped<T>(series: &mut Vec<T>, sample: T, cap: usize) {
    if series.len() >= cap {
        thin(series);
    }
    series.push(sample);
}

/// Keep every second element (index 0, 2, 4, ...), halving the series.
fn thin<T>(series: &mut Vec<T>) {
    let mut keep = 0;
    for i in 0..series.len() {
        if i % 2 == 0 {
            series.swap(keep, i);
            keep += 1;
        }
    }
    series.truncate(keep);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_grid() -> Vec<Vec<u8>> {
        vec![vec![0u8; 10]; 20]
    }

    #[test]
    fn board_metrics_empty() {
        let m = board_metrics(&empty_grid());
        assert_eq!(m.max_height, 0);
        assert_eq!(m.holes, 0);
        assert!(!m.tetris_ready);
        assert!(m.clean_slope, "flat board with no holes is a clean slope");
    }

    #[test]
    fn board_metrics_heights_and_holes() {
        let mut g = empty_grid();
        // Column 0: filled rows 16..20 (height 4) with a hole at row 18.
        for r in [16, 17, 19] {
            g[r][0] = 1;
        }
        let m = board_metrics(&g);
        assert_eq!(m.max_height, 4);
        assert_eq!(m.holes, 1);
    }

    #[test]
    fn tetris_ready_well() {
        let mut g = empty_grid();
        // Bottom 4 rows full except column 9.
        for r in 16..20 {
            for c in 0..9 {
                g[r][c] = 1;
            }
        }
        let m = board_metrics(&g);
        assert!(m.tetris_ready);
        assert!(!m.double_well);
    }

    #[test]
    fn double_well_detected() {
        let mut g = empty_grid();
        // Everything height 5 except columns 3 and 7 empty.
        for r in 15..20 {
            for c in 0..10 {
                if c != 3 && c != 7 {
                    g[r][c] = 1;
                }
            }
        }
        let m = board_metrics(&g);
        assert!(m.double_well);
    }

    #[test]
    fn clean_slope_monotone() {
        let mut g = empty_grid();
        // Staircase: column c has height c (no holes), monotone increasing.
        for (c, h) in (0..10).enumerate() {
            for r in (20 - h)..20 {
                g[r][c] = 1;
            }
        }
        let m = board_metrics(&g);
        assert!(m.clean_slope);
        assert_eq!(m.holes, 0);
        // Punch a hole: no longer clean.
        g[19][9] = 0;
        assert!(!board_metrics(&g).clean_slope);
    }

    #[test]
    fn points_breakdown_and_efficiency() {
        let mut t = ExtTracker::new();
        t.on_clear(4, Some(18), 4, 4); // tetris at level 18: 1200*19 = 22800
        t.on_clear(1, Some(18), 5, 4); // single: 40*19 = 760
        let snap = t.snapshot(
            23_760,
            Some(5),
            Some(18),
            &LineClears {
                single: 1,
                tetris: 1,
                ..Default::default()
            },
            5,
            4,
            0,
        );
        assert_eq!(snap.points.tetrises, 22_800);
        assert_eq!(snap.points.singles, 760);
        assert_eq!(snap.points.drops, 200);
        // EFF: (1200 + 40) / 5 lines = 248
        assert_eq!(snap.efficiency, Some(248.0));
    }

    #[test]
    fn drops_never_negative() {
        let mut t = ExtTracker::new();
        t.on_clear(4, Some(18), 4, 4);
        let snap = t.snapshot(100, Some(4), Some(18), &LineClears::default(), 4, 4, 0);
        assert_eq!(snap.points.drops, 0);
    }

    #[test]
    fn i_drought_last_and_count() {
        let mut t = ExtTracker::new();
        t.on_spawn(Piece::I, 15); // ends a 15-drought: counted
        assert_eq!(t.i_drought_last, 15);
        assert_eq!(t.i_drought_count, 1);
        t.on_spawn(Piece::I, 3); // short gap: not counted
        assert_eq!(t.i_drought_last, 3);
        assert_eq!(t.i_drought_count, 1);
    }

    #[test]
    fn spawn_counts_and_piece_droughts() {
        let mut t = ExtTracker::new();
        t.on_spawn(Piece::T, 0);
        t.on_spawn(Piece::T, 1);
        t.on_spawn(Piece::J, 2);
        assert_eq!(t.spawn_counts[0], 2); // T
        assert_eq!(t.spawn_counts[1], 1); // J
        assert_eq!(t.piece_drought[0], 1); // T: one piece since last T
        assert_eq!(t.piece_drought[1], 0); // J just spawned
        assert_eq!(t.piece_drought[6], 3); // I never spawned
    }

    #[test]
    fn rail_counts_preferred() {
        let mut t = ExtTracker::new();
        t.on_spawn(Piece::T, 0);
        t.on_rail([10, 9, 8, 11, 7, 9, 10]);
        let snap = t.snapshot(0, None, None, &LineClears::default(), 0, 0, 0);
        assert_eq!(snap.piece_dist.counts, [10, 9, 8, 11, 7, 9, 10]);
        assert!(snap.piece_dist.deviation > 0.0);
    }

    #[test]
    fn pace_projection_reaches_target() {
        // All-tetris mix at level 18, 100 lines in, 400k points.
        let clears = LineClears {
            tetris: 25,
            ..Default::default()
        };
        let pace = project_pace(400_000, Some(100), Some(18), &clears, 100, 100).unwrap();
        // 130 remaining lines at 300 base/line with rising multiplier ⇒
        // strictly more than 130 * 300 * 19 on top of the current score.
        assert!(pace > 400_000 + 130 * 300 * 19);
        // At the target the projection is the score itself.
        assert_eq!(
            project_pace(999, Some(230), Some(29), &clears, 230, 230),
            Some(999)
        );
    }

    #[test]
    fn pace_needs_inputs() {
        assert_eq!(
            project_pace(0, None, Some(18), &LineClears::default(), 4, 0),
            None
        );
        assert_eq!(
            project_pace(0, Some(4), Some(18), &LineClears::default(), 0, 0),
            None
        );
    }

    #[test]
    fn trt_trend_capped_and_thinned() {
        let mut t = ExtTracker::new();
        for i in 0..(TRT_TREND_CAP as i64 + 10) {
            t.on_clear(1, Some(0), i + 1, 0);
        }
        assert!(t.trt_trend.len() <= TRT_TREND_CAP);
        // First sample survives thinning (index 0 is always kept).
        assert_eq!(t.trt_trend[0].0, 1);
    }

    #[test]
    fn height_timeline_paced() {
        let mut t = ExtTracker::new();
        let g = empty_grid();
        t.on_board(&g, 0.0, false);
        t.on_board(&g, 0.1, false); // too soon, skipped
        t.on_board(&g, 0.30, false);
        assert_eq!(t.height_timeline.len(), 2);
    }

    #[test]
    fn thin_keeps_even_indices() {
        let mut v: Vec<i32> = (0..8).collect();
        thin(&mut v);
        assert_eq!(v, vec![0, 2, 4, 6]);
    }
}

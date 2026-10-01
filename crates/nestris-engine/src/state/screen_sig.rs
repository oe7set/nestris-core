//! Screen signatures: NES-tile layout fingerprints of the menu, pause and
//! ending screens.
//!
//! Every NES-Tetris screen is drawn from 8×8 tiles on a fixed 32×30 grid, so
//! a screen is recognized by comparing the mean luma of each tile against an
//! embedded reference with a masked Pearson correlation. The correlation is
//! invariant to capture gain/offset; tiles whose content changes (cursors,
//! digits, high-score names, the falling stack, the mod's title logo) are
//! masked out of the reference. A full grid costs one pass over the 256×240
//! canonical luma plus a few thousand multiply-adds per reference.
//!
//! References live in `assets/screens/<kind>.png`: a 32×60 grayscale image,
//! tile means in the top 30 rows and the mask (255 = compared) in the bottom
//! 30. They are generated from labelled captures by `nestris screens refs`.

use std::sync::OnceLock;

use nestris_vision::Image;
use nestris_vision::color::bgr_pixel_to_gray;

use crate::enums::GameState;
use crate::templates::decode_png_gray;

pub const COLS: usize = 32;
pub const ROWS: usize = 30;
pub const TILES: usize = COLS * ROWS;
const POOL_COLS: usize = COLS / 2;
const POOL_ROWS: usize = ROWS / 2;

/// Below this luma standard deviation over the compared tiles a sample is
/// flat (black screen, no signal): no reference can match it.
const MIN_SAMPLE_STD: f32 = 2.0;

/// The screens with a reference signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScreenKind {
    Title,
    TypeSelect,
    LevelSelect,
    HighscoreEntry,
    /// Rocket / ending cut scene after a high-scoring game.
    Ending,
    /// Copyright screen, flash-cart menus: console booting.
    Boot,
    InGame,
    /// Fully blanked pause screen ("PAUSE" on black).
    Pause,
}

impl ScreenKind {
    pub const ALL: [ScreenKind; 8] = [
        ScreenKind::Title,
        ScreenKind::TypeSelect,
        ScreenKind::LevelSelect,
        ScreenKind::HighscoreEntry,
        ScreenKind::Ending,
        ScreenKind::Boot,
        ScreenKind::InGame,
        ScreenKind::Pause,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ScreenKind::Title => "title",
            ScreenKind::TypeSelect => "type_select",
            ScreenKind::LevelSelect => "level_select",
            ScreenKind::HighscoreEntry => "highscore_entry",
            ScreenKind::Ending => "ending",
            ScreenKind::Boot => "boot",
            ScreenKind::InGame => "in_game",
            ScreenKind::Pause => "pause",
        }
    }

    pub fn from_name(name: &str) -> Option<ScreenKind> {
        Self::ALL.into_iter().find(|k| k.name() == name)
    }

    /// The wire state a match reports (the ending is part of game over,
    /// boot screens are not a game screen).
    pub fn state(self) -> GameState {
        match self {
            ScreenKind::Title => GameState::Title,
            ScreenKind::TypeSelect => GameState::TypeSelect,
            ScreenKind::LevelSelect => GameState::LevelSelect,
            ScreenKind::HighscoreEntry => GameState::HighscoreEntry,
            ScreenKind::Ending => GameState::GameOver,
            ScreenKind::Boot => GameState::Unknown,
            ScreenKind::InGame => GameState::InGame,
            ScreenKind::Pause => GameState::Paused,
        }
    }

    pub fn is_menu(self) -> bool {
        matches!(
            self,
            ScreenKind::Title
                | ScreenKind::TypeSelect
                | ScreenKind::LevelSelect
                | ScreenKind::HighscoreEntry
                | ScreenKind::Ending
        )
    }
}

/// Tiles masked by hand on top of the data-driven masks: content that differs
/// between ROM versions. `(kind, col, row, cols, rows)`.
const MANUAL_MASKS: &[(ScreenKind, usize, usize, usize, usize)] = &[
    // Title: the original's church vs the Retroverse logo of the event ROM.
    (ScreenKind::Title, 23, 16, 8, 10),
];

/// Mean luma per NES tile (32×30, row-major).
#[derive(Clone, Debug, PartialEq)]
pub struct TileGrid {
    pub luma: Vec<f32>,
}

impl TileGrid {
    /// Tile means of a single-channel image covering the full 256×240 NES
    /// frame (any size; tiles take proportional integer bounds).
    pub fn from_gray(gray: &Image) -> TileGrid {
        debug_assert_eq!(gray.channels, 1);
        let (w, h) = (gray.width, gray.height);
        let mut sums = vec![0u32; TILES];
        let mut counts = vec![0u32; TILES];
        let col_of: Vec<usize> = (0..w).map(|x| (x * COLS / w.max(1)).min(COLS - 1)).collect();
        for y in 0..h {
            let row = (y * ROWS / h.max(1)).min(ROWS - 1);
            let base = row * COLS;
            for (x, &v) in gray.row(y).iter().enumerate() {
                let t = base + col_of[x];
                sums[t] += v as u32;
                counts[t] += 1;
            }
        }
        TileGrid {
            luma: sums
                .iter()
                .zip(&counts)
                .map(|(&s, &c)| if c > 0 { s as f32 / c as f32 } else { 0.0 })
                .collect(),
        }
    }

    /// Tile means sampled from a BGR frame region assumed to show the full
    /// NES frame (4×4 nearest samples per tile; no geometry lock needed).
    pub fn from_bgr_region(bgr: &Image, x0: f64, y0: f64, w: f64, h: f64) -> TileGrid {
        debug_assert_eq!(bgr.channels, 3);
        const S: usize = 4;
        let mut luma = vec![0.0f32; TILES];
        let max_x = bgr.width.saturating_sub(1) as f64;
        let max_y = bgr.height.saturating_sub(1) as f64;
        for row in 0..ROWS {
            for col in 0..COLS {
                let mut sum = 0u32;
                for sy in 0..S {
                    let fy = (row as f64 + (sy as f64 + 0.5) / S as f64) / ROWS as f64;
                    let y = (y0 + fy * h).clamp(0.0, max_y) as usize;
                    for sx in 0..S {
                        let fx = (col as f64 + (sx as f64 + 0.5) / S as f64) / COLS as f64;
                        let x = (x0 + fx * w).clamp(0.0, max_x) as usize;
                        let px = bgr.pixel(x, y);
                        sum += bgr_pixel_to_gray(px[0], px[1], px[2]) as u32;
                    }
                }
                luma[row * COLS + col] = sum as f32 / (S * S) as f32;
            }
        }
        TileGrid { luma }
    }

    pub fn mean(&self) -> f32 {
        self.luma.iter().sum::<f32>() / TILES as f32
    }

    /// 2×2-pooled 16×15 grid (tolerates the misalignment of an estimated
    /// frame box).
    fn pooled(&self) -> Vec<f32> {
        let mut out = vec![0.0f32; POOL_COLS * POOL_ROWS];
        for r in 0..POOL_ROWS {
            for c in 0..POOL_COLS {
                let at = |rr: usize, cc: usize| self.luma[rr * COLS + cc];
                out[r * POOL_COLS + c] = (at(2 * r, 2 * c)
                    + at(2 * r, 2 * c + 1)
                    + at(2 * r + 1, 2 * c)
                    + at(2 * r + 1, 2 * c + 1))
                    / 4.0;
            }
        }
        out
    }
}

/// One reference: tile means plus the compared-tile mask.
#[derive(Clone, Debug)]
pub struct ScreenRef {
    pub kind: ScreenKind,
    pub mean: Vec<f32>,
    pub mask: Vec<bool>,
}

impl ScreenRef {
    /// Decode the 32×60 reference PNG layout (means over mask).
    pub fn from_png(kind: ScreenKind, bytes: &[u8]) -> ScreenRef {
        let img = decode_png_gray(bytes);
        assert!(
            img.width == COLS && img.height == 2 * ROWS,
            "screen reference {} must be {COLS}x{}",
            kind.name(),
            2 * ROWS
        );
        let mean = img.data[..TILES].iter().map(|&v| v as f32).collect();
        let mask = img.data[TILES..].iter().map(|&v| v >= 128).collect();
        let mut r = ScreenRef { kind, mean, mask };
        r.apply_manual_masks();
        r
    }

    /// Encode as the 32×60 reference image (top: means, bottom: mask).
    pub fn to_image(&self) -> Image {
        let mut img = Image::new(COLS, 2 * ROWS, 1);
        for (i, &m) in self.mean.iter().enumerate() {
            img.data[i] = m.round().clamp(0.0, 255.0) as u8;
        }
        for (i, &keep) in self.mask.iter().enumerate() {
            img.data[TILES + i] = if keep { 255 } else { 0 };
        }
        img
    }

    pub fn apply_manual_masks(&mut self) {
        for &(kind, col, row, cols, rows) in MANUAL_MASKS {
            if kind != self.kind {
                continue;
            }
            for r in row..(row + rows).min(ROWS) {
                for c in col..(col + cols).min(COLS) {
                    self.mask[r * COLS + c] = false;
                }
            }
        }
    }

    fn pooled(&self) -> (Vec<f32>, Vec<bool>) {
        let mut mean = vec![0.0f32; POOL_COLS * POOL_ROWS];
        let mut mask = vec![false; POOL_COLS * POOL_ROWS];
        for r in 0..POOL_ROWS {
            for c in 0..POOL_COLS {
                let cells = [
                    (2 * r) * COLS + 2 * c,
                    (2 * r) * COLS + 2 * c + 1,
                    (2 * r + 1) * COLS + 2 * c,
                    (2 * r + 1) * COLS + 2 * c + 1,
                ];
                let kept = cells.iter().filter(|&&i| self.mask[i]).count();
                mean[r * POOL_COLS + c] = cells.iter().map(|&i| self.mean[i]).sum::<f32>() / 4.0;
                // A pooled cell is only stable when all four tiles are.
                mask[r * POOL_COLS + c] = kept == 4;
            }
        }
        (mean, mask)
    }
}

/// A reference prepared for correlation: compared indexes, zero-mean values
/// and their norm.
struct Prepared {
    kind: ScreenKind,
    idx: Vec<u16>,
    centered: Vec<f32>,
    norm: f32,
}

impl Prepared {
    fn new(kind: ScreenKind, mean: &[f32], mask: &[bool]) -> Prepared {
        let idx: Vec<u16> = (0..mean.len())
            .filter(|&i| mask[i])
            .map(|i| i as u16)
            .collect();
        let m = idx.iter().map(|&i| mean[i as usize]).sum::<f32>() / idx.len().max(1) as f32;
        let centered: Vec<f32> = idx.iter().map(|&i| mean[i as usize] - m).collect();
        let norm = centered.iter().map(|v| v * v).sum::<f32>().sqrt();
        Prepared {
            kind,
            idx,
            centered,
            norm,
        }
    }

    /// Masked Pearson correlation of `sample` with this reference.
    fn score(&self, sample: &[f32]) -> f32 {
        let n = self.idx.len();
        if n < 8 || self.norm <= f32::EPSILON {
            return 0.0;
        }
        let m = self.idx.iter().map(|&i| sample[i as usize]).sum::<f32>() / n as f32;
        let mut dot = 0.0f32;
        let mut ss = 0.0f32;
        for (&i, &r) in self.idx.iter().zip(&self.centered) {
            let d = sample[i as usize] - m;
            dot += d * r;
            ss += d * d;
        }
        if ss < MIN_SAMPLE_STD * MIN_SAMPLE_STD * n as f32 {
            return 0.0;
        }
        dot / (ss.sqrt() * self.norm)
    }
}

/// Best reference for a grid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenMatch {
    pub kind: ScreenKind,
    /// Pearson correlation in [-1, 1].
    pub score: f32,
    /// Distance to the best *other* reference.
    pub margin: f32,
}

/// Correlates tile grids against a reference set.
pub struct SignatureMatcher {
    full: Vec<Prepared>,
    pooled: Vec<Prepared>,
}

impl SignatureMatcher {
    pub fn new(refs: &[ScreenRef]) -> SignatureMatcher {
        let full = refs
            .iter()
            .map(|r| Prepared::new(r.kind, &r.mean, &r.mask))
            .collect();
        let pooled = refs
            .iter()
            .map(|r| {
                let (mean, mask) = r.pooled();
                Prepared::new(r.kind, &mean, &mask)
            })
            .collect();
        SignatureMatcher { full, pooled }
    }

    /// The embedded reference set.
    pub fn builtin() -> &'static SignatureMatcher {
        static M: OnceLock<SignatureMatcher> = OnceLock::new();
        M.get_or_init(|| SignatureMatcher::new(&builtin_refs()))
    }

    /// Scores of every reference on an aligned (geometry-locked) grid.
    pub fn scores(&self, grid: &TileGrid) -> Vec<(ScreenKind, f32)> {
        self.full
            .iter()
            .map(|p| (p.kind, p.score(&grid.luma)))
            .collect()
    }

    /// Scores on the pooled grid (estimated frame box, no lock).
    pub fn scores_pooled(&self, grid: &TileGrid) -> Vec<(ScreenKind, f32)> {
        let pooled = grid.pooled();
        self.pooled
            .iter()
            .map(|p| (p.kind, p.score(&pooled)))
            .collect()
    }

    /// Score of one reference on an aligned grid.
    pub fn score_of(&self, grid: &TileGrid, kind: ScreenKind) -> Option<f32> {
        self.full
            .iter()
            .find(|p| p.kind == kind)
            .map(|p| p.score(&grid.luma))
    }

    pub fn best(&self, grid: &TileGrid) -> Option<ScreenMatch> {
        best_of(&self.scores(grid))
    }

    pub fn best_pooled(&self, grid: &TileGrid) -> Option<ScreenMatch> {
        best_of(&self.scores_pooled(grid))
    }
}

fn best_of(scores: &[(ScreenKind, f32)]) -> Option<ScreenMatch> {
    let (best_i, &(kind, score)) = scores
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.1.total_cmp(&b.1.1))?;
    let second = scores
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != best_i)
        .map(|(_, s)| s.1)
        .fold(f32::MIN, f32::max);
    Some(ScreenMatch {
        kind,
        score,
        margin: if second == f32::MIN {
            score
        } else {
            score - second
        },
    })
}

/// Estimated NES frame box in a raw frame: the bounding box of non-black
/// content (rows/columns with at least 3 % bright samples). `None` when the
/// frame is too dark or the box too small to be a full screen.
pub fn content_box(bgr: &Image) -> Option<(f64, f64, f64, f64)> {
    const STEP: usize = 4;
    const BRIGHT: u8 = 40;
    let (w, h) = (bgr.width, bgr.height);
    let cols = w.div_ceil(STEP);
    let rows = h.div_ceil(STEP);
    let mut col_hits = vec![0u32; cols];
    let mut row_hits = vec![0u32; rows];
    for (ri, y) in (0..h).step_by(STEP).enumerate() {
        for (ci, x) in (0..w).step_by(STEP).enumerate() {
            let px = bgr.pixel(x, y);
            if bgr_pixel_to_gray(px[0], px[1], px[2]) > BRIGHT {
                col_hits[ci] += 1;
                row_hits[ri] += 1;
            }
        }
    }
    let span = |hits: &[u32], across: usize| {
        let need = ((across as f64 * 0.03).ceil() as u32).max(1);
        let first = hits.iter().position(|&n| n >= need)?;
        let last = hits.iter().rposition(|&n| n >= need)?;
        Some((first, last))
    };
    let (c0, c1) = span(&col_hits, rows)?;
    let (r0, r1) = span(&row_hits, cols)?;
    let (x0, x1) = ((c0 * STEP) as f64, ((c1 + 1) * STEP).min(w) as f64);
    let (y0, y1) = ((r0 * STEP) as f64, ((r1 + 1) * STEP).min(h) as f64);
    let (bw, bh) = (x1 - x0, y1 - y0);
    if bw < w as f64 * 0.5 || bh < h as f64 * 0.5 {
        return None;
    }
    Some((x0, y0, bw, bh))
}

macro_rules! screen_refs {
    ($($kind:expr => $path:literal),+ $(,)?) => {
        &[$(($kind, include_bytes!($path) as &[u8])),+]
    };
}

const SCREEN_PNGS: &[(ScreenKind, &[u8])] = screen_refs![
    ScreenKind::Title => "../../assets/screens/title.png",
    ScreenKind::TypeSelect => "../../assets/screens/type_select.png",
    ScreenKind::LevelSelect => "../../assets/screens/level_select.png",
    ScreenKind::HighscoreEntry => "../../assets/screens/highscore_entry.png",
    ScreenKind::Ending => "../../assets/screens/ending.png",
    ScreenKind::Boot => "../../assets/screens/boot.png",
    ScreenKind::InGame => "../../assets/screens/in_game.png",
    ScreenKind::Pause => "../../assets/screens/pause.png",
];

/// The embedded references.
pub fn builtin_refs() -> Vec<ScreenRef> {
    SCREEN_PNGS
        .iter()
        .map(|(kind, bytes)| ScreenRef::from_png(*kind, bytes))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic(seed: u32) -> Vec<f32> {
        (0..TILES)
            .map(|i| (((i as u32).wrapping_mul(2654435761u32.wrapping_add(seed.wrapping_mul(40503))) >> 7) % 200) as f32)
            .collect()
    }

    #[test]
    fn correlation_is_gain_and_offset_invariant() {
        let mean = synthetic(1);
        let r = ScreenRef {
            kind: ScreenKind::Title,
            mean: mean.clone(),
            mask: vec![true; TILES],
        };
        let m = SignatureMatcher::new(&[r]);
        let grid = TileGrid {
            luma: mean.iter().map(|v| v * 0.6 + 20.0).collect(),
        };
        let s = m.scores(&grid)[0].1;
        assert!(s > 0.999, "{s}");
    }

    #[test]
    fn flat_sample_never_matches() {
        let r = ScreenRef {
            kind: ScreenKind::Pause,
            mean: synthetic(2),
            mask: vec![true; TILES],
        };
        let m = SignatureMatcher::new(&[r]);
        let grid = TileGrid {
            luma: vec![3.0; TILES],
        };
        assert_eq!(m.scores(&grid)[0].1, 0.0);
    }

    #[test]
    fn best_reports_margin() {
        let a = ScreenRef {
            kind: ScreenKind::Title,
            mean: synthetic(3),
            mask: vec![true; TILES],
        };
        let b = ScreenRef {
            kind: ScreenKind::TypeSelect,
            mean: synthetic(4),
            mask: vec![true; TILES],
        };
        let m = SignatureMatcher::new(&[a.clone(), b]);
        let best = m.best(&TileGrid { luma: a.mean }).unwrap();
        assert_eq!(best.kind, ScreenKind::Title);
        assert!(best.margin > 0.5, "{best:?}");
    }

    #[test]
    fn reference_png_roundtrip() {
        let mut mask = vec![true; TILES];
        mask[5] = false;
        let r = ScreenRef {
            kind: ScreenKind::Boot,
            mean: synthetic(5),
            mask,
        };
        let img = r.to_image();
        assert_eq!((img.width, img.height), (COLS, 2 * ROWS));
        assert_eq!(img.data[TILES + 5], 0);
        assert_eq!(img.data[TILES + 6], 255);
    }

    #[test]
    fn builtin_refs_load() {
        let refs = builtin_refs();
        assert_eq!(refs.len(), ScreenKind::ALL.len());
        for r in &refs {
            let kept = r.mask.iter().filter(|&&k| k).count();
            assert!(kept >= 40, "{}: only {kept} tiles compared", r.kind.name());
        }
    }

    #[test]
    fn content_box_finds_frame() {
        let mut img = Image::new(200, 160, 3);
        for y in 20..150 {
            for x in 10..190 {
                img.pixel_mut(x, y).copy_from_slice(&[120, 120, 120]);
            }
        }
        let (x, y, w, h) = content_box(&img).unwrap();
        assert!((x - 8.0).abs() <= 4.0 && (y - 20.0).abs() <= 4.0, "{x} {y}");
        assert!((w - 180.0).abs() <= 8.0 && (h - 130.0).abs() <= 8.0, "{w} {h}");
        assert!(content_box(&Image::new(200, 160, 3)).is_none());
    }
}

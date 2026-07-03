//! NEXT-box tetromino recognition by shape (port of `recognition/next_piece.py`).

use std::collections::BTreeSet;

use nestris_vision::{Image, components, morphology, threshold};

use crate::enums::Piece;
use crate::geometry::Rect;
use crate::palette::to_luma;

const EMPTY_LUMA: f64 = 10.0;
const GRID_COLS: usize = 4;
const CELL_FILL_FRACTION: f64 = 0.4;
const FLAT_ROW_RATIO: f64 = 0.5;
const MIN_COMPONENT_FRAC: f64 = 0.2;

/// Result of reading the NEXT box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PieceReading {
    pub piece: Option<Piece>,
    pub confidence: f32,
}

/// A normalized footprint: sorted (row, col) cells with min row/col at 0.
pub type Footprint = BTreeSet<(i32, i32)>;

/// Canonical spawn footprints of the seven tetrominoes.
pub fn spawn_footprints() -> &'static [(Piece, Footprint)] {
    use std::sync::OnceLock;
    static TABLE: OnceLock<Vec<(Piece, Footprint)>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let fp = |cells: &[(i32, i32)]| cells.iter().copied().collect::<Footprint>();
        vec![
            (Piece::I, fp(&[(0, 0), (0, 1), (0, 2), (0, 3)])),
            (Piece::O, fp(&[(0, 0), (0, 1), (1, 0), (1, 1)])),
            (Piece::T, fp(&[(0, 0), (0, 1), (0, 2), (1, 1)])),
            (Piece::S, fp(&[(0, 1), (0, 2), (1, 0), (1, 1)])),
            (Piece::Z, fp(&[(0, 0), (0, 1), (1, 1), (1, 2)])),
            (Piece::J, fp(&[(0, 0), (1, 0), (1, 1), (1, 2)])),
            (Piece::L, fp(&[(0, 2), (1, 0), (1, 1), (1, 2)])),
        ]
    })
}

fn normalize(cells: &Footprint) -> Footprint {
    let r0 = cells.iter().map(|c| c.0).min().unwrap_or(0);
    let c0 = cells.iter().map(|c| c.1).min().unwrap_or(0);
    cells.iter().map(|&(r, c)| (r - r0, c - c0)).collect()
}

fn rotate(cells: &Footprint) -> Footprint {
    normalize(&cells.iter().map(|&(r, c)| (c, -r)).collect())
}

/// All 4 rotations of all 7 pieces -> (piece, rotation index).
fn rotation_table() -> &'static Vec<(Footprint, (Piece, u8))> {
    use std::sync::OnceLock;
    static TABLE: OnceLock<Vec<(Footprint, (Piece, u8))>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table: Vec<(Footprint, (Piece, u8))> = Vec::new();
        for (piece, footprint) in spawn_footprints() {
            let mut cells = normalize(footprint);
            for rot in 0..4u8 {
                if !table.iter().any(|(f, _)| f == &cells) {
                    table.push((cells.clone(), (*piece, rot)));
                }
                cells = rotate(&cells);
            }
        }
        table
    })
}

/// Rotation-tolerant footprint classification (exact 4-cell match).
pub fn classify_footprint(cells: &Footprint) -> Option<(Piece, u8)> {
    if cells.len() != 4 {
        return None;
    }
    let norm = normalize(cells);
    rotation_table()
        .iter()
        .find(|(f, _)| f == &norm)
        .map(|(_, m)| *m)
}

/// Keep only components >= MIN_COMPONENT_FRAC of the largest (drop speckle).
fn largest_components(binary: &Image) -> Image {
    let labeled = components::connected_components(binary);
    if labeled.components.is_empty() {
        return binary.clone();
    }
    let max_area = labeled.components.iter().map(|c| c.area).max().unwrap() as f64;
    let keep_min = max_area * MIN_COMPONENT_FRAC;
    let mut out = Image::new(binary.width, binary.height, 1);
    for (i, (&label, dst)) in labeled.labels.iter().zip(out.data.iter_mut()).enumerate() {
        let _ = i;
        if label != 0 && labeled.components[(label - 1) as usize].area as f64 >= keep_min {
            *dst = 255;
        }
    }
    out
}

/// Reads the tetromino in the NEXT box from a canonical frame.
pub struct NextPieceReader;

impl NextPieceReader {
    pub fn read(canon: &Image, next_box: &Rect) -> PieceReading {
        let (x0, y0, x1, y1) = next_box.to_bounds();
        let (x1, y1) = (x1.min(canon.width), y1.min(canon.height));
        if x1 <= x0 || y1 <= y0 {
            return PieceReading {
                piece: None,
                confidence: 0.0,
            };
        }
        let patch = canon.crop(x0, y0, x1 - x0, y1 - y0);
        let gray = to_luma(&patch);
        let mean = gray.data.iter().map(|&v| v as f64).sum::<f64>() / gray.data.len() as f64;
        if mean < EMPTY_LUMA {
            return PieceReading {
                piece: Some(Piece::None),
                confidence: 1.0,
            };
        }
        let Some(cells) = piece_cells(&gray) else {
            return PieceReading {
                piece: None,
                confidence: 0.0,
            };
        };
        if cells.len() != 4 {
            return PieceReading {
                piece: None,
                confidence: 0.0,
            };
        }
        match classify_footprint(&cells) {
            Some((piece, _rot)) => PieceReading {
                piece: Some(piece),
                confidence: 1.0,
            },
            None => PieceReading {
                piece: None,
                confidence: 0.0,
            },
        }
    }
}

/// Otsu-binarize, clean, crop to the piece bbox, resample onto a grid sized
/// to the piece, return the normalized filled-cell set.
fn piece_cells(gray: &Image) -> Option<Footprint> {
    let (_thresh, mut binary) = threshold::threshold_binary_otsu(gray, 255);
    let k3 = morphology::Kernel::ellipse3();
    binary = morphology::close(&binary, &k3, 1);
    binary = morphology::open(&binary, &k3, 1);
    binary = largest_components(&binary);

    let mut y0 = usize::MAX;
    let mut y1 = 0usize;
    let mut x0 = usize::MAX;
    let mut x1 = 0usize;
    for y in 0..binary.height {
        for x in 0..binary.width {
            if binary.data[y * binary.width + x] != 0 {
                y0 = y0.min(y);
                y1 = y1.max(y);
                x0 = x0.min(x);
                x1 = x1.max(x);
            }
        }
    }
    if y0 == usize::MAX {
        return None;
    }
    let crop = binary.crop(x0, y0, x1 - x0 + 1, y1 - y0 + 1);
    let (ch, cw) = (crop.height, crop.width);
    if ch == 0 || cw == 0 {
        return None;
    }

    let rows = if (ch as f64) < FLAT_ROW_RATIO * cw as f64 {
        1usize
    } else {
        2
    };
    let unit = ch as f64 / rows as f64;
    let cols = if unit > 0.0 {
        GRID_COLS.min(((cw as f64 / unit).round_ties_even() as usize).max(1))
    } else {
        1
    };

    let mut filled: Footprint = BTreeSet::new();
    for r in 0..rows {
        for c in 0..cols {
            let r0p = (r as f64 * ch as f64 / rows as f64).round_ties_even() as usize;
            let r1p = ((r + 1) as f64 * ch as f64 / rows as f64).round_ties_even() as usize;
            let c0p = (c as f64 * cw as f64 / cols as f64).round_ties_even() as usize;
            let c1p = ((c + 1) as f64 * cw as f64 / cols as f64).round_ties_even() as usize;
            if r1p <= r0p || c1p <= c0p {
                continue;
            }
            let mut sum = 0.0f64;
            let mut count = 0usize;
            for y in r0p..r1p.min(ch) {
                for x in c0p..c1p.min(cw) {
                    sum += crop.data[y * cw + x] as f64;
                    count += 1;
                }
            }
            if count > 0 && sum / count as f64 / 255.0 >= CELL_FILL_FRACTION {
                filled.insert((r as i32, c as i32));
            }
        }
    }
    if filled.is_empty() {
        return None;
    }
    Some(normalize(&filled))
}

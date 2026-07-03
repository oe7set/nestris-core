//! Falling-piece separation from the settled stack
//! (port of `recognition/current_piece.py`).

use crate::enums::Piece;
use crate::layout::{PLAYFIELD_COLS, PLAYFIELD_ROWS};
use crate::recognition::next_piece::{Footprint, classify_footprint};
use crate::recognition::playfield::Grid;

const MAX_FRESH_CELLS: usize = 6;

/// Result of locating the falling piece.
#[derive(Clone, Debug, Default)]
pub struct CurrentPieceReading {
    pub piece: Option<Piece>,
    pub row: Option<usize>,
    pub col: Option<usize>,
    pub rotation: Option<u8>,
    pub cells: Vec<(usize, usize)>,
    pub confidence: f32,
}

fn connected_groups(mask: &Grid<bool>) -> Vec<Vec<(usize, usize)>> {
    let mut seen = [[false; PLAYFIELD_COLS]; PLAYFIELD_ROWS];
    let mut groups = Vec::new();
    for r in 0..PLAYFIELD_ROWS {
        for c in 0..PLAYFIELD_COLS {
            if !mask[r][c] || seen[r][c] {
                continue;
            }
            let mut stack = vec![(r, c)];
            seen[r][c] = true;
            let mut group = Vec::new();
            while let Some((cr, cc)) = stack.pop() {
                group.push((cr, cc));
                let neighbors = [
                    (cr.wrapping_add(1), cc),
                    (cr.wrapping_sub(1), cc),
                    (cr, cc.wrapping_add(1)),
                    (cr, cc.wrapping_sub(1)),
                ];
                for (nr, nc) in neighbors {
                    if nr < PLAYFIELD_ROWS && nc < PLAYFIELD_COLS && mask[nr][nc] && !seen[nr][nc] {
                        seen[nr][nc] = true;
                        stack.push((nr, nc));
                    }
                }
            }
            groups.push(group);
        }
    }
    groups
}

/// Locates the falling piece from a playfield occupancy grid.
pub struct CurrentPieceReader {
    held_stack: Grid<bool>,
}

impl Default for CurrentPieceReader {
    fn default() -> Self {
        Self::new()
    }
}

impl CurrentPieceReader {
    pub fn new() -> Self {
        Self {
            held_stack: [[false; PLAYFIELD_COLS]; PLAYFIELD_ROWS],
        }
    }

    pub fn reset(&mut self) {
        self.held_stack = [[false; PLAYFIELD_COLS]; PLAYFIELD_ROWS];
    }

    /// Adopt `occupancy` as the new settled stack (piece locked).
    pub fn commit(&mut self, occupancy: &Grid<bool>) {
        self.held_stack = *occupancy;
    }

    pub fn read(&self, occupancy: &Grid<bool>) -> CurrentPieceReading {
        let candidate = self.subtract(occupancy).or_else(|| self.surface(occupancy));
        let Some(mut cells) = candidate else {
            return CurrentPieceReading::default();
        };
        if cells.is_empty() {
            return CurrentPieceReading::default();
        }
        cells.sort();
        let r0 = cells.iter().map(|c| c.0).min().unwrap();
        let c0 = cells.iter().map(|c| c.1).min().unwrap();
        let footprint: Footprint = cells.iter().map(|&(r, c)| (r as i32, c as i32)).collect();
        match classify_footprint(&footprint) {
            None => CurrentPieceReading {
                piece: None,
                row: Some(r0),
                col: Some(c0),
                rotation: None,
                cells,
                confidence: 0.3,
            },
            Some((piece, rotation)) => CurrentPieceReading {
                piece: Some(piece),
                row: Some(r0),
                col: Some(c0),
                rotation: Some(rotation),
                cells,
                confidence: 0.9,
            },
        }
    }

    /// Method A: cells present now but not in the held stack.
    fn subtract(&self, occupancy: &Grid<bool>) -> Option<Vec<(usize, usize)>> {
        if !self.held_stack.iter().flatten().any(|&v| v) {
            return None;
        }
        let mut fresh = [[false; PLAYFIELD_COLS]; PLAYFIELD_ROWS];
        let mut count = 0usize;
        for r in 0..PLAYFIELD_ROWS {
            for c in 0..PLAYFIELD_COLS {
                if occupancy[r][c] && !self.held_stack[r][c] {
                    fresh[r][c] = true;
                    count += 1;
                }
            }
        }
        if count == 0 || count > MAX_FRESH_CELLS {
            return None;
        }
        let groups = connected_groups(&fresh);
        let mut small: Vec<Vec<(usize, usize)>> =
            groups.into_iter().filter(|g| g.len() <= 4).collect();
        if small.is_empty() {
            return None;
        }
        small.sort_by_key(|g| g.iter().map(|c| c.0).min().unwrap());
        Some(small.swap_remove(0))
    }

    /// Method B: cells floating strictly above the per-column stack surface.
    fn surface(&self, occupancy: &Grid<bool>) -> Option<Vec<(usize, usize)>> {
        let mut floating = [[false; PLAYFIELD_COLS]; PLAYFIELD_ROWS];
        let mut any = false;
        for c in 0..PLAYFIELD_COLS {
            let col_cells: Vec<usize> = (0..PLAYFIELD_ROWS).filter(|&r| occupancy[r][c]).collect();
            if col_cells.is_empty() {
                continue;
            }
            // Top of the bottom-most contiguous run of filled cells.
            let mut bottom_run_top = *col_cells.last().unwrap();
            while bottom_run_top > 0 && col_cells.contains(&(bottom_run_top - 1)) {
                bottom_run_top -= 1;
            }
            for &rr in &col_cells {
                if rr < bottom_run_top {
                    floating[rr][c] = true;
                    any = true;
                }
            }
        }
        if !any {
            return None;
        }
        let groups = connected_groups(&floating);
        let mut small: Vec<Vec<(usize, usize)>> = groups
            .into_iter()
            .filter(|g| (1..=4).contains(&g.len()))
            .collect();
        if small.is_empty() {
            return None;
        }
        small.sort_by_key(|g| {
            let top = g.iter().map(|c| c.0).min().unwrap();
            (top, std::cmp::Reverse(g.len()))
        });
        Some(small.swap_remove(0))
    }
}

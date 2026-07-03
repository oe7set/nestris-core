//! Temporal per-cell Schmitt hysteresis (port of `recognition/stabilizer.py`).

use crate::layout::{PLAYFIELD_COLS, PLAYFIELD_ROWS};
use crate::recognition::playfield::{Grid, PlayfieldReading};

const ON_THRESHOLD: f32 = 1.0;
const HOLD_THRESHOLD: f32 = 0.7;

/// Stateful per-cell hysteresis between the playfield reader and fusion.
#[derive(Default)]
pub struct PlayfieldStabilizer {
    prev_occupancy: Option<Grid<bool>>,
    prev_grid: Option<Grid<u8>>,
}

impl PlayfieldStabilizer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        self.prev_occupancy = None;
        self.prev_grid = None;
    }

    /// Return `reading` with per-cell hysteresis applied.
    pub fn update(&mut self, reading: &PlayfieldReading) -> PlayfieldReading {
        let strength = &reading.strength;
        let mut occupancy: Grid<bool> = [[false; PLAYFIELD_COLS]; PLAYFIELD_ROWS];
        for r in 0..PLAYFIELD_ROWS {
            for c in 0..PLAYFIELD_COLS {
                let mut occ = strength[r][c] > ON_THRESHOLD;
                if let Some(prev) = &self.prev_occupancy {
                    occ = occ || (prev[r][c] && strength[r][c] > HOLD_THRESHOLD);
                }
                occupancy[r][c] = occ;
            }
        }

        let mut grid = reading.grid;
        if let Some(prev_grid) = &self.prev_grid {
            for r in 0..PLAYFIELD_ROWS {
                for c in 0..PLAYFIELD_COLS {
                    // Ambiguously-classified cells keep their previous id.
                    if occupancy[r][c] && reading.color_ambiguous[r][c] && prev_grid[r][c] > 0 {
                        grid[r][c] = prev_grid[r][c];
                    }
                    // Hysteresis-held cells (grid id 0) keep their previous id.
                    if occupancy[r][c] && grid[r][c] == 0 {
                        grid[r][c] = prev_grid[r][c];
                    }
                }
            }
        }

        self.prev_occupancy = Some(occupancy);
        self.prev_grid = Some(grid);
        let filled_count = occupancy
            .iter()
            .map(|r| r.iter().filter(|&&v| v).count())
            .sum();
        PlayfieldReading {
            grid,
            occupancy,
            confidence: reading.confidence,
            filled_count,
            strength: reading.strength,
            color_ambiguous: reading.color_ambiguous,
        }
    }
}

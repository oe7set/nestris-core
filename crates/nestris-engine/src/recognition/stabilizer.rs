//! Temporal per-cell Schmitt hysteresis (port of `recognition/stabilizer.py`),
//! plus optional per-cell color voting.

use crate::layout::{PLAYFIELD_COLS, PLAYFIELD_ROWS};
use crate::recognition::playfield::{Grid, PlayfieldReading};

const ON_THRESHOLD: f32 = 1.0;
const HOLD_THRESHOLD: f32 = 0.7;
/// Color-voting window length (frames).
const VOTE_WINDOW: usize = 5;
/// Vote weight of an ambiguously-classified sample.
const AMBIGUOUS_WEIGHT: f32 = 0.4;

/// Ring buffer of recent (color id, weight) samples for one cell.
#[derive(Clone, Copy, Default)]
struct CellVotes {
    ids: [u8; VOTE_WINDOW],
    weights: [f32; VOTE_WINDOW],
    len: u8,
    cursor: u8,
}

impl CellVotes {
    fn clear(&mut self) {
        self.len = 0;
        self.cursor = 0;
    }

    fn push(&mut self, id: u8, weight: f32) {
        self.ids[self.cursor as usize] = id;
        self.weights[self.cursor as usize] = weight;
        self.cursor = (self.cursor + 1) % VOTE_WINDOW as u8;
        self.len = (self.len + 1).min(VOTE_WINDOW as u8);
    }

    /// Weighted-majority id; ties resolve to the most recent sample.
    fn winner(&self) -> Option<u8> {
        if self.len == 0 {
            return None;
        }
        let mut sums = [0.0f32; 4]; // ids 0..=3 (0 never voted)
        for k in 0..self.len as usize {
            let id = self.ids[k] as usize;
            if id < sums.len() {
                sums[id] += self.weights[k];
            }
        }
        let newest =
            self.ids[(self.cursor as usize + VOTE_WINDOW - 1) % VOTE_WINDOW] as usize;
        let mut best = newest;
        for (id, &sum) in sums.iter().enumerate() {
            if sum > sums[best] {
                best = id;
            }
        }
        Some(best as u8)
    }
}

/// Stateful per-cell hysteresis between the playfield reader and fusion.
#[derive(Default)]
pub struct PlayfieldStabilizer {
    prev_occupancy: Option<Grid<bool>>,
    prev_grid: Option<Grid<u8>>,
    /// Per-cell color-vote history (allocated only when voting is enabled).
    votes: Option<Box<Grid<CellVotes>>>,
}

impl PlayfieldStabilizer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable temporal color voting: an occupied cell's reported id becomes
    /// the weighted majority of its last few classifications instead of
    /// "keep the previous id when ambiguous". Kills color flicker on levels
    /// with close accent pairs.
    pub fn new_with_voting(color_voting: bool) -> Self {
        Self {
            votes: color_voting
                .then(|| Box::new([[CellVotes::default(); PLAYFIELD_COLS]; PLAYFIELD_ROWS])),
            ..Self::default()
        }
    }

    pub fn reset(&mut self) {
        self.prev_occupancy = None;
        self.prev_grid = None;
        if let Some(votes) = &mut self.votes {
            for row in votes.iter_mut() {
                for cell in row.iter_mut() {
                    cell.clear();
                }
            }
        }
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
        if let Some(votes) = &mut self.votes {
            // Voting mode: every classification (weighted down when
            // ambiguous) enters the cell's window; the reported id is the
            // weighted majority instead of a hard latch on the previous id.
            for r in 0..PLAYFIELD_ROWS {
                for c in 0..PLAYFIELD_COLS {
                    if !occupancy[r][c] {
                        votes[r][c].clear();
                        continue;
                    }
                    if reading.grid[r][c] > 0 {
                        let weight = if reading.color_ambiguous[r][c] {
                            AMBIGUOUS_WEIGHT
                        } else {
                            1.0
                        };
                        votes[r][c].push(reading.grid[r][c], weight);
                    }
                    if let Some(winner) = votes[r][c].winner() {
                        grid[r][c] = winner;
                    } else if let Some(prev_grid) = &self.prev_grid {
                        // Hysteresis-held cell with no votes yet.
                        grid[r][c] = prev_grid[r][c];
                    }
                }
            }
        } else if let Some(prev_grid) = &self.prev_grid {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(id: u8, ambiguous: bool) -> PlayfieldReading {
        let mut r = PlayfieldReading {
            grid: [[0; PLAYFIELD_COLS]; PLAYFIELD_ROWS],
            occupancy: [[false; PLAYFIELD_COLS]; PLAYFIELD_ROWS],
            confidence: 1.0,
            filled_count: 1,
            strength: [[0.0; PLAYFIELD_COLS]; PLAYFIELD_ROWS],
            color_ambiguous: [[false; PLAYFIELD_COLS]; PLAYFIELD_ROWS],
        };
        r.grid[10][4] = id;
        r.occupancy[10][4] = true;
        r.strength[10][4] = 1.5;
        r.color_ambiguous[10][4] = ambiguous;
        r
    }

    #[test]
    fn voting_suppresses_single_frame_color_flips() {
        let mut voting = PlayfieldStabilizer::new_with_voting(true);
        let mut plain = PlayfieldStabilizer::new_with_voting(false);
        for _ in 0..3 {
            voting.update(&reading(2, false));
            plain.update(&reading(2, false));
        }
        // One unambiguous misread flips the plain path immediately...
        let flip = reading(3, false);
        assert_eq!(plain.update(&flip).grid[10][4], 3);
        // ...but the weighted majority holds the stable id.
        assert_eq!(voting.update(&flip).grid[10][4], 2);
    }

    #[test]
    fn voting_follows_sustained_change() {
        let mut voting = PlayfieldStabilizer::new_with_voting(true);
        for _ in 0..3 {
            voting.update(&reading(2, false));
        }
        let mut latest = 0;
        for _ in 0..4 {
            latest = voting.update(&reading(3, false)).grid[10][4];
        }
        assert_eq!(latest, 3, "a real change wins the window");
    }

    #[test]
    fn ambiguous_votes_carry_less_weight() {
        let mut voting = PlayfieldStabilizer::new_with_voting(true);
        voting.update(&reading(2, false));
        // Two ambiguous 3-votes (0.4 each) do not outvote one solid 2.
        voting.update(&reading(3, true));
        let out = voting.update(&reading(3, true));
        assert_eq!(out.grid[10][4], 2);
        // A third ambiguous vote tips the sum (1.2 > 1.0).
        let out = voting.update(&reading(3, true));
        assert_eq!(out.grid[10][4], 3);
    }

    #[test]
    fn vacated_cells_clear_their_history() {
        let mut voting = PlayfieldStabilizer::new_with_voting(true);
        for _ in 0..3 {
            voting.update(&reading(2, false));
        }
        // Cell empties (strength low, no previous hold since occupancy drops
        // below the hold threshold too).
        let mut empty = reading(0, false);
        empty.occupancy[10][4] = false;
        empty.strength[10][4] = 0.1;
        voting.update(&empty);
        // Re-fills with id 3: history was cleared, 3 wins immediately.
        assert_eq!(voting.update(&reading(3, false)).grid[10][4], 3);
    }
}

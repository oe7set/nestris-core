//! STATISTICS piece-count recognition (port of `recognition/statistics.py`).

use nestris_vision::Image;

use crate::enums::Piece;
use crate::layout::{LayoutTable, STATS_ORDER};
use crate::recognition::digits::{BaseMode, DigitReader};

/// Result of reading the STATISTICS counts, in STATS_ORDER.
#[derive(Clone, Debug)]
pub struct StatisticsReading {
    /// Per-piece count in STATS_ORDER (T, J, Z, O, S, L, I).
    pub counts: [(Piece, Option<i64>); 7],
    pub confidence: f32,
}

/// Reads the seven STATISTICS counts from the shared canonical luma.
pub struct StatisticsReader {
    digits: DigitReader,
}

impl Default for StatisticsReader {
    fn default() -> Self {
        Self::new()
    }
}

impl StatisticsReader {
    pub fn new() -> Self {
        // Statistics counts are always decimal, even on hex-score mods.
        Self {
            digits: DigitReader::new(BaseMode::Dec),
        }
    }

    pub fn read(&self, gray: &Image, layout: &LayoutTable) -> StatisticsReading {
        let mut counts: [(Piece, Option<i64>); 7] = STATS_ORDER.map(|p| (p, None));
        let mut conf_sum = 0.0f32;
        let mut conf_count = 0usize;
        for (i, rect) in layout.statistics.iter().enumerate() {
            let reading = self
                .digits
                .read_field(gray, rect, layout.stats_digits, None);
            counts[i].1 = reading.value;
            if reading.value.is_some() {
                conf_sum += reading.confidence;
                conf_count += 1;
            }
        }
        StatisticsReading {
            counts,
            confidence: if conf_count > 0 {
                conf_sum / conf_count as f32
            } else {
                0.0
            },
        }
    }
}

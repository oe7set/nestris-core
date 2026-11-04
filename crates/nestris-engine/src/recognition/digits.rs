//! Numeric field recognition (port of `recognition/digits.py`).
//!
//! Every field is decimal arithmetic. A NES score is BCD: only the top digit of
//! a modded ROM can show `A`-`F` (its value 10-15), so `A00000` is 1 000 000 and
//! the lower five digits stay decimal. Reading the whole field as base 16 turned
//! 025782 into 153 474, and latching the base kept that for every later game.

use nestris_vision::Image;

use crate::geometry::Rect;
use crate::templates::{MatchResult, TemplateSet, digit_templates};

const MIN_DIGIT_SCORE: f32 = 0.62;
const MIN_MARGIN: f32 = 0.02;
const BLANK_LUMA: f64 = 12.0;
const RECENTER_RADIUS: i64 = 2;

const DEC_LABELS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
const HEX_LABELS: [&str; 16] = [
    "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "A", "B", "C", "D", "E", "F",
];

fn digit_value(label: &str) -> i64 {
    i64::from_str_radix(label, 16).expect("digit label")
}

/// Which glyphs a SCORE read accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BaseMode {
    /// Digits only (vanilla ROM).
    Dec,
    /// `A`-`F` allowed in the top digit.
    Hex,
    /// `Hex` once the score can reach the top digit's letters, else `Dec`
    /// (decided by the caller, which knows the held score).
    Auto,
}

/// Result of reading one numeric field.
#[derive(Clone, Debug)]
pub struct DigitReading {
    pub value: Option<i64>,
    pub confidence: f32,
    /// Per-cell (label, score) diagnostics.
    pub digits: Vec<(Option<&'static str>, f32)>,
}

impl DigitReading {
    fn unreadable(digits: Vec<(Option<&'static str>, f32)>) -> Self {
        DigitReading {
            value: None,
            confidence: 0.0,
            digits,
        }
    }
}

/// Reads fixed-width numeric fields from a canonical frame's luma.
pub struct DigitReader {
    pub default_base: BaseMode,
}

impl DigitReader {
    pub fn new(default_base: BaseMode) -> Self {
        Self { default_base }
    }

    /// Read a field from the shared whole-frame luma `gray` (256×240, 1ch).
    pub fn read_field(
        &self,
        gray: &Image,
        field: &Rect,
        digit_count: usize,
        base: Option<BaseMode>,
    ) -> DigitReading {
        let top_letters = base.unwrap_or(self.default_base) == BaseMode::Hex;
        read_digits(gray, field, digit_count, digit_templates(), top_letters)
    }
}

fn crop_gray(gray: &Image, x0: usize, y0: usize, x1: usize, y1: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity((x1 - x0) * (y1 - y0));
    for y in y0..y1 {
        out.extend_from_slice(&gray.data[y * gray.width + x0..y * gray.width + x1]);
    }
    out
}

/// `top_letters`: the first cell may show `A`-`F`; every other cell is 0-9.
fn read_digits(
    gray: &Image,
    field: &Rect,
    digit_count: usize,
    tset: &TemplateSet,
    top_letters: bool,
) -> DigitReading {
    let dw = field.w / digit_count as f64;
    let mut per_digit: Vec<(Option<&'static str>, f32)> = Vec::with_capacity(digit_count);
    let mut value: i64 = 0;
    let mut scores: Vec<f32> = Vec::new();
    let mut any_digit = false;

    for i in 0..digit_count {
        let cell = Rect::new(field.x + i as f64 * dw, field.y, dw, field.h);
        let (x0, y0, x1, y1) = cell.to_bounds();
        let (x1, y1) = (x1.min(gray.width), y1.min(gray.height));
        if x1 <= x0 || y1 <= y0 {
            per_digit.push((None, 0.0));
            continue;
        }
        let patch = crop_gray(gray, x0, y0, x1, y1);
        let mean = patch.iter().map(|&v| v as f64).sum::<f64>() / patch.len() as f64;
        if mean < BLANK_LUMA {
            per_digit.push((None, 1.0));
            continue;
        }

        let allowed: &[&str] = if i == 0 && top_letters {
            &HEX_LABELS
        } else {
            &DEC_LABELS
        };
        let result = match_recentered(gray, &cell, tset, allowed);
        per_digit.push((result.label, result.score));
        let acceptable = result.label.map(|l| allowed.contains(&l)).unwrap_or(false)
            && result.score >= MIN_DIGIT_SCORE
            && result.margin >= MIN_MARGIN;
        if acceptable {
            value = value * 10 + digit_value(result.label.unwrap());
            scores.push(result.score);
            any_digit = true;
        } else {
            return DigitReading::unreadable(per_digit);
        }
    }

    if !any_digit {
        // All cells blank -> unreadable, NOT zero (vanilla ROM renders
        // leading zeros; no ink means occlusion/blanking).
        return DigitReading::unreadable(per_digit);
    }
    let confidence = scores.iter().sum::<f32>() / scores.len() as f32;
    DigitReading {
        value: Some(value),
        confidence,
        digits: per_digit,
    }
}

fn match_recentered(
    gray: &Image,
    cell: &Rect,
    tset: &TemplateSet,
    allowed: &[&str],
) -> MatchResult {
    let (h, w) = (gray.height as i64, gray.width as i64);
    let mut patches: Vec<Vec<u8>> = Vec::new();
    for dy in -RECENTER_RADIUS..=RECENTER_RADIUS {
        for dx in -RECENTER_RADIUS..=RECENTER_RADIUS {
            let shifted = Rect::new(cell.x + dx as f64, cell.y + dy as f64, cell.w, cell.h);
            // Mirror the Python out-of-frame test, which checks the *unclamped*
            // rounded bounds before slicing.
            let sx0 = shifted.x.round_ties_even() as i64;
            let sy0 = shifted.y.round_ties_even() as i64;
            let sx1 = shifted.x2().round_ties_even() as i64;
            let sy1 = shifted.y2().round_ties_even() as i64;
            if sx0 < 0 || sy0 < 0 || sx1 > w || sy1 > h {
                continue;
            }
            if sx1 > sx0 && sy1 > sy0 {
                patches.push(crop_gray(
                    gray,
                    sx0 as usize,
                    sy0 as usize,
                    sx1 as usize,
                    sy1 as usize,
                ));
            }
        }
    }
    if patches.is_empty() {
        let (x0, y0, x1, y1) = cell.to_bounds();
        let (x1, y1) = (x1.min(gray.width), y1.min(gray.height));
        patches.push(crop_gray(gray, x0, y0, x1, y1));
    }
    let refs: Vec<&[u8]> = patches.iter().map(|p| p.as_slice()).collect();
    tset.match_best_of_gray(&refs, Some(allowed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::get_layout;
    use crate::templates::digit_glyph;

    /// A black 256x240 luma frame showing `text` in the SCORE field.
    fn frame_with_score(text: &str) -> Image {
        let mut gray = Image::new(256, 240, 1);
        let field = get_layout().score;
        for (i, c) in text.chars().enumerate() {
            let glyph = digit_glyph(&c.to_string());
            let (x0, y0) = (field.x as usize + i * 8, field.y as usize);
            for y in 0..glyph.height {
                for x in 0..glyph.width {
                    gray.data[(y0 + y) * gray.width + x0 + x] = glyph.data[y * glyph.width + x];
                }
            }
        }
        gray
    }

    fn read(text: &str, base: BaseMode) -> Option<i64> {
        let layout = get_layout();
        DigitReader::new(base)
            .read_field(&frame_with_score(text), &layout.score, 6, None)
            .value
    }

    #[test]
    fn plain_digits_are_decimal_in_every_mode() {
        for base in [BaseMode::Dec, BaseMode::Hex, BaseMode::Auto] {
            assert_eq!(read("025782", base), Some(25_782), "{base:?}");
        }
    }

    /// 025799 -> 025800 is one point, not the +103 of a base-16 reading.
    #[test]
    fn a_step_across_a_carry_is_one_point() {
        let (a, b) = (read("025799", BaseMode::Hex), read("025800", BaseMode::Hex));
        assert_eq!(b.unwrap() - a.unwrap(), 1);
    }

    #[test]
    fn a_top_letter_counts_as_its_digit_value() {
        assert_eq!(read("A00000", BaseMode::Hex), Some(1_000_000));
        assert_eq!(read("B23456", BaseMode::Hex), Some(1_123_456));
        assert_eq!(read("F99999", BaseMode::Hex), Some(1_599_999));
    }

    #[test]
    fn dec_never_reads_a_letter() {
        assert_ne!(read("A00000", BaseMode::Dec), Some(1_000_000));
    }

    #[test]
    fn letters_below_the_top_digit_are_never_read() {
        assert_ne!(read("1B3456", BaseMode::Hex), Some(0x1B_3456));
    }
}

//! Numeric field recognition (port of `recognition/digits.py`).

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

/// Numeric base mode for a field read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BaseMode {
    Dec,
    Hex,
    Auto,
}

/// Result of reading one numeric field.
#[derive(Clone, Debug)]
pub struct DigitReading {
    pub value: Option<i64>,
    pub confidence: f32,
    /// Per-cell (label, score) diagnostics.
    pub digits: Vec<(Option<&'static str>, f32)>,
    pub base: u32,
}

impl DigitReading {
    fn unreadable(base: u32, digits: Vec<(Option<&'static str>, f32)>) -> Self {
        DigitReading {
            value: None,
            confidence: 0.0,
            digits,
            base,
        }
    }
}

/// Latches the SCORE base after consistent auto reads (ROM property).
pub struct ScoreBaseLatch {
    latch_frames: u32,
    latched: Option<BaseMode>,
    streak_base: Option<u32>,
    streak: u32,
}

impl ScoreBaseLatch {
    pub fn new(latch_frames: u32) -> Self {
        Self {
            latch_frames,
            latched: None,
            streak_base: None,
            streak: 0,
        }
    }

    pub fn base(&self) -> Option<BaseMode> {
        self.latched
    }

    pub fn reset(&mut self) {
        self.latched = None;
        self.streak_base = None;
        self.streak = 0;
    }

    pub fn observe(&mut self, reading: &DigitReading) {
        if self.latch_frames == 0 {
            return;
        }
        if reading.value.is_none() {
            self.streak_base = None;
            self.streak = 0;
            return;
        }
        if Some(reading.base) == self.streak_base {
            self.streak += 1;
        } else {
            self.streak_base = Some(reading.base);
            self.streak = 1;
        }
        if self.streak >= self.latch_frames {
            self.latched = Some(if self.streak_base == Some(10) {
                BaseMode::Dec
            } else {
                BaseMode::Hex
            });
        }
    }

    pub fn relatch(&mut self, base: BaseMode) {
        self.latched = Some(base);
        self.streak_base = Some(if base == BaseMode::Dec { 10 } else { 16 });
        self.streak = self.latch_frames;
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
        let tset = digit_templates();
        match base.unwrap_or(self.default_base) {
            BaseMode::Auto => {
                let dec = read_base(gray, field, digit_count, tset, 10, &DEC_LABELS);
                let hexr = read_base(gray, field, digit_count, tset, 16, &HEX_LABELS);
                if hexr.value.is_some()
                    && (dec.value.is_none() || hexr.confidence > dec.confidence + 1e-6)
                {
                    hexr
                } else {
                    dec
                }
            }
            BaseMode::Hex => read_base(gray, field, digit_count, tset, 16, &HEX_LABELS),
            BaseMode::Dec => read_base(gray, field, digit_count, tset, 10, &DEC_LABELS),
        }
    }

    /// Auto-base read steered by a [`ScoreBaseLatch`].
    pub fn read_field_latched(
        &self,
        latch: &mut ScoreBaseLatch,
        gray: &Image,
        field: &Rect,
        digit_count: usize,
    ) -> DigitReading {
        if let Some(latched) = latch.base() {
            let reading = self.read_field(gray, field, digit_count, Some(latched));
            if reading.value.is_some() {
                return reading;
            }
            let other = if latched == BaseMode::Dec {
                BaseMode::Hex
            } else {
                BaseMode::Dec
            };
            let alt = self.read_field(gray, field, digit_count, Some(other));
            if alt.value.is_some() {
                latch.relatch(other);
                return alt;
            }
            return reading;
        }
        let reading = self.read_field(gray, field, digit_count, Some(BaseMode::Auto));
        latch.observe(&reading);
        reading
    }
}

fn crop_gray(gray: &Image, x0: usize, y0: usize, x1: usize, y1: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity((x1 - x0) * (y1 - y0));
    for y in y0..y1 {
        out.extend_from_slice(&gray.data[y * gray.width + x0..y * gray.width + x1]);
    }
    out
}

fn read_base(
    gray: &Image,
    field: &Rect,
    digit_count: usize,
    tset: &TemplateSet,
    base: u32,
    allowed: &[&str],
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

        let result = match_recentered(gray, &cell, tset, allowed);
        per_digit.push((result.label, result.score));
        let acceptable = result.label.map(|l| allowed.contains(&l)).unwrap_or(false)
            && result.score >= MIN_DIGIT_SCORE
            && result.margin >= MIN_MARGIN;
        if acceptable {
            value = value * base as i64 + digit_value(result.label.unwrap());
            scores.push(result.score);
            any_digit = true;
        } else {
            return DigitReading::unreadable(base, per_digit);
        }
    }

    if !any_digit {
        // All cells blank -> unreadable, NOT zero (vanilla ROM renders
        // leading zeros; no ink means occlusion/blanking).
        return DigitReading::unreadable(base, per_digit);
    }
    let confidence = scores.iter().sum::<f32>() / scores.len() as f32;
    DigitReading {
        value: Some(value),
        confidence,
        digits: per_digit,
        base,
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

//! Embedded glyph templates + the normalized-correlation matcher
//! (port of `recognition/templates.py`).
//!
//! The 28 template PNGs are compiled into the binary via `include_bytes!`, so
//! native, wasm, and Android builds all carry them without filesystem access.
//! Matching is the zero-mean/unit-norm matmul formulation of TM_CCOEFF_NORMED
//! in f32, mirroring the Python `TemplateSet` numerics.

use std::sync::OnceLock;

use nestris_vision::Image;

/// One match outcome: best label + [0,1]-mapped score + margin to runner-up.
#[derive(Clone, Debug, PartialEq)]
pub struct MatchResult {
    pub label: Option<&'static str>,
    pub score: f32,
    pub margin: f32,
}

impl MatchResult {
    fn none() -> Self {
        MatchResult {
            label: None,
            score: 0.0,
            margin: 0.0,
        }
    }
}

/// Decode an embedded grayscale PNG (an RGB file collapses to channel 0).
pub(crate) fn decode_png_gray(bytes: &[u8]) -> Image {
    decode_png(bytes)
}

fn decode_png(bytes: &[u8]) -> Image {
    let decoder = png::Decoder::new(bytes);
    let mut reader = decoder.read_info().expect("embedded template png");
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .expect("embedded template frame");
    buf.truncate(info.buffer_size());
    let channels = match info.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::Rgb => 3,
        other => panic!("template png must be gray/rgb, got {other:?}"),
    };
    let img = Image::from_vec(buf, info.width as usize, info.height as usize, channels);
    if channels == 3 {
        // Grayscale template accidentally stored as RGB; collapse channel 0.
        let mut gray = Image::new(img.width, img.height, 1);
        for (dst, px) in gray.data.iter_mut().zip(img.data.chunks_exact(3)) {
            *dst = px[0];
        }
        gray
    } else {
        img
    }
}

macro_rules! embedded {
    ($($label:literal => $path:literal),+ $(,)?) => {
        &[$(($label, include_bytes!($path) as &[u8])),+]
    };
}

const DIGIT_PNGS: &[(&str, &[u8])] = embedded![
    "0" => "../assets/digits/0.png",
    "1" => "../assets/digits/1.png",
    "2" => "../assets/digits/2.png",
    "3" => "../assets/digits/3.png",
    "4" => "../assets/digits/4.png",
    "5" => "../assets/digits/5.png",
    "6" => "../assets/digits/6.png",
    "7" => "../assets/digits/7.png",
    "8" => "../assets/digits/8.png",
    "9" => "../assets/digits/9.png",
    "A" => "../assets/digits/A.png",
    "B" => "../assets/digits/B.png",
    "C" => "../assets/digits/C.png",
    "D" => "../assets/digits/D.png",
    "E" => "../assets/digits/E.png",
    "F" => "../assets/digits/F.png",
];

const PIECE_PNGS: &[(&str, &[u8])] = embedded![
    "I" => "../assets/pieces/I.png",
    "J" => "../assets/pieces/J.png",
    "L" => "../assets/pieces/L.png",
    "O" => "../assets/pieces/O.png",
    "S" => "../assets/pieces/S.png",
    "T" => "../assets/pieces/T.png",
    "Z" => "../assets/pieces/Z.png",
];

const LABEL_PNGS: &[(&str, &[u8])] = embedded![
    "LEVEL" => "../assets/labels/LEVEL.png",
    "LINES" => "../assets/labels/LINES.png",
    "NEXT" => "../assets/labels/NEXT.png",
    "SCORE" => "../assets/labels/SCORE.png",
    "STATISTICS" => "../assets/labels/STATISTICS.png",
];

/// Uniform-size template stack matched by normalized correlation.
pub struct TemplateSet {
    labels: Vec<&'static str>,
    /// (n_templates, d) zero-mean unit-norm rows, f32.
    matrix: Vec<Vec<f32>>,
    pub height: usize,
    pub width: usize,
}

fn normalize_row(row: &mut [f32]) {
    let n = row.len() as f32;
    let mean = row.iter().sum::<f32>() / n;
    let mut norm2 = 0.0f32;
    for v in row.iter_mut() {
        *v -= mean;
        norm2 += *v * *v;
    }
    let norm = norm2.sqrt();
    if norm > 1e-6 {
        for v in row.iter_mut() {
            *v /= norm;
        }
    } else {
        row.fill(0.0);
    }
}

impl TemplateSet {
    fn from_pngs(pngs: &[(&'static str, &[u8])]) -> TemplateSet {
        let images: Vec<(&'static str, Image)> =
            pngs.iter().map(|(l, b)| (*l, decode_png(b))).collect();
        let (h, w) = (images[0].1.height, images[0].1.width);
        let mut labels = Vec::new();
        let mut matrix = Vec::new();
        for (label, img) in &images {
            assert_eq!(
                (img.height, img.width),
                (h, w),
                "template {label} size mismatch"
            );
            let mut row: Vec<f32> = img.data.iter().map(|&v| v as f32).collect();
            normalize_row(&mut row);
            labels.push(*label);
            matrix.push(row);
        }
        TemplateSet {
            labels,
            matrix,
            height: h,
            width: w,
        }
    }

    pub fn labels(&self) -> &[&'static str] {
        &self.labels
    }

    /// Match a stack of equal-size grayscale crops (each `height*width` u8);
    /// returns the best (label, offset) result, mirroring Python's
    /// `match_best_of_gray` (ties resolve like `np.argsort(...)[::-1]`, i.e.
    /// the later label wins an exact tie).
    pub fn match_best_of_gray(&self, patches: &[&[u8]], allowed: Option<&[&str]>) -> MatchResult {
        if patches.is_empty() {
            return MatchResult::none();
        }
        let keep: Vec<usize> = match allowed {
            None => (0..self.labels.len()).collect(),
            Some(set) => (0..self.labels.len())
                .filter(|&i| set.contains(&self.labels[i]))
                .collect(),
        };
        if keep.is_empty() {
            return MatchResult::none();
        }
        let d = self.height * self.width;
        let mut best_offset = 0usize;
        let mut best_offset_max = f32::NEG_INFINITY;
        let mut per_offset_scores: Vec<Vec<f32>> = Vec::with_capacity(patches.len());
        for patch in patches {
            assert_eq!(patch.len(), d, "patch size mismatch");
            let mut vec: Vec<f32> = patch.iter().map(|&v| v as f32).collect();
            normalize_row(&mut vec);
            let scores: Vec<f32> = keep
                .iter()
                .map(|&t| {
                    self.matrix[t]
                        .iter()
                        .zip(vec.iter())
                        .map(|(a, b)| a * b)
                        .sum::<f32>()
                })
                .collect();
            let m = scores.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
            per_offset_scores.push(scores);
            // np.argmax keeps the FIRST maximal offset.
            if m > best_offset_max {
                best_offset_max = m;
                best_offset = per_offset_scores.len() - 1;
            }
        }
        let scores = &per_offset_scores[best_offset];
        // Python's `np.argsort(scores)[::-1]` tie rule: among equal scores the
        // later index wins, hence `>=` while scanning forward.
        let mut best_i = 0usize;
        for (i, &s) in scores.iter().enumerate().skip(1) {
            if s >= scores[best_i] {
                best_i = i;
            }
        }
        let mut second = f32::NEG_INFINITY;
        for (i, &s) in scores.iter().enumerate() {
            if i != best_i {
                second = second.max(s);
            }
        }
        let best = scores[best_i];
        let best01 = (best + 1.0) / 2.0;
        let second01 = if second > -1.0 && second.is_finite() {
            (second + 1.0) / 2.0
        } else {
            0.0
        };
        MatchResult {
            label: Some(self.labels[keep[best_i]]),
            score: best01,
            margin: (best01 - second01).max(0.0),
        }
    }
}

/// The digit template set (0-9 A-F, 8×8).
pub fn digit_templates() -> &'static TemplateSet {
    static SET: OnceLock<TemplateSet> = OnceLock::new();
    SET.get_or_init(|| TemplateSet::from_pngs(DIGIT_PNGS))
}

/// The piece template set (unused by the shape-based readers, kept for parity).
pub fn piece_templates() -> &'static TemplateSet {
    static SET: OnceLock<TemplateSet> = OnceLock::new();
    SET.get_or_init(|| TemplateSet::from_pngs(PIECE_PNGS))
}

/// Raw (non-uniform) HUD label templates for anchor matching (Phase 5).
pub fn label_templates() -> &'static Vec<(&'static str, Image)> {
    static SET: OnceLock<Vec<(&'static str, Image)>> = OnceLock::new();
    SET.get_or_init(|| {
        LABEL_PNGS
            .iter()
            .map(|(l, b)| (*l, decode_png(b)))
            .collect()
    })
}

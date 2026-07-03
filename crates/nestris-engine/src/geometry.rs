//! Rect/Quad value types mirroring the Python `core/geometry.py` semantics,
//! including the banker's rounding of `Rect.to_slices` (Python `round`).

/// Axis-aligned rectangle in (possibly sub-pixel) pixel coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    pub fn x2(&self) -> f64 {
        self.x + self.w
    }

    pub fn y2(&self) -> f64 {
        self.y + self.h
    }

    /// Integer crop bounds `(x0, y0, x1, y1)` exactly like the Python
    /// `Rect.to_slices`: round-half-to-even, clamped at zero, end >= start.
    pub fn to_bounds(&self) -> (usize, usize, usize, usize) {
        let x0 = self.x.round_ties_even().max(0.0) as usize;
        let y0 = self.y.round_ties_even().max(0.0) as usize;
        let x1 = (self.x2().round_ties_even() as isize).max(x0 as isize) as usize;
        let y1 = (self.y2().round_ties_even() as isize).max(y0 as isize) as usize;
        (x0, y0, x1, y1)
    }

    pub fn scaled(&self, sx: f64, sy: f64) -> Rect {
        Rect::new(self.x * sx, self.y * sy, self.w * sx, self.h * sy)
    }
}

/// Four corners of a quadrilateral, clockwise from top-left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quad {
    pub tl: (f64, f64),
    pub tr: (f64, f64),
    pub br: (f64, f64),
    pub bl: (f64, f64),
}

impl Quad {
    pub fn from_rect(r: &Rect) -> Quad {
        Quad {
            tl: (r.x, r.y),
            tr: (r.x2(), r.y),
            br: (r.x2(), r.y2()),
            bl: (r.x, r.y2()),
        }
    }

    pub fn corners(&self) -> [(f64, f64); 4] {
        [self.tl, self.tr, self.br, self.bl]
    }
}

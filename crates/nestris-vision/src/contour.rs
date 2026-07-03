//! Rotated minimum-area rectangle (`cv2.minAreaRect` + `cv2.boxPoints`)
//! computed as convex hull (Andrew monotone chain) + rotating calipers over
//! hull edges — contour tracing is unnecessary because callers pass component
//! point sets. Decision-level equivalence with OpenCV: same center/size/box
//! within 0.5 px (the angle convention is normalized by the caller-facing
//! `box_points` corners, which is all the engine consumes).

/// A rotated rectangle: center, size, angle in degrees (edge direction of
/// the `width` side, OpenCV-style).
#[derive(Clone, Copy, Debug)]
pub struct RotatedRect {
    pub cx: f64,
    pub cy: f64,
    pub width: f64,
    pub height: f64,
    pub angle_deg: f64,
}

impl RotatedRect {
    /// The 4 corners (like `cv2.boxPoints`, up to cyclic order).
    pub fn box_points(&self) -> [(f64, f64); 4] {
        let a = self.angle_deg.to_radians();
        let (s, c) = a.sin_cos();
        let (hw, hh) = (self.width / 2.0, self.height / 2.0);
        let corners = [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)];
        corners.map(|(x, y)| (self.cx + x * c - y * s, self.cy + x * s + y * c))
    }
}

/// Andrew monotone-chain convex hull; returns hull points in CCW order
/// (y-down image coordinates make this visually clockwise).
pub fn convex_hull(points: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut pts: Vec<(f64, f64)> = points.to_vec();
    pts.sort_by(|a, b| a.partial_cmp(b).unwrap());
    pts.dedup();
    if pts.len() <= 2 {
        return pts;
    }
    let cross = |o: (f64, f64), a: (f64, f64), b: (f64, f64)| {
        (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
    };
    let mut hull: Vec<(f64, f64)> = Vec::with_capacity(pts.len() * 2);
    for &p in pts.iter().chain(pts.iter().rev().skip(1)) {
        while hull.len() >= 2 && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0.0 {
            hull.pop();
        }
        hull.push(p);
        if hull.len() > 1 && hull[0] == p && hull.len() > pts.len() {
            break;
        }
    }
    hull.pop(); // last point == first point
    hull
}

/// `cv2.minAreaRect(points)`: the minimum-area enclosing rotated rectangle.
pub fn min_area_rect(points: &[(f64, f64)]) -> Option<RotatedRect> {
    if points.is_empty() {
        return None;
    }
    let hull = convex_hull(points);
    if hull.len() == 1 {
        return Some(RotatedRect {
            cx: hull[0].0,
            cy: hull[0].1,
            width: 0.0,
            height: 0.0,
            angle_deg: 0.0,
        });
    }
    if hull.len() == 2 {
        let (a, b) = (hull[0], hull[1]);
        let dx = b.0 - a.0;
        let dy = b.1 - a.1;
        return Some(RotatedRect {
            cx: (a.0 + b.0) / 2.0,
            cy: (a.1 + b.1) / 2.0,
            width: dx.hypot(dy),
            height: 0.0,
            angle_deg: dy.atan2(dx).to_degrees(),
        });
    }
    let n = hull.len();
    let mut best: Option<(f64, RotatedRect)> = None;
    for i in 0..n {
        let (ax, ay) = hull[i];
        let (bx, by) = hull[(i + 1) % n];
        let (mut ex, mut ey) = (bx - ax, by - ay);
        let len = ex.hypot(ey);
        if len < 1e-12 {
            continue;
        }
        ex /= len;
        ey /= len;
        // Project the hull onto the edge direction and its normal.
        let (mut min_u, mut max_u) = (f64::INFINITY, f64::NEG_INFINITY);
        let (mut min_v, mut max_v) = (f64::INFINITY, f64::NEG_INFINITY);
        for &(px, py) in &hull {
            let u = (px - ax) * ex + (py - ay) * ey;
            let v = -(px - ax) * ey + (py - ay) * ex;
            min_u = min_u.min(u);
            max_u = max_u.max(u);
            min_v = min_v.min(v);
            max_v = max_v.max(v);
        }
        let w = max_u - min_u;
        let h = max_v - min_v;
        let area = w * h;
        if best.as_ref().is_none_or(|(ba, _)| area < *ba) {
            let cu = (min_u + max_u) / 2.0;
            let cv = (min_v + max_v) / 2.0;
            let cx = ax + cu * ex - cv * ey;
            let cy = ay + cu * ey + cv * ex;
            best = Some((
                area,
                RotatedRect {
                    cx,
                    cy,
                    width: w,
                    height: h,
                    angle_deg: ey.atan2(ex).to_degrees(),
                },
            ));
        }
    }
    best.map(|(_, r)| r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_aligned_rect() {
        let pts: Vec<(f64, f64)> = (10..=30)
            .flat_map(|x| (5..=15).map(move |y| (x as f64, y as f64)))
            .collect();
        let r = min_area_rect(&pts).unwrap();
        assert!((r.cx - 20.0).abs() < 1e-9);
        assert!((r.cy - 10.0).abs() < 1e-9);
        let (long, short) = (r.width.max(r.height), r.width.min(r.height));
        assert!((long - 20.0).abs() < 1e-9 && (short - 10.0).abs() < 1e-9);
    }
}

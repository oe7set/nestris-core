//! Probabilistic Hough line segments, mirroring `cv2.HoughLinesP`'s
//! algorithm with our deterministic RNG instead of `cv::theRNG`.
//! Cross-language equivalence is decision-level only (the undistort bow
//! metric and the resulting k1 choice), never per-segment.

use crate::image::Image;
use crate::rng::Pcg32;

/// A detected line segment `(x1, y1, x2, y2)`.
pub type Segment = (i32, i32, i32, i32);

/// `cv2.HoughLinesP(edges, rho=1, theta, threshold, minLineLength, maxLineGap)`.
pub fn hough_lines_p(
    edges: &Image,
    theta_step: f64,
    threshold: i32,
    min_line_length: f64,
    max_line_gap: i32,
    rng: &mut Pcg32,
) -> Vec<Segment> {
    assert_eq!(edges.channels, 1);
    let (w, h) = (edges.width as i32, edges.height as i32);
    let num_angle = (std::f64::consts::PI / theta_step).round() as usize;
    let num_rho = ((w + h) * 2 + 1) as usize;
    let mut accum = vec![0i32; num_angle * num_rho];
    let mut mask: Vec<u8> = edges.data.iter().map(|&v| u8::from(v != 0)).collect();
    let mut points: Vec<(i32, i32)> = Vec::new();
    for y in 0..h {
        for x in 0..w {
            if mask[(y * w + x) as usize] != 0 {
                points.push((x, y));
            }
        }
    }
    let trig: Vec<(f64, f64)> = (0..num_angle)
        .map(|a| {
            let ang = a as f64 * theta_step;
            (ang.cos(), ang.sin())
        })
        .collect();
    let mut segments = Vec::new();

    while !points.is_empty() {
        // Pick a random remaining point (OpenCV: rng.uniform(0, count)).
        let pick = rng.next_below(points.len() as u32) as usize;
        let (px, py) = points.swap_remove(pick);
        if mask[(py * w + px) as usize] == 0 {
            continue; // already absorbed into a segment
        }
        // Vote and find the best angle for this point.
        let mut best_a = 0usize;
        let mut best_votes = threshold - 1;
        for (a, &(c, s)) in trig.iter().enumerate() {
            let r = (px as f64 * c + py as f64 * s).round() as i32 + (w + h);
            let cell = &mut accum[a * num_rho + r as usize];
            *cell += 1;
            if *cell > best_votes {
                best_votes = *cell;
                best_a = a;
            }
        }
        if best_votes < threshold {
            continue;
        }
        // Walk the line in both directions gathering the longest run with
        // gaps <= max_line_gap.
        let (c, s) = trig[best_a];
        let (dx, dy) = if s.abs() > c.abs() {
            // Mostly-horizontal walk.
            (1.0f64, -c / s)
        } else {
            (-s / c, 1.0f64)
        };
        let mut ends = [(px, py), (px, py)];
        for (dir, end) in [(1.0f64, 0usize), (-1.0, 1)] {
            let (mut fx, mut fy) = (px as f64, py as f64);
            let mut gap = 0;
            loop {
                fx += dx * dir;
                fy += dy * dir;
                let ix = fx.round() as i32;
                let iy = fy.round() as i32;
                if ix < 0 || iy < 0 || ix >= w || iy >= h {
                    break;
                }
                if mask[(iy * w + ix) as usize] != 0 {
                    gap = 0;
                    ends[end] = (ix, iy);
                } else {
                    gap += 1;
                    if gap > max_line_gap {
                        break;
                    }
                }
            }
        }
        let len = (((ends[0].0 - ends[1].0).pow(2) + (ends[0].1 - ends[1].1).pow(2)) as f64).sqrt();
        let good = len >= min_line_length;
        // Erase the segment's points from the mask and un-vote them.
        for (dir, stop) in [(1.0f64, ends[0]), (-1.0, ends[1])] {
            let (mut fx, mut fy) = (px as f64, py as f64);
            loop {
                let ix = fx.round() as i32;
                let iy = fy.round() as i32;
                if mask[(iy * w + ix) as usize] != 0 {
                    mask[(iy * w + ix) as usize] = 0;
                    if good {
                        for (a, &(ca, sa)) in trig.iter().enumerate() {
                            let r = (ix as f64 * ca + iy as f64 * sa).round() as i32 + (w + h);
                            accum[a * num_rho + r as usize] -= 1;
                        }
                    }
                }
                if (ix, iy) == stop {
                    break;
                }
                fx += dx * dir;
                fy += dy * dir;
            }
        }
        if good {
            segments.push((ends[0].0, ends[0].1, ends[1].0, ends[1].1));
        }
    }
    segments
}

//! Homography estimation replicating `cv2.getPerspectiveTransform` and
//! `cv2.findHomography(..., RANSAC)`.
//!
//! The RANSAC loop draws from the caller-supplied deterministic [`Pcg32`], so
//! Rust runs are bit-identical; equivalence with OpenCV (whose RANSAC has its
//! own RNG) is asserted at the outcome level by the goldens: same inlier set
//! on separable data, H within 1e-3 relative, mean inlier reprojection within
//! 0.1 px.

use crate::rng::Pcg32;

/// Row-major 3×3 matrix.
pub type Mat3 = [f64; 9];

/// Multiply two 3×3 matrices.
pub fn mat3_mul(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut out = [0.0; 9];
    for r in 0..3 {
        for c in 0..3 {
            out[r * 3 + c] = (0..3).map(|k| a[r * 3 + k] * b[k * 3 + c]).sum();
        }
    }
    out
}

/// Inverse of a 3×3 matrix via the adjugate; `None` when singular.
pub fn mat3_inv(m: &Mat3) -> Option<Mat3> {
    let det = m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6])
        + m[2] * (m[3] * m[7] - m[4] * m[6]);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv_det = 1.0 / det;
    Some([
        (m[4] * m[8] - m[5] * m[7]) * inv_det,
        (m[2] * m[7] - m[1] * m[8]) * inv_det,
        (m[1] * m[5] - m[2] * m[4]) * inv_det,
        (m[5] * m[6] - m[3] * m[8]) * inv_det,
        (m[0] * m[8] - m[2] * m[6]) * inv_det,
        (m[2] * m[3] - m[0] * m[5]) * inv_det,
        (m[3] * m[7] - m[4] * m[6]) * inv_det,
        (m[1] * m[6] - m[0] * m[7]) * inv_det,
        (m[0] * m[4] - m[1] * m[3]) * inv_det,
    ])
}

/// `cv2.perspectiveTransform` for one point.
#[inline]
pub fn project(h: &Mat3, x: f64, y: f64) -> (f64, f64) {
    let w = h[6] * x + h[7] * y + h[8];
    let iw = if w.abs() > f64::MIN_POSITIVE {
        1.0 / w
    } else {
        0.0
    };
    (
        (h[0] * x + h[1] * y + h[2]) * iw,
        (h[3] * x + h[4] * y + h[5]) * iw,
    )
}

/// Solve `A x = b` for a dense square system with partial-pivot Gaussian
/// elimination. `a` is row-major n×n, consumed in place.
fn solve_dense(a: &mut [f64], b: &mut [f64], n: usize) -> bool {
    for col in 0..n {
        let mut pivot = col;
        for row in (col + 1)..n {
            if a[row * n + col].abs() > a[pivot * n + col].abs() {
                pivot = row;
            }
        }
        if a[pivot * n + col].abs() < 1e-12 {
            return false;
        }
        if pivot != col {
            for k in 0..n {
                a.swap(col * n + k, pivot * n + k);
            }
            b.swap(col, pivot);
        }
        let inv = 1.0 / a[col * n + col];
        for row in (col + 1)..n {
            let f = a[row * n + col] * inv;
            if f == 0.0 {
                continue;
            }
            for k in col..n {
                a[row * n + k] -= f * a[col * n + k];
            }
            b[row] -= f * b[col];
        }
    }
    for col in (0..n).rev() {
        let mut v = b[col];
        for k in (col + 1)..n {
            v -= a[col * n + k] * b[k];
        }
        b[col] = v / a[col * n + col];
    }
    true
}

/// `cv2.getPerspectiveTransform`: exact homography from 4 correspondences.
pub fn perspective_transform_4(src: &[(f64, f64); 4], dst: &[(f64, f64); 4]) -> Option<Mat3> {
    let mut a = [0.0f64; 64];
    let mut b = [0.0f64; 8];
    for i in 0..4 {
        let (sx, sy) = src[i];
        let (dx, dy) = dst[i];
        let r0 = i * 2 * 8;
        a[r0] = sx;
        a[r0 + 1] = sy;
        a[r0 + 2] = 1.0;
        a[r0 + 6] = -sx * dx;
        a[r0 + 7] = -sy * dx;
        b[i * 2] = dx;
        let r1 = (i * 2 + 1) * 8;
        a[r1 + 3] = sx;
        a[r1 + 4] = sy;
        a[r1 + 5] = 1.0;
        a[r1 + 6] = -sx * dy;
        a[r1 + 7] = -sy * dy;
        b[i * 2 + 1] = dy;
    }
    if !solve_dense(&mut a, &mut b, 8) {
        return None;
    }
    Some([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], 1.0])
}

/// Jacobi eigenvalue decomposition of a symmetric 9×9 matrix; returns the
/// eigenvector of the smallest eigenvalue.
fn smallest_eigenvector_9(mut m: [f64; 81]) -> [f64; 9] {
    const N: usize = 9;
    let mut v = [0.0f64; 81];
    for i in 0..N {
        v[i * N + i] = 1.0;
    }
    for _sweep in 0..64 {
        let mut off = 0.0;
        for r in 0..N {
            for c in (r + 1)..N {
                off += m[r * N + c] * m[r * N + c];
            }
        }
        if off < 1e-24 {
            break;
        }
        for p in 0..N {
            for q in (p + 1)..N {
                let apq = m[p * N + q];
                if apq.abs() < 1e-30 {
                    continue;
                }
                let app = m[p * N + p];
                let aqq = m[q * N + q];
                let theta = (aqq - app) / (2.0 * apq);
                let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;
                for k in 0..N {
                    let mkp = m[k * N + p];
                    let mkq = m[k * N + q];
                    m[k * N + p] = c * mkp - s * mkq;
                    m[k * N + q] = s * mkp + c * mkq;
                }
                for k in 0..N {
                    let mpk = m[p * N + k];
                    let mqk = m[q * N + k];
                    m[p * N + k] = c * mpk - s * mqk;
                    m[q * N + k] = s * mpk + c * mqk;
                }
                for k in 0..N {
                    let vkp = v[k * N + p];
                    let vkq = v[k * N + q];
                    v[k * N + p] = c * vkp - s * vkq;
                    v[k * N + q] = s * vkp + c * vkq;
                }
            }
        }
    }
    let mut min_i = 0;
    for i in 1..N {
        if m[i * N + i] < m[min_i * N + min_i] {
            min_i = i;
        }
    }
    let mut out = [0.0f64; 9];
    for k in 0..N {
        out[k] = v[k * N + min_i];
    }
    out
}

/// Hartley-normalized DLT least squares over all correspondences (n ≥ 4).
pub fn dlt(src: &[(f64, f64)], dst: &[(f64, f64)]) -> Option<Mat3> {
    let n = src.len();
    if n < 4 || dst.len() != n {
        return None;
    }
    // Normalize both point sets: centroid to origin, mean distance sqrt(2).
    let norm = |pts: &[(f64, f64)]| -> (Mat3, Vec<(f64, f64)>) {
        let cx = pts.iter().map(|p| p.0).sum::<f64>() / n as f64;
        let cy = pts.iter().map(|p| p.1).sum::<f64>() / n as f64;
        let mean_d = pts
            .iter()
            .map(|p| ((p.0 - cx).powi(2) + (p.1 - cy).powi(2)).sqrt())
            .sum::<f64>()
            / n as f64;
        let s = if mean_d > 1e-12 {
            std::f64::consts::SQRT_2 / mean_d
        } else {
            1.0
        };
        let t: Mat3 = [s, 0.0, -s * cx, 0.0, s, -s * cy, 0.0, 0.0, 1.0];
        let mapped = pts
            .iter()
            .map(|p| (s * (p.0 - cx), s * (p.1 - cy)))
            .collect();
        (t, mapped)
    };
    let (t_src, ns) = norm(src);
    let (t_dst, nd) = norm(dst);

    // Accumulate A^T A (9×9) over the 2n DLT rows.
    let mut ata = [0.0f64; 81];
    for i in 0..n {
        let (x, y) = ns[i];
        let (u, v) = nd[i];
        let rows: [[f64; 9]; 2] = [
            [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, -u],
            [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, -v],
        ];
        for row in rows {
            for r in 0..9 {
                for c in r..9 {
                    ata[r * 9 + c] += row[r] * row[c];
                }
            }
        }
    }
    for r in 0..9 {
        for c in 0..r {
            ata[r * 9 + c] = ata[c * 9 + r];
        }
    }
    let h_norm = smallest_eigenvector_9(ata);
    // Denormalize: H = T_dst^-1 * Hn * T_src.
    let t_dst_inv = mat3_inv(&t_dst)?;
    let h = mat3_mul(&mat3_mul(&t_dst_inv, &h_norm_to_mat(&h_norm)), &t_src);
    if h[8].abs() < 1e-12 {
        return None;
    }
    Some(std::array::from_fn(|i| h[i] / h[8]))
}

#[inline]
fn h_norm_to_mat(v: &[f64; 9]) -> Mat3 {
    *v
}

/// Levenberg–Marquardt refinement of `h` (h22 pinned to 1) minimizing the
/// dst-space reprojection error, mirroring OpenCV's post-RANSAC refinement.
pub fn lm_refine(h: &mut Mat3, src: &[(f64, f64)], dst: &[(f64, f64)], iterations: usize) {
    let mut lambda = 1e-3;
    let mut err = reproj_sq_sum(h, src, dst);
    for _ in 0..iterations {
        // Build J^T J and J^T r for the 8 free parameters.
        let mut jtj = [0.0f64; 64];
        let mut jtr = [0.0f64; 8];
        for i in 0..src.len() {
            let (x, y) = src[i];
            let w = h[6] * x + h[7] * y + h[8];
            if w.abs() < 1e-12 {
                continue;
            }
            let iw = 1.0 / w;
            let (px, py) = (
                (h[0] * x + h[1] * y + h[2]) * iw,
                (h[3] * x + h[4] * y + h[5]) * iw,
            );
            let (rx, ry) = (px - dst[i].0, py - dst[i].1);
            // d(px)/d(params), d(py)/d(params) for params h0..h7.
            let jx = [
                x * iw,
                y * iw,
                iw,
                0.0,
                0.0,
                0.0,
                -px * x * iw,
                -px * y * iw,
            ];
            let jy = [
                0.0,
                0.0,
                0.0,
                x * iw,
                y * iw,
                iw,
                -py * x * iw,
                -py * y * iw,
            ];
            for r in 0..8 {
                for c in 0..8 {
                    jtj[r * 8 + c] += jx[r] * jx[c] + jy[r] * jy[c];
                }
                jtr[r] += jx[r] * rx + jy[r] * ry;
            }
        }
        let mut a = jtj;
        for d in 0..8 {
            a[d * 8 + d] *= 1.0 + lambda;
        }
        let mut delta = jtr;
        if !solve_dense(&mut a, &mut delta, 8) {
            break;
        }
        let mut candidate = *h;
        for (k, d) in delta.iter().enumerate() {
            candidate[k] -= d;
        }
        let new_err = reproj_sq_sum(&candidate, src, dst);
        if new_err < err {
            *h = candidate;
            err = new_err;
            lambda = (lambda * 0.1).max(1e-12);
        } else {
            lambda *= 10.0;
            if lambda > 1e8 {
                break;
            }
        }
    }
}

fn reproj_sq_sum(h: &Mat3, src: &[(f64, f64)], dst: &[(f64, f64)]) -> f64 {
    src.iter()
        .zip(dst.iter())
        .map(|(s, d)| {
            let (px, py) = project(h, s.0, s.1);
            (px - d.0).powi(2) + (py - d.1).powi(2)
        })
        .sum()
}

/// Result of [`find_homography_ransac`].
pub struct HomographyResult {
    pub h: Mat3,
    pub inliers: Vec<bool>,
    pub inlier_count: usize,
}

/// Whether any 3 of the 4 sampled points are (nearly) collinear — OpenCV's
/// degenerate-sample check.
fn sample_degenerate(pts: &[(f64, f64); 4]) -> bool {
    for i in 0..4 {
        let others: Vec<_> = (0..4).filter(|&j| j != i).map(|j| pts[j]).collect();
        let (ax, ay) = others[0];
        let (bx, by) = others[1];
        let (cx, cy) = others[2];
        let cross = (bx - ax) * (cy - ay) - (by - ay) * (cx - ax);
        if cross.abs() < 1e-9 * ((bx - ax).hypot(by - ay) * (cx - ax).hypot(cy - ay)).max(1.0) {
            return true;
        }
    }
    false
}

/// `cv2.findHomography(src, dst, RANSAC, threshold)` with deterministic RNG.
///
/// `max_iters`/`confidence` default to OpenCV's 2000 / 0.995 at call sites.
pub fn find_homography_ransac(
    src: &[(f64, f64)],
    dst: &[(f64, f64)],
    threshold: f64,
    rng: &mut Pcg32,
    max_iters: usize,
    confidence: f64,
) -> Option<HomographyResult> {
    let n = src.len();
    if n < 4 || dst.len() != n {
        return None;
    }
    let thresh_sq = threshold * threshold;
    let mut best_mask = vec![false; n];
    let mut best_count = 0usize;
    let mut niters = max_iters;
    let mut iter = 0usize;
    while iter < niters {
        iter += 1;
        // Sample 4 distinct, non-degenerate correspondences.
        let mut idx = [0usize; 4];
        let mut ok = false;
        for _attempt in 0..300 {
            for k in 0..4 {
                loop {
                    let cand = rng.next_below(n as u32) as usize;
                    if !idx[..k].contains(&cand) {
                        idx[k] = cand;
                        break;
                    }
                }
            }
            let s4 = [src[idx[0]], src[idx[1]], src[idx[2]], src[idx[3]]];
            let d4 = [dst[idx[0]], dst[idx[1]], dst[idx[2]], dst[idx[3]]];
            if !sample_degenerate(&s4) && !sample_degenerate(&d4) {
                ok = true;
                break;
            }
        }
        if !ok {
            break;
        }
        let s4 = [src[idx[0]], src[idx[1]], src[idx[2]], src[idx[3]]];
        let d4 = [dst[idx[0]], dst[idx[1]], dst[idx[2]], dst[idx[3]]];
        let Some(h) = perspective_transform_4(&s4, &d4) else {
            continue;
        };
        let mut mask = vec![false; n];
        let mut count = 0usize;
        for i in 0..n {
            let (px, py) = project(&h, src[i].0, src[i].1);
            let d = (px - dst[i].0).powi(2) + (py - dst[i].1).powi(2);
            if d <= thresh_sq {
                mask[i] = true;
                count += 1;
            }
        }
        if count > best_count {
            best_count = count;
            best_mask = mask;
            // OpenCV's RANSACUpdateNumIters: shrink the budget as the
            // observed inlier ratio improves.
            let ep = 1.0 - count as f64 / n as f64;
            let denom = 1.0 - (1.0 - ep).powi(4);
            if denom < f64::EPSILON {
                niters = iter;
            } else {
                let num = (1.0 - confidence).ln();
                let d = denom.ln();
                if d < 0.0 {
                    niters = niters.min((num / d).ceil() as usize);
                }
            }
        }
    }
    if best_count < 4 {
        return None;
    }
    let in_src: Vec<_> = (0..n).filter(|&i| best_mask[i]).map(|i| src[i]).collect();
    let in_dst: Vec<_> = (0..n).filter(|&i| best_mask[i]).map(|i| dst[i]).collect();
    let mut h = dlt(&in_src, &in_dst)?;
    lm_refine(&mut h, &in_src, &in_dst, 10);
    let h = std::array::from_fn(|i| h[i] / h[8]);
    Some(HomographyResult {
        h,
        inliers: best_mask,
        inlier_count: best_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact4_roundtrip() {
        let src = [(0.0, 0.0), (100.0, 5.0), (95.0, 110.0), (-3.0, 100.0)];
        let dst = [(10.0, 10.0), (200.0, 20.0), (190.0, 210.0), (5.0, 205.0)];
        let h = perspective_transform_4(&src, &dst).unwrap();
        for (s, d) in src.iter().zip(dst.iter()) {
            let (px, py) = project(&h, s.0, s.1);
            assert!((px - d.0).abs() < 1e-9 && (py - d.1).abs() < 1e-9);
        }
    }

    #[test]
    fn mat3_inverse_roundtrip() {
        let m: Mat3 = [2.0, 0.1, 3.0, 0.0, 1.5, -2.0, 0.001, 0.0, 1.0];
        let inv = mat3_inv(&m).unwrap();
        let prod = mat3_mul(&m, &inv);
        for (i, v) in prod.iter().enumerate() {
            let expected = if i % 4 == 0 { 1.0 } else { 0.0 };
            assert!((v - expected).abs() < 1e-9, "prod[{i}]={v}");
        }
    }
}

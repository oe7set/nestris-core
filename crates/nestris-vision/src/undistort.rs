//! Single-coefficient barrel undistortion maps, replicating
//! `cv2.initUndistortRectifyMap(K, [k1,0,0,0,0], None, K, size, CV_32FC1)`
//! for the synthetic pinhole the Python engine uses (f = max(w, h),
//! principal point at the frame center).

/// Precomputed remap tables (dst pixel → distorted source coordinates).
pub struct UndistortMap {
    pub map_x: Vec<f32>,
    pub map_y: Vec<f32>,
    pub k1: f64,
    pub width: usize,
    pub height: usize,
}

/// Build the undistortion map for coefficient `k1` at `(width, height)`.
pub fn build_map(width: usize, height: usize, k1: f64) -> UndistortMap {
    let f = width.max(height) as f64;
    let cx = width as f64 / 2.0;
    let cy = height as f64 / 2.0;
    let mut map_x = vec![0.0f32; width * height];
    let mut map_y = vec![0.0f32; width * height];
    for v in 0..height {
        let y = (v as f64 - cy) / f;
        for u in 0..width {
            let x = (u as f64 - cx) / f;
            let r2 = x * x + y * y;
            let d = 1.0 + k1 * r2;
            let idx = v * width + u;
            map_x[idx] = (x * d * f + cx) as f32;
            map_y[idx] = (y * d * f + cy) as f32;
        }
    }
    UndistortMap {
        map_x,
        map_y,
        k1,
        width,
        height,
    }
}

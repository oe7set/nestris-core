//! numpy-compatible reductions: percentile (linear interpolation), median,
//! population std, argsort. The engine's adaptive thresholds were tuned
//! against numpy's exact semantics, so these replicate them in f64.

/// numpy `percentile(data, q)` with the default linear interpolation.
/// Sorts a copy; `data` must be non-empty and free of NaN.
pub fn percentile(data: &[f64], q: f64) -> f64 {
    debug_assert!(!data.is_empty());
    let mut sorted = data.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).expect("NaN in percentile input"));
    percentile_sorted(&sorted, q)
}

/// Percentile over already-sorted data (ascending).
pub fn percentile_sorted(sorted: &[f64], q: f64) -> f64 {
    let n = sorted.len();
    if n == 1 {
        return sorted[0];
    }
    let pos = q / 100.0 * (n - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    let frac = pos - lo as f64;
    sorted[lo] + (sorted[hi] - sorted[lo]) * frac
}

/// numpy `median` (= 50th percentile with linear interpolation).
pub fn median(data: &[f64]) -> f64 {
    percentile(data, 50.0)
}

/// numpy `std` with the default `ddof=0` (population standard deviation).
pub fn std_dev(data: &[f64]) -> f64 {
    debug_assert!(!data.is_empty());
    let n = data.len() as f64;
    let mean = data.iter().sum::<f64>() / n;
    let var = data.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / n;
    var.sqrt()
}

/// Indices that would sort `data` ascending (stable, like numpy on distinct
/// values; ties keep original order which numpy's default quicksort does NOT
/// guarantee — callers must not depend on tie order).
pub fn argsort(data: &[f64]) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..data.len()).collect();
    idx.sort_by(|&a, &b| data[a].partial_cmp(&data[b]).expect("NaN in argsort input"));
    idx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_matches_numpy_basics() {
        let data = [1.0, 2.0, 3.0, 4.0];
        assert_eq!(percentile(&data, 50.0), 2.5);
        assert_eq!(percentile(&data, 0.0), 1.0);
        assert_eq!(percentile(&data, 100.0), 4.0);
        assert!((percentile(&data, 90.0) - 3.7).abs() < 1e-12);
    }

    #[test]
    fn std_is_population() {
        let data = [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];
        assert!((std_dev(&data) - 2.0).abs() < 1e-12);
    }
}

//! Pipeline performance over a rolling window: what the capture delivers,
//! what the engine processes and drops, how long a frame takes and waits,
//! how often `live` goes out, and the CPU load. Published as `perf` in the
//! status payload (`docs/STATION.md`).

use std::time::{Duration, Instant};

use nestris_host::capture_supervisor::CaptureStats;
use serde::Serialize;

/// Length of one measuring window.
pub const WINDOW: Duration = Duration::from_secs(2);

/// One finished window (`perf` in `<base>/status`).
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Perf {
    /// Frames ffmpeg delivered per second.
    pub capture_fps: f64,
    /// Frames the engine processed per second.
    pub fps: f64,
    /// The configured capture rate (`capture.fps`), if any.
    pub target_fps: Option<f64>,
    /// Share of delivered frames dropped because the engine fell behind.
    pub drop_rate: f64,
    /// Frames missing from the source in this window (gaps in its clock).
    pub missing: u64,
    /// Totals since the station started.
    pub dropped_total: u64,
    pub missing_total: u64,
    /// Engine time per frame (`process` + recalibration hand-off).
    pub engine_ms_p50: Option<f64>,
    pub engine_ms_p95: Option<f64>,
    /// Read from ffmpeg until the result is out (queue wait + engine).
    pub frame_age_ms_p95: Option<f64>,
    /// `live` messages published per second.
    pub live_hz: f64,
    /// Engine input size, e.g. `360x288`.
    pub size: Option<String>,
    /// Whole-machine CPU use (Linux only).
    pub cpu_pct: Option<f64>,
    pub load1: Option<f64>,
}

pub struct Telemetry {
    target_fps: Option<f64>,
    started: Instant,
    processed: u64,
    live_sent: u64,
    engine_ms: Vec<f64>,
    age_ms: Vec<f64>,
    capture_at_start: CaptureStats,
    cpu: CpuSampler,
    size: Option<String>,
    last: Option<Perf>,
}

impl Telemetry {
    pub fn new(target_fps: Option<f64>) -> Self {
        Self {
            target_fps,
            started: Instant::now(),
            processed: 0,
            live_sent: 0,
            engine_ms: Vec::new(),
            age_ms: Vec::new(),
            capture_at_start: CaptureStats::default(),
            cpu: CpuSampler::default(),
            size: None,
            last: None,
        }
    }

    /// A processed frame: engine time and age when its result was out.
    /// (`None`s for replayed frames, which have no capture.)
    pub fn frame(
        &mut self,
        engine_ms: Option<f64>,
        age_ms: Option<f64>,
        size: Option<(usize, usize)>,
    ) {
        self.processed += 1;
        self.engine_ms.extend(engine_ms);
        self.age_ms.extend(age_ms);
        if let Some((w, h)) = size {
            self.size = Some(format!("{w}x{h}"));
        }
    }

    pub fn live_sent(&mut self) {
        self.live_sent += 1;
    }

    /// Close the window once it is [`WINDOW`] long; `capture` is the
    /// supervisor's running total.
    pub fn roll(&mut self, capture: CaptureStats) {
        let elapsed = self.started.elapsed();
        if elapsed < WINDOW {
            return;
        }
        self.last = Some(self.summarize(elapsed.as_secs_f64(), capture));
        self.started = Instant::now();
        self.processed = 0;
        self.live_sent = 0;
        self.engine_ms.clear();
        self.age_ms.clear();
        self.capture_at_start = capture;
    }

    fn summarize(&mut self, secs: f64, capture: CaptureStats) -> Perf {
        let base = self.capture_at_start;
        // A restarted supervisor (new capture loop) starts from zero.
        let delta = |now: u64, then: u64| now.checked_sub(then).unwrap_or(now);
        let delivered = delta(capture.delivered, base.delivered);
        let dropped = delta(capture.dropped, base.dropped);
        let missing = delta(capture.missing, base.missing);
        self.engine_ms.sort_by(|a, b| a.total_cmp(b));
        self.age_ms.sort_by(|a, b| a.total_cmp(b));
        Perf {
            capture_fps: round1(delivered as f64 / secs),
            fps: round1(self.processed as f64 / secs),
            target_fps: self.target_fps,
            drop_rate: if delivered > 0 {
                round4(dropped as f64 / delivered as f64)
            } else {
                0.0
            },
            missing,
            dropped_total: capture.dropped,
            missing_total: capture.missing,
            engine_ms_p50: percentile(&self.engine_ms, 0.50).map(round2),
            engine_ms_p95: percentile(&self.engine_ms, 0.95).map(round2),
            frame_age_ms_p95: percentile(&self.age_ms, 0.95).map(round2),
            live_hz: round1(self.live_sent as f64 / secs),
            size: self.size.clone(),
            cpu_pct: self.cpu.sample().map(round1),
            load1: load1(),
        }
    }

    /// The last finished window (`None` until the first one closed).
    pub fn perf(&self) -> Option<&Perf> {
        self.last.as_ref()
    }

    /// Processed frames per second of the last window (0 before).
    pub fn fps(&self) -> f64 {
        self.last.as_ref().map_or(0.0, |p| p.fps)
    }
}

/// The `q` quantile (0..=1) of an ascending sample.
pub fn percentile(sorted: &[f64], q: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    Some(sorted[((sorted.len() - 1) as f64 * q).round() as usize])
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn round4(v: f64) -> f64 {
    (v * 10000.0).round() / 10000.0
}

/// CPU use between two `/proc/stat` reads.
#[derive(Default)]
struct CpuSampler {
    last: Option<(u64, u64)>,
}

impl CpuSampler {
    fn sample(&mut self) -> Option<f64> {
        let now = std::fs::read_to_string("/proc/stat")
            .ok()
            .and_then(|s| parse_proc_stat(&s))?;
        let prev = self.last.replace(now)?;
        cpu_busy_pct(prev, now)
    }
}

/// `(total, idle)` jiffies from the `cpu` line of `/proc/stat`.
fn parse_proc_stat(text: &str) -> Option<(u64, u64)> {
    let line = text.lines().find(|l| l.starts_with("cpu "))?;
    let values: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|v| v.parse().ok())
        .collect();
    if values.len() < 4 {
        return None;
    }
    // idle + iowait
    let idle = values[3] + values.get(4).copied().unwrap_or(0);
    // guest/guest_nice are already part of user/nice.
    let total = values.iter().take(8).sum();
    Some((total, idle))
}

fn cpu_busy_pct(prev: (u64, u64), now: (u64, u64)) -> Option<f64> {
    let total = now.0.checked_sub(prev.0)?;
    let idle = now.1.checked_sub(prev.1)?;
    if total == 0 {
        return None;
    }
    Some(100.0 * (total.saturating_sub(idle)) as f64 / total as f64)
}

fn load1() -> Option<f64> {
    std::fs::read_to_string("/proc/loadavg")
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles() {
        assert_eq!(percentile(&[], 0.5), None);
        let v: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(percentile(&v, 0.5), Some(51.0));
        assert_eq!(percentile(&v, 0.95), Some(95.0));
        assert_eq!(percentile(&[7.0], 0.95), Some(7.0));
    }

    #[test]
    fn window_rates() {
        let mut t = Telemetry::new(Some(50.0));
        t.capture_at_start = CaptureStats {
            delivered: 1000,
            dropped: 10,
            missing: 2,
        };
        for i in 0..90 {
            t.frame(Some(2.0 + f64::from(i % 10)), Some(5.0), Some((360, 288)));
        }
        for _ in 0..50 {
            t.live_sent();
        }
        let p = t.summarize(
            2.0,
            CaptureStats {
                delivered: 1100,
                dropped: 20,
                missing: 5,
            },
        );
        assert_eq!(p.capture_fps, 50.0);
        assert_eq!(p.fps, 45.0);
        assert_eq!(p.drop_rate, 0.1);
        assert_eq!(p.missing, 3);
        assert_eq!((p.dropped_total, p.missing_total), (20, 5));
        assert_eq!(p.engine_ms_p50, Some(7.0));
        assert_eq!(p.frame_age_ms_p95, Some(5.0));
        assert_eq!(p.live_hz, 25.0);
        assert_eq!(p.size.as_deref(), Some("360x288"));
        assert_eq!(p.target_fps, Some(50.0));
    }

    #[test]
    fn restarted_supervisor_counts_from_zero() {
        let mut t = Telemetry::new(None);
        t.capture_at_start = CaptureStats {
            delivered: 5000,
            dropped: 0,
            missing: 0,
        };
        let p = t.summarize(
            2.0,
            CaptureStats {
                delivered: 100,
                dropped: 0,
                missing: 0,
            },
        );
        assert_eq!(p.capture_fps, 50.0);
    }

    #[test]
    fn proc_stat_cpu() {
        let a = parse_proc_stat("cpu  100 0 100 800 0 0 0 0 0 0\ncpu0 1 2 3 4\n").unwrap();
        let b = parse_proc_stat("cpu  200 0 200 1400 0 0 0 0 0 0\n").unwrap();
        assert_eq!(a, (1000, 800));
        // 800 jiffies passed, 600 idle: 25 % busy.
        assert_eq!(cpu_busy_pct(a, b), Some(25.0));
        assert_eq!(parse_proc_stat("intr 1 2"), None);
    }
}

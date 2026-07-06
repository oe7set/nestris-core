//! Native host driver for the engine's background-recalibration protocol:
//! a worker thread running `estimate_geometry` on frame snapshots
//! (newest-wins), paced like the Python BackgroundRecalibrator (>=0.5 s
//! between solves), results polled by the processing loop.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use nestris_engine::frame::Frame;
use nestris_engine::geometry_cal::calibration::{
    GeometryResult, SolveOptions, estimate_geometry_with,
};
use nestris_engine::geometry_cal::lock::LockState;
use nestris_engine::layout::get_layout;
use nestris_engine::processor::FrameProcessor;
use nestris_vision::Image;

const MIN_SOLVE_INTERVAL_S: f64 = 0.5;

struct Snapshot {
    image: Image,
    seq: i64,
    undistort: Option<Arc<nestris_vision::undistort::UndistortMap>>,
    try_undistort: bool,
    opts: SolveOptions,
}

pub struct RecalibThread {
    latest: Arc<Mutex<Option<Snapshot>>>,
    results: Receiver<GeometryResult>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    last_submit_ts: Option<f64>,
}

impl RecalibThread {
    pub fn start() -> RecalibThread {
        let latest: Arc<Mutex<Option<Snapshot>>> = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx): (Sender<GeometryResult>, Receiver<GeometryResult>) = channel();
        let worker_latest = latest.clone();
        let worker_stop = stop.clone();
        let handle = std::thread::spawn(move || {
            // Solves run on a dedicated half-size rayon pool: their
            // row-parallel kernels must not queue behind (or ahead of) the
            // pipeline thread's own global-pool work (warp/NCC splits), which
            // would inflate per-frame latency whenever a solve is in flight.
            let threads = std::thread::available_parallelism()
                .map(|n| (n.get() / 2).max(1))
                .unwrap_or(1);
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .ok();
            while !worker_stop.load(Ordering::Relaxed) {
                let snapshot = worker_latest.lock().unwrap().take();
                let Some(snapshot) = snapshot else {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                };
                let solve = || {
                    estimate_geometry_with(
                        &snapshot.image,
                        get_layout(),
                        snapshot.undistort.clone(),
                        snapshot.try_undistort,
                        snapshot.seq as u64,
                        &snapshot.opts,
                    )
                };
                let result = match &pool {
                    Some(pool) => pool.install(solve),
                    None => solve(),
                };
                // Failed solves must flow too: background acquisition resets
                // its streak on them (maintenance mode ignores them anyway).
                if tx.send(result).is_err() {
                    break;
                }
            }
        });
        RecalibThread {
            latest,
            results: rx,
            stop,
            handle: Some(handle),
            last_submit_ts: None,
        }
    }

    /// Feed the protocol for one processed frame: snapshot when the lock asks
    /// and the pacing interval elapsed; adopt any finished solve.
    pub fn drive(&mut self, processor: &mut FrameProcessor, frame: &Frame) {
        while let Ok(result) = self.results.try_recv() {
            processor.lock().offer_solution(result);
        }
        let lock = processor.lock();
        if !lock.wants_background_solve() {
            return;
        }
        // The tracker signals urgency (drift, misses, sustained motion) via
        // a faster pacing hint; stable sources keep the relaxed default.
        let min_interval = lock.solve_interval_hint().unwrap_or(MIN_SOLVE_INTERVAL_S);
        if let Some(last) = self.last_submit_ts
            && frame.ts - last < min_interval
        {
            return;
        }
        self.last_submit_ts = Some(frame.ts);
        // Acquisition solves honor the lock's once-only undistort guard;
        // locked-state maintenance keeps today's re-probe-when-none behavior.
        let acquiring = !matches!(lock.state(), LockState::Locked | LockState::Drift);
        let undistort = lock.undistort_map();
        let try_undistort = if acquiring {
            lock.try_undistort_hint()
        } else {
            undistort.is_none()
        };
        let snapshot = Snapshot {
            image: frame.image.clone(),
            seq: frame.seq,
            undistort,
            try_undistort,
            opts: lock.solve_options(),
        };
        *self.latest.lock().unwrap() = Some(snapshot);
    }
}

impl Drop for RecalibThread {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

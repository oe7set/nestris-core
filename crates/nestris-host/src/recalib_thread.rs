//! Native host driver for the engine's background-recalibration protocol:
//! a worker thread running `estimate_geometry` on frame snapshots
//! (newest-wins), paced like the Python BackgroundRecalibrator (>=0.5 s
//! between solves), results polled by the processing loop.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use nestris_engine::frame::Frame;
use nestris_engine::geometry_cal::calibration::{GeometryResult, estimate_geometry};
use nestris_engine::layout::get_layout;
use nestris_engine::processor::FrameProcessor;
use nestris_vision::Image;

const MIN_SOLVE_INTERVAL_S: f64 = 0.5;

struct Snapshot {
    image: Image,
    seq: i64,
    undistort: Option<Arc<nestris_vision::undistort::UndistortMap>>,
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
            while !worker_stop.load(Ordering::Relaxed) {
                let snapshot = worker_latest.lock().unwrap().take();
                let Some(snapshot) = snapshot else {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                };
                let result = estimate_geometry(
                    &snapshot.image,
                    get_layout(),
                    snapshot.undistort.clone(),
                    snapshot.undistort.is_none(),
                    snapshot.seq as u64,
                );
                if result.ok() && tx.send(result).is_err() {
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
        let snapshot = Snapshot {
            image: frame.image.clone(),
            seq: frame.seq,
            undistort: lock.undistort_map(),
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

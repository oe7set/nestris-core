//! Self-healing capture: runs the ffmpeg decoder on its own thread and keeps
//! a live source alive across every failure mode an unattended station
//! sees — ffmpeg dying, the device stalling (no frames), the USB capture
//! stick being unplugged and re-plugged.
//!
//! The consumer receives [`CaptureMsg`]s over a bounded channel: frames
//! (with a monotonic `seq`/`ts` across restarts, and `discontinuity` set on
//! the first frame after a reconnect) and [`CaptureStatus`] changes. Frames
//! are dropped when the consumer falls behind, so latency stays bounded;
//! status messages are never dropped.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use nestris_engine::frame::Frame;

use crate::capture_ffmpeg::{self, DecoderKiller, InputKind, LiveOptions, VideoDecoder};

/// Frames buffered between the capture thread and the consumer.
const FRAME_QUEUE: usize = 4;
/// A session delivering frames this long resets the restart backoff.
const HEALTHY_SESSION: Duration = Duration::from_secs(10);
/// Poll interval while waiting for a missing device node.
const DEVICE_POLL: Duration = Duration::from_millis(500);

#[derive(Clone, Debug)]
pub struct SupervisorConfig {
    /// ffmpeg input (`v4l2:/dev/...`, `dshow:...`, or a file for testing).
    pub input: String,
    pub live: LiveOptions,
    /// No frame for this long counts as a stall and restarts ffmpeg.
    pub stall_timeout: Duration,
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    /// Pace file input at its native frame rate (realistic tests).
    pub pace_files: bool,
    /// Restart after a file ends (loop it) instead of reporting `Ended`.
    pub loop_files: bool,
}

impl SupervisorConfig {
    pub fn new(input: impl Into<String>) -> Self {
        Self {
            input: input.into(),
            live: LiveOptions::default(),
            stall_timeout: Duration::from_secs(5),
            backoff_min: Duration::from_secs(1),
            backoff_max: Duration::from_secs(30),
            pace_files: false,
            loop_files: false,
        }
    }
}

/// Capture health, published on every change.
#[derive(Clone, Debug, PartialEq)]
pub enum CaptureStatus {
    /// Starting ffmpeg.
    Opening,
    /// Frames are flowing.
    Running { width: usize, height: usize },
    /// The device node is missing (unplugged); waiting for it to return.
    WaitingForDevice { path: PathBuf },
    /// The source failed; retrying after `retry_in`.
    Failed { reason: String, retry_in: Duration },
    /// A file source ended (never for live sources).
    Ended,
}

pub enum CaptureMsg {
    Frame(Frame),
    Status(CaptureStatus),
}

/// Result of [`CaptureSupervisor::recv`].
pub enum Recv {
    Msg(CaptureMsg),
    /// Nothing arrived within the timeout (the capture thread is alive).
    Timeout,
    /// The capture thread is gone (file ended or shutdown).
    Closed,
}

pub struct CaptureSupervisor {
    rx: Receiver<CaptureMsg>,
    stop: Arc<AtomicBool>,
    killer: Arc<Mutex<Option<DecoderKiller>>>,
    dropped: Arc<AtomicU64>,
    handles: Vec<JoinHandle<()>>,
}

impl CaptureSupervisor {
    pub fn start(cfg: SupervisorConfig) -> CaptureSupervisor {
        let (tx, rx) = sync_channel(FRAME_QUEUE);
        let stop = Arc::new(AtomicBool::new(false));
        let killer: Arc<Mutex<Option<DecoderKiller>>> = Arc::default();
        let dropped = Arc::new(AtomicU64::new(0));
        // Milliseconds since `epoch` of the last frame (stall detection).
        let epoch = Instant::now();
        let last_frame_ms = Arc::new(AtomicU64::new(0));
        let reading = Arc::new(AtomicBool::new(false));

        let mut handles = Vec::new();
        {
            let (stop, killer, last, reading) = (
                stop.clone(),
                killer.clone(),
                last_frame_ms.clone(),
                reading.clone(),
            );
            let timeout_ms = cfg.stall_timeout.as_millis() as u64;
            handles.push(std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(250));
                    if !reading.load(Ordering::Relaxed) {
                        continue;
                    }
                    let now = epoch.elapsed().as_millis() as u64;
                    if now.saturating_sub(last.load(Ordering::Relaxed)) > timeout_ms
                        && let Some(k) = killer.lock().unwrap_or_else(|e| e.into_inner()).as_ref()
                    {
                        // Unblocks the capture thread's read; it restarts.
                        k.kill();
                        last.store(now, Ordering::Relaxed);
                    }
                }
            }));
        }
        {
            let worker = Worker {
                cfg,
                tx,
                stop: stop.clone(),
                killer: killer.clone(),
                dropped: dropped.clone(),
                epoch,
                last_frame_ms,
                reading,
                seq: 0,
            };
            handles.push(std::thread::spawn(move || worker.run()));
        }
        CaptureSupervisor {
            rx,
            stop,
            killer,
            dropped,
            handles,
        }
    }

    /// Wait up to `timeout` for the next message.
    pub fn recv(&self, timeout: Duration) -> Recv {
        match self.rx.recv_timeout(timeout) {
            Ok(msg) => Recv::Msg(msg),
            Err(RecvTimeoutError::Timeout) => Recv::Timeout,
            Err(RecvTimeoutError::Disconnected) => Recv::Closed,
        }
    }

    /// Frames dropped because the consumer fell behind.
    pub fn dropped_frames(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Force a reconnect (e.g. the consumer judged the picture dead).
    pub fn restart(&self) {
        if let Some(k) = self
            .killer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            k.kill();
        }
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.restart();
        // Drain so a capture thread blocked on a full queue can exit.
        while self.rx.try_recv().is_ok() {}
        for handle in self.handles.drain(..) {
            while !handle.is_finished() {
                while self.rx.try_recv().is_ok() {}
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = handle.join();
        }
    }
}

impl Drop for CaptureSupervisor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct Worker {
    cfg: SupervisorConfig,
    tx: SyncSender<CaptureMsg>,
    stop: Arc<AtomicBool>,
    killer: Arc<Mutex<Option<DecoderKiller>>>,
    dropped: Arc<AtomicU64>,
    epoch: Instant,
    last_frame_ms: Arc<AtomicU64>,
    reading: Arc<AtomicBool>,
    seq: i64,
}

enum SessionEnd {
    /// The consumer is gone or shutdown was requested.
    Quit,
    /// A file source reached its end.
    Eof,
    Failed(String),
}

impl Worker {
    fn run(mut self) {
        let kind = InputKind::of(&self.cfg.input);
        let mut backoff = self.cfg.backoff_min;
        let mut last_status: Option<CaptureStatus> = None;
        let mut first_session = true;
        while !self.stop.load(Ordering::Relaxed) {
            if let Some(path) = capture_ffmpeg::device_path(&self.cfg.input)
                && !path.exists()
            {
                let status = CaptureStatus::WaitingForDevice {
                    path: path.to_path_buf(),
                };
                if !self.status(&mut last_status, status) {
                    return;
                }
                std::thread::sleep(DEVICE_POLL);
                continue;
            }

            if !self.status(&mut last_status, CaptureStatus::Opening) {
                return;
            }
            let started = Instant::now();
            let end = match VideoDecoder::open_with(&self.cfg.input, 0.0, &self.cfg.live) {
                Ok(decoder) => {
                    let end = self.session(decoder, !first_session, &mut last_status);
                    first_session = false;
                    end
                }
                Err(e) => SessionEnd::Failed(format!("{e:#}")),
            };
            match end {
                SessionEnd::Quit => return,
                // A file that cannot be read will not heal by retrying.
                SessionEnd::Failed(reason) if !kind.is_live() => {
                    let status = CaptureStatus::Failed {
                        reason,
                        retry_in: Duration::ZERO,
                    };
                    if self.status(&mut last_status, status) {
                        let _ = self.status(&mut last_status, CaptureStatus::Ended);
                    }
                    return;
                }
                SessionEnd::Eof if !kind.is_live() && !self.cfg.loop_files => {
                    let _ = self.status(&mut last_status, CaptureStatus::Ended);
                    return;
                }
                SessionEnd::Eof if !kind.is_live() => {
                    backoff = self.cfg.backoff_min;
                    continue;
                }
                SessionEnd::Eof => self.retry(
                    &mut last_status,
                    "stream ended".into(),
                    &mut backoff,
                    started,
                ),
                SessionEnd::Failed(reason) => {
                    self.retry(&mut last_status, reason, &mut backoff, started)
                }
            }
        }
    }

    fn retry(
        &mut self,
        last_status: &mut Option<CaptureStatus>,
        reason: String,
        backoff: &mut Duration,
        started: Instant,
    ) {
        if started.elapsed() >= HEALTHY_SESSION {
            *backoff = self.cfg.backoff_min;
        }
        let status = CaptureStatus::Failed {
            reason,
            retry_in: *backoff,
        };
        if !self.status(last_status, status) {
            return;
        }
        // Sleep in slices so shutdown stays responsive.
        let until = Instant::now() + *backoff;
        while Instant::now() < until && !self.stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(100));
        }
        *backoff = (*backoff * 2).min(self.cfg.backoff_max);
    }

    fn session(
        &mut self,
        mut decoder: VideoDecoder,
        reconnect: bool,
        last_status: &mut Option<CaptureStatus>,
    ) -> SessionEnd {
        let live = InputKind::of(&self.cfg.input).is_live();
        let fps = decoder.info().fps;
        let (width, height) = (decoder.info().width, decoder.info().height);
        *self.killer.lock().unwrap_or_else(|e| e.into_inner()) = Some(decoder.killer());
        self.touch();
        self.reading.store(true, Ordering::Relaxed);
        let paced_start = Instant::now();
        let mut first = true;
        let end = loop {
            if self.stop.load(Ordering::Relaxed) {
                break SessionEnd::Quit;
            }
            let mut frame = match decoder.next_frame() {
                Ok(Some(frame)) => frame,
                Ok(None) if first => {
                    break SessionEnd::Failed(stderr_reason(&decoder, "no frames"));
                }
                Ok(None) => {
                    break if live {
                        SessionEnd::Failed(stderr_reason(&decoder, "stream ended"))
                    } else {
                        SessionEnd::Eof
                    };
                }
                Err(e) => break SessionEnd::Failed(stderr_reason(&decoder, &format!("{e}"))),
            };
            self.touch();
            if first {
                first = false;
                if !self.status(last_status, CaptureStatus::Running { width, height }) {
                    break SessionEnd::Quit;
                }
                frame.discontinuity = reconnect;
            }
            if !live && self.cfg.pace_files {
                let due = Duration::from_secs_f64(frame.seq as f64 / fps);
                if let Some(wait) = due.checked_sub(paced_start.elapsed()) {
                    std::thread::sleep(wait);
                }
            }
            // Monotonic across restarts: the engine's clocks never run back.
            frame.seq = self.seq;
            self.seq += 1;
            if live {
                frame.ts = self.epoch.elapsed().as_secs_f64();
            }
            match self.tx.try_send(CaptureMsg::Frame(frame)) {
                Ok(()) => {}
                Err(TrySendError::Full(CaptureMsg::Frame(frame))) => {
                    if live {
                        self.dropped.fetch_add(1, Ordering::Relaxed);
                    } else if self.tx.send(CaptureMsg::Frame(frame)).is_err() {
                        // Files are never dropped: block instead.
                        break SessionEnd::Quit;
                    }
                }
                Err(_) => break SessionEnd::Quit,
            }
        };
        self.reading.store(false, Ordering::Relaxed);
        *self.killer.lock().unwrap_or_else(|e| e.into_inner()) = None;
        end
    }

    fn touch(&self) {
        self.last_frame_ms
            .store(self.epoch.elapsed().as_millis() as u64, Ordering::Relaxed);
    }

    /// Publish a status change; `false` when the consumer is gone.
    fn status(&self, last: &mut Option<CaptureStatus>, status: CaptureStatus) -> bool {
        if last.as_ref() == Some(&status) {
            return true;
        }
        *last = Some(status.clone());
        self.tx.send(CaptureMsg::Status(status)).is_ok()
    }
}

fn stderr_reason(decoder: &VideoDecoder, what: &str) -> String {
    match decoder.stderr_tail().last() {
        Some(line) => format!("{what}: {line}"),
        None => what.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_v4l2_device_waits_instead_of_failing() {
        let mut cfg = SupervisorConfig::new("v4l2:/nonexistent/nestris-test-video");
        cfg.backoff_min = Duration::from_millis(10);
        let sup = CaptureSupervisor::start(cfg);
        match sup.recv(Duration::from_secs(2)) {
            Recv::Msg(CaptureMsg::Status(CaptureStatus::WaitingForDevice { path })) => {
                assert!(path.ends_with("nestris-test-video"));
            }
            _ => panic!("expected WaitingForDevice"),
        }
        // Status is not repeated while still waiting.
        assert!(matches!(
            sup.recv(Duration::from_millis(1200)),
            Recv::Timeout
        ));
        sup.stop();
    }

    #[test]
    fn unreadable_file_fails_and_ends() {
        let sup = CaptureSupervisor::start(SupervisorConfig::new("/nonexistent/nestris-test.mp4"));
        let mut statuses = Vec::new();
        loop {
            match sup.recv(Duration::from_secs(10)) {
                Recv::Msg(CaptureMsg::Status(s)) => statuses.push(s),
                Recv::Msg(CaptureMsg::Frame(_)) => panic!("no frames expected"),
                Recv::Timeout | Recv::Closed => break,
            }
        }
        assert!(
            matches!(
                statuses[..],
                [
                    CaptureStatus::Opening,
                    CaptureStatus::Failed { .. },
                    CaptureStatus::Ended
                ]
            ),
            "{statuses:?}"
        );
        sup.stop();
    }

    #[test]
    fn failing_live_device_retries_with_backoff() {
        let mut cfg = SupervisorConfig::new("dshow:nestris-nonexistent-test-device");
        cfg.backoff_min = Duration::from_millis(20);
        cfg.backoff_max = Duration::from_millis(40);
        let sup = CaptureSupervisor::start(cfg);
        let mut failures = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while failures.len() < 2 && Instant::now() < deadline {
            if let Recv::Msg(CaptureMsg::Status(CaptureStatus::Failed { retry_in, .. })) =
                sup.recv(Duration::from_millis(500))
            {
                failures.push(retry_in);
            }
        }
        assert_eq!(
            failures,
            vec![Duration::from_millis(20), Duration::from_millis(40)]
        );
        sup.stop();
    }
}

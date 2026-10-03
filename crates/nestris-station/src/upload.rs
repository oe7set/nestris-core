//! Uploads finished recordings (`.ngf.gz`) to the NestrisLTM host.
//!
//! When a recording is saved, a small job file `{game_id, path}` is written
//! to `<state_dir>/uploads` (temp file + rename). A background thread sends
//! each job as
//!
//! ```text
//! PUT <host.url>/api/stations/<station>/games/<game_id>/ngf
//! Authorization: Bearer <host token>      (NestrisLTM API token, scope "stations")
//! X-NGF-SHA256: <hex>
//! ```
//!
//! and deletes the job on success. `404` means the host has not processed
//! the game's `game_end` yet: the job is retried. Network errors and `5xx`
//! are retried with exponential backoff; jobs older than `max_age_h` or a
//! permanent rejection (`400`, `409`, `413`) are dropped with an error log.
//! Jobs survive restarts, so a host outage only delays the upload.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{debug, error, info, warn};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UploadJob {
    pub game_id: String,
    pub path: PathBuf,
}

/// What the host said about one attempt.
#[derive(Debug, PartialEq)]
enum Outcome {
    Done,
    /// Try again later (host down, game not known yet, 5xx).
    Retry(String),
    /// Will never succeed (bad file, wrong station, too large, file gone).
    Drop(String),
}

pub struct UploadQueue {
    dir: PathBuf,
    counter: AtomicU64,
}

impl UploadQueue {
    pub fn open(dir: PathBuf) -> Result<UploadQueue> {
        fs::create_dir_all(&dir).with_context(|| format!("create upload dir {}", dir.display()))?;
        for entry in fs::read_dir(&dir)?.flatten() {
            if entry.path().extension().is_some_and(|e| e == "tmp") {
                let _ = fs::remove_file(entry.path());
            }
        }
        Ok(UploadQueue {
            dir,
            counter: AtomicU64::new(0),
        })
    }

    pub fn push(&self, job: &UploadJob) -> Result<()> {
        let id = format!(
            "{:013}-{:06}",
            chrono::Utc::now().timestamp_millis(),
            self.counter.fetch_add(1, Ordering::Relaxed) % 1_000_000
        );
        let tmp = self.dir.join(format!("{id}.tmp"));
        fs::write(&tmp, serde_json::to_vec(job)?)?;
        fs::rename(&tmp, self.dir.join(format!("{id}.json")))?;
        Ok(())
    }

    /// Pending jobs, oldest first.
    pub fn list(&self) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = fs::read_dir(&self.dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|e| e == "json"))
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        files
    }

    fn load(path: &Path) -> Option<UploadJob> {
        serde_json::from_slice(&fs::read(path).ok()?).ok()
    }
}

pub struct UploaderConfig {
    pub base_url: String,
    pub station: String,
    pub token: String,
    pub retry_max: Duration,
    pub max_age: Duration,
    pub timeout: Duration,
}

pub struct Uploader {
    queue: Arc<UploadQueue>,
    stop: Arc<AtomicBool>,
    wake: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Uploader {
    pub fn start(queue: Arc<UploadQueue>, cfg: UploaderConfig) -> Uploader {
        let stop = Arc::new(AtomicBool::new(false));
        let wake = Arc::new(AtomicBool::new(false));
        let (q, s, w) = (queue.clone(), stop.clone(), wake.clone());
        let handle = std::thread::Builder::new()
            .name("ngf-upload".into())
            .spawn(move || run(&q, &cfg, &s, &w))
            .expect("spawn upload thread");
        Uploader {
            queue,
            stop,
            wake,
            handle: Some(handle),
        }
    }

    /// A job was queued: upload now instead of after the idle pause.
    pub fn notify(&self) {
        self.wake.store(true, Ordering::Relaxed);
    }

    /// On shutdown: give pending uploads up to `timeout` to finish.
    pub fn drain(&self, timeout: Duration) {
        self.notify();
        let until = Instant::now() + timeout;
        while Instant::now() < until && !self.queue.list().is_empty() {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Uploader {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn run(queue: &UploadQueue, cfg: &UploaderConfig, stop: &AtomicBool, wake: &AtomicBool) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(cfg.timeout))
        .http_status_as_error(false)
        .build()
        .into();
    let mut delay = Duration::from_secs(2);
    while !stop.load(Ordering::Relaxed) {
        let mut retry = false;
        for job_path in queue.list() {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            match process(&agent, cfg, &job_path) {
                Outcome::Done => {
                    let _ = fs::remove_file(&job_path);
                    delay = Duration::from_secs(2);
                }
                Outcome::Drop(reason) => {
                    error!(job = %job_path.display(), reason, "recording upload dropped");
                    let _ = fs::remove_file(&job_path);
                }
                Outcome::Retry(reason) => {
                    debug!(job = %job_path.display(), reason, "recording upload deferred");
                    retry = true;
                    break; // keep order; try again after the backoff
                }
            }
        }
        let wait = if retry { delay } else { Duration::from_secs(5) };
        if retry {
            delay = (delay * 2).min(cfg.retry_max);
        }
        sleep_unless(stop, wake, wait);
    }
}

/// Sleep up to `total`; a stop or wake request ends the pause early.
fn sleep_unless(stop: &AtomicBool, wake: &AtomicBool, total: Duration) {
    let until = Instant::now() + total;
    while Instant::now() < until && !stop.load(Ordering::Relaxed) {
        if wake.swap(false, Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn process(agent: &ureq::Agent, cfg: &UploaderConfig, job_path: &Path) -> Outcome {
    let Some(job) = UploadQueue::load(job_path) else {
        return Outcome::Drop("unreadable job file".into());
    };
    let age = fs::metadata(job_path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .unwrap_or_default();
    if age > cfg.max_age {
        return Outcome::Drop(format!("older than {} h", cfg.max_age.as_secs() / 3600));
    }
    let data = match fs::read(&job.path) {
        Ok(d) => d,
        Err(e) => return Outcome::Drop(format!("recording {}: {e}", job.path.display())),
    };
    let url = format!(
        "{}/api/stations/{}/games/{}/ngf",
        cfg.base_url.trim_end_matches('/'),
        cfg.station,
        job.game_id
    );
    let sha = hex(&Sha256::digest(&data));
    let response = agent
        .put(&url)
        .header("Authorization", &format!("Bearer {}", cfg.token))
        .header("Content-Type", "application/gzip")
        .header("X-NGF-SHA256", &sha)
        .send(&data[..]);
    let outcome = match response {
        Err(e) => Outcome::Retry(format!("{e}")),
        Ok(r) => classify(r.status().as_u16()),
    };
    if outcome == Outcome::Done {
        info!(game_id = %job.game_id, bytes = data.len(), "recording uploaded");
    } else if let Outcome::Retry(reason) = &outcome
        && reason.starts_with("401")
    {
        warn!(game_id = %job.game_id, "host rejected the upload token (host.token)");
    }
    outcome
}

fn classify(status: u16) -> Outcome {
    match status {
        200..=299 => Outcome::Done,
        404 => Outcome::Retry("404 game not known yet".into()),
        // A wrong/missing token is a config problem: keep the job until fixed.
        401 | 403 => Outcome::Retry(format!("{status} unauthorized")),
        400 | 409 | 413 | 422 => Outcome::Drop(format!("host rejected the recording ({status})")),
        s => Outcome::Retry(format!("{s} from host")),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_roundtrip_in_order() {
        let dir = std::env::temp_dir().join(format!("nltm-upload-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let q = UploadQueue::open(dir.clone()).unwrap();
        for id in ["a", "b"] {
            q.push(&UploadJob {
                game_id: id.into(),
                path: PathBuf::from(format!("/tmp/{id}.ngf.gz")),
            })
            .unwrap();
        }
        let jobs: Vec<_> = q
            .list()
            .iter()
            .map(|p| UploadQueue::load(p).unwrap().game_id)
            .collect();
        assert_eq!(jobs, ["a", "b"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_classification() {
        assert_eq!(classify(201), Outcome::Done);
        assert_eq!(classify(200), Outcome::Done);
        assert!(matches!(classify(404), Outcome::Retry(_)));
        assert!(matches!(classify(503), Outcome::Retry(_)));
        assert!(matches!(classify(401), Outcome::Retry(_)));
        assert!(matches!(classify(400), Outcome::Drop(_)));
        assert!(matches!(classify(409), Outcome::Drop(_)));
    }

    #[test]
    fn hex_encoding() {
        assert_eq!(hex(&[0x00, 0xab, 0xff]), "00abff");
    }
}

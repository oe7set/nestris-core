//! Service health: systemd notification (readiness, watchdog, status line)
//! and disk hygiene for the recording directory.

use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use tracing::{info, warn};

/// systemd `Type=notify` integration; a no-op off Linux or outside systemd.
pub struct Watchdog {
    interval: Option<Duration>,
    last_ping: Instant,
}

impl Watchdog {
    pub fn new() -> Watchdog {
        #[cfg(target_os = "linux")]
        let interval = sd_notify::watchdog_enabled().map(|d| d / 3);
        #[cfg(not(target_os = "linux"))]
        let interval: Option<Duration> = None;
        if let Some(i) = interval {
            info!(
                ping_every_ms = i.as_millis() as u64,
                "systemd watchdog enabled"
            );
        }
        Watchdog {
            interval,
            last_ping: Instant::now(),
        }
    }

    pub fn ready(&self) {
        #[cfg(target_os = "linux")]
        let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);
    }

    pub fn stopping(&self) {
        #[cfg(target_os = "linux")]
        let _ = sd_notify::notify(&[sd_notify::NotifyState::Stopping]);
    }

    /// Call from the main loop: pings the watchdog at a third of its
    /// timeout. A wedged main loop stops pinging and systemd restarts us.
    pub fn tick(&mut self, status: &str) {
        let Some(interval) = self.interval else {
            return;
        };
        if self.last_ping.elapsed() < interval {
            return;
        }
        self.last_ping = Instant::now();
        #[cfg(target_os = "linux")]
        let _ = sd_notify::notify(&[
            sd_notify::NotifyState::Watchdog,
            sd_notify::NotifyState::Status(status),
        ]);
        #[cfg(not(target_os = "linux"))]
        let _ = status;
    }
}

/// Delete finished recordings older than `keep_days`, then the oldest ones
/// until the directory is under `max_bytes` (`0` disables either limit).
/// In-progress `.part` files are never touched.
pub fn prune_recordings(dir: &Path, keep_days: u64, max_bytes: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(SystemTime, u64, std::path::PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let name = path.file_name()?.to_str()?;
            if !(name.ends_with(".ngf") || name.ends_with(".ngf.gz")) {
                return None;
            }
            let meta = e.metadata().ok()?;
            Some((meta.modified().ok()?, meta.len(), path))
        })
        .collect();
    files.sort();

    let mut removed = 0usize;
    if keep_days > 0 {
        let cutoff = SystemTime::now() - Duration::from_secs(keep_days * 86_400);
        files.retain(|(modified, _, path)| {
            if *modified < cutoff {
                removed += usize::from(std::fs::remove_file(path).is_ok());
                false
            } else {
                true
            }
        });
    }
    if max_bytes > 0 {
        let mut total: u64 = files.iter().map(|(_, len, _)| len).sum();
        for (_, len, path) in &files {
            if total <= max_bytes {
                break;
            }
            if std::fs::remove_file(path).is_ok() {
                removed += 1;
                total -= len;
            } else {
                warn!(path = %path.display(), "could not prune recording");
            }
        }
    }
    if removed > 0 {
        info!(removed, dir = %dir.display(), "pruned old recordings");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prunes_to_size_oldest_first_and_keeps_parts() {
        let dir = std::env::temp_dir().join(format!("nestris-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.ngf.gz", "b.ngf.gz", "c.ngf"] {
            std::fs::write(dir.join(name), vec![0u8; 100]).unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }
        std::fs::write(dir.join("live.ngf.part"), vec![0u8; 1000]).unwrap();
        prune_recordings(&dir, 0, 150);
        assert!(!dir.join("a.ngf.gz").exists());
        assert!(!dir.join("b.ngf.gz").exists());
        assert!(dir.join("c.ngf").exists());
        assert!(dir.join("live.ngf.part").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

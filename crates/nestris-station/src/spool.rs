//! Durable outbox for messages that must reach the host (game results,
//! cheat events). A message is written to disk (temp file + rename, so a
//! crash never leaves a torn file) before it is published and deleted only
//! after the broker acknowledged it. Delivery is therefore at-least-once:
//! consumers deduplicate by `game_id` (+ event kind).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::warn;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpooledMessage {
    pub topic: String,
    pub payload: String,
    pub retain: bool,
}

/// Spool entry id: the file stem (sorts in creation order).
pub type SpoolId = String;

pub struct Spool {
    dir: PathBuf,
    max_files: usize,
    counter: AtomicU64,
}

impl Spool {
    pub fn open(dir: PathBuf, max_files: usize) -> Result<Spool> {
        fs::create_dir_all(&dir).with_context(|| format!("create spool dir {}", dir.display()))?;
        // Leftover temp files are writes that never completed.
        for entry in fs::read_dir(&dir)?.flatten() {
            if entry.path().extension().is_some_and(|e| e == "tmp") {
                let _ = fs::remove_file(entry.path());
            }
        }
        Ok(Spool {
            dir,
            max_files: max_files.max(1),
            counter: AtomicU64::new(0),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Persist a message; returns its id.
    pub fn put(&self, msg: &SpooledMessage) -> Result<SpoolId> {
        let id = format!(
            "{:013}-{:06}",
            chrono::Utc::now().timestamp_millis(),
            self.counter.fetch_add(1, Ordering::Relaxed) % 1_000_000
        );
        let tmp = self.dir.join(format!("{id}.tmp"));
        let path = self.path(&id);
        let body = serde_json::to_vec(msg)?;
        {
            use std::io::Write;
            let mut f =
                fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
            f.write_all(&body)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &path).with_context(|| format!("commit {}", path.display()))?;
        self.enforce_limit();
        Ok(id)
    }

    /// Pending ids, oldest first.
    pub fn list(&self) -> Vec<SpoolId> {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut ids: Vec<SpoolId> = entries
            .flatten()
            .filter_map(|e| {
                let path = e.path();
                (path.extension()? == "json")
                    .then(|| path.file_stem()?.to_str().map(str::to_owned))
                    .flatten()
            })
            .collect();
        ids.sort();
        ids
    }

    pub fn get(&self, id: &str) -> Option<SpooledMessage> {
        let raw = fs::read(self.path(id)).ok()?;
        match serde_json::from_slice(&raw) {
            Ok(msg) => Some(msg),
            Err(e) => {
                warn!(id, error = %e, "dropping unreadable spool entry");
                self.remove(id);
                None
            }
        }
    }

    pub fn remove(&self, id: &str) {
        let _ = fs::remove_file(self.path(id));
    }

    fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    fn enforce_limit(&self) {
        let ids = self.list();
        if ids.len() > self.max_files {
            let excess = ids.len() - self.max_files;
            warn!(
                excess,
                "spool full, discarding the oldest undelivered messages"
            );
            for id in &ids[..excess] {
                self.remove(id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nestris-spool-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn msg(n: u32) -> SpooledMessage {
        SpooledMessage {
            topic: format!("t/{n}"),
            payload: format!("{{\"n\":{n}}}"),
            retain: false,
        }
    }

    #[test]
    fn survives_a_restart_until_removed() {
        let dir = temp_dir("restart");
        let spool = Spool::open(dir.clone(), 100).unwrap();
        let a = spool.put(&msg(1)).unwrap();
        let b = spool.put(&msg(2)).unwrap();
        // Simulate a crash mid-write.
        fs::write(dir.join("9999999999999-000000.tmp"), b"torn").unwrap();
        drop(spool);

        let spool = Spool::open(dir.clone(), 100).unwrap();
        assert_eq!(spool.list(), vec![a.clone(), b.clone()]);
        assert_eq!(spool.get(&a), Some(msg(1)));
        spool.remove(&a);
        assert_eq!(spool.list(), vec![b]);
        assert!(!dir.join("9999999999999-000000.tmp").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn limit_drops_the_oldest() {
        let dir = temp_dir("limit");
        let spool = Spool::open(dir.clone(), 2).unwrap();
        spool.put(&msg(1)).unwrap();
        spool.put(&msg(2)).unwrap();
        spool.put(&msg(3)).unwrap();
        let left: Vec<_> = spool
            .list()
            .iter()
            .map(|id| spool.get(id).unwrap().topic)
            .collect();
        assert_eq!(left, vec!["t/2", "t/3"]);
        let _ = fs::remove_dir_all(&dir);
    }
}

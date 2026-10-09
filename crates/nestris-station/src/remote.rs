//! Configuration set from NestrisLTM (`station_config` commands).
//!
//! NestrisLTM sends the complete set of its overrides; the station checks
//! them against [`ALLOWED`], validates the merged config and stores them in
//! `<state_dir>/remote.json` (`/etc` is read-only for the service). They
//! apply between the config file and the environment:
//! defaults ← `station.toml` ← `remote.json` ← env ← `--set`.
//!
//! Keys that could cut the station off from the host (station id, broker,
//! host URL and token, state paths, updates) are never remote-settable, so a
//! bad remote config can always be corrected remotely. A remote file that no
//! longer validates at startup is set aside as `remote.json.rejected` and the
//! station runs on its local config.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const FILE: &str = "remote.json";

/// Remote-settable keys: exact paths, or prefixes ending in `.`.
pub const ALLOWED: &[&str] = &[
    "station.name",
    "capture.",
    "rfid.enabled",
    "rfid.port",
    "mqtt.live_max_hz",
    "mqtt.live_playfield",
    "mqtt.status_interval_s",
    "recording.enabled",
    "recording.gzip",
    "recording.keep_days",
    "recording.max_gb",
    "session.",
    "integrity.",
    "engine.",
    "log.level",
];

/// The stored remote overrides.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct RemoteDoc {
    /// NestrisLTM's revision of this set.
    pub rev: u64,
    /// Nested like the config file (`{"capture": {"scale_width": 480}}`).
    pub values: Value,
}

pub fn path(state_dir: &Path) -> PathBuf {
    state_dir.join(FILE)
}

/// Dotted paths of every leaf in `values` (an empty table is a leaf).
pub fn leaf_paths(values: &Value) -> Vec<String> {
    fn walk(value: &Value, prefix: &str, out: &mut Vec<String>) {
        match value {
            Value::Object(map) if !map.is_empty() => {
                for (key, sub) in map {
                    let here = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{prefix}.{key}")
                    };
                    walk(sub, &here, out);
                }
            }
            _ if !prefix.is_empty() => out.push(prefix.to_string()),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(values, "", &mut out);
    out
}

pub fn is_allowed(key: &str) -> bool {
    ALLOWED
        .iter()
        .any(|pattern| match pattern.strip_suffix('.') {
            Some(prefix) => key.starts_with(*pattern) && key.len() > prefix.len() + 1,
            None => key == *pattern,
        })
}

/// Reject a set that is not a table or touches a key outside [`ALLOWED`].
pub fn check_allowed(values: &Value) -> Result<()> {
    if !values.is_object() {
        bail!("remote config values must be a table");
    }
    let denied: Vec<String> = leaf_paths(values)
        .into_iter()
        .filter(|k| !is_allowed(k))
        .collect();
    if !denied.is_empty() {
        bail!("not settable remotely: {}", denied.join(", "));
    }
    Ok(())
}

/// Read the stored set; `None` when there is none.
pub fn read(path: &Path) -> Option<Result<RemoteDoc>> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => return Some(Err(e).with_context(|| format!("read {}", path.display()))),
    };
    Some(serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display())))
}

/// Store atomically; the previous set stays as `remote.json.bak`.
pub fn write(path: &Path, doc: &RemoteDoc) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(doc)?)
        .with_context(|| format!("write {}", tmp.display()))?;
    if path.exists() {
        let _ = std::fs::copy(path, path.with_extension("json.bak"));
    }
    std::fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

/// Set a stored set aside that no longer validates.
pub fn set_aside(path: &Path) {
    let _ = std::fs::rename(path, path.with_extension("json.rejected"));
}

/// Keys pinned by the environment or `--set` (they win over remote values).
pub fn locked_keys(overrides: &[String]) -> Vec<String> {
    let mut keys: Vec<String> = overrides
        .iter()
        .filter_map(|a| a.split_once('=').map(|(k, _)| k.trim().to_string()))
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

/// What the station reports about its remote config.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RemoteStatus {
    /// Revision of the set in effect (or the one rejected).
    pub rev: Option<u64>,
    /// `none`, `applied`, `pending` (applies after the running game),
    /// `restarting`, `rejected`.
    pub state: &'static str,
    pub error: Option<String>,
    /// The set in effect.
    pub values: Value,
}

impl RemoteStatus {
    pub fn none() -> Self {
        Self {
            rev: None,
            state: "none",
            error: None,
            values: Value::Object(Default::default()),
        }
    }
}

/// One capture device with the formats and sizes ffmpeg reports for it.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct CaptureDevice {
    pub path: String,
    pub formats: Vec<CaptureFormat>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct CaptureFormat {
    /// `-input_format` name, e.g. `mjpeg`, `yuyv422`.
    pub format: String,
    pub sizes: Vec<String>,
}

/// Parse `ffmpeg -f v4l2 -list_formats all -i <dev>` output, e.g.
/// `[video4linux2,v4l2 @ 0x..] Compressed:  mjpeg : Motion-JPEG : 720x576 640x480`.
pub fn parse_v4l2_formats(text: &str) -> Vec<CaptureFormat> {
    let mut out: Vec<CaptureFormat> = Vec::new();
    for line in text.lines() {
        let Some(rest) = line
            .split_once("Compressed:")
            .or_else(|| line.split_once("Raw       :"))
            .or_else(|| line.split_once("Raw:"))
            .map(|(_, r)| r)
        else {
            continue;
        };
        let parts: Vec<&str> = rest.split(" : ").map(str::trim).collect();
        let (Some(format), Some(sizes)) = (parts.first(), parts.last()) else {
            continue;
        };
        if parts.len() < 3 || format.is_empty() {
            continue;
        }
        let sizes: Vec<String> = sizes
            .split_whitespace()
            .filter(|s| s.contains('x') || s.starts_with('{'))
            .map(str::to_string)
            .collect();
        out.push(CaptureFormat {
            format: format.to_string(),
            sizes,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn allowlist() {
        for key in [
            "station.name",
            "capture.device",
            "capture.scale_width",
            "engine.calibration.acquire_downscale_width",
            "session.min_game_frames",
            "mqtt.live_max_hz",
            "log.level",
        ] {
            assert!(is_allowed(key), "{key}");
        }
        for key in [
            "station.id",
            "station.state_dir",
            "mqtt.host",
            "mqtt.password",
            "host.token",
            "update.enabled",
            "spool.dir",
            "recording.dir",
            "capture",
            "capture.",
        ] {
            assert!(!is_allowed(key), "{key}");
        }
    }

    #[test]
    fn checks_every_leaf() {
        let ok = json!({"capture": {"scale_width": 480, "lowres": 1}, "station": {"name": "A"}});
        assert!(check_allowed(&ok).is_ok());
        let bad = json!({"capture": {"fps": 50}, "mqtt": {"host": "evil", "live_max_hz": 30}});
        let err = check_allowed(&bad).unwrap_err().to_string();
        assert!(
            err.contains("mqtt.host") && !err.contains("live_max_hz"),
            "{err}"
        );
        assert!(check_allowed(&json!([1])).is_err());
        assert!(check_allowed(&json!({})).is_ok());
    }

    #[test]
    fn leaf_paths_flatten() {
        let v = json!({"a": {"b": 1, "c": {"d": true}}, "e": {}});
        let mut paths = leaf_paths(&v);
        paths.sort();
        assert_eq!(paths, ["a.b", "a.c.d", "e"]);
    }

    #[test]
    fn write_read_and_set_aside() {
        let dir = std::env::temp_dir().join(format!("nestris-remote-{}", std::process::id()));
        let file = path(&dir);
        assert!(read(&file).is_none());
        let doc = RemoteDoc {
            rev: 3,
            values: json!({"capture": {"lowres": 1}}),
        };
        write(&file, &doc).unwrap();
        write(
            &file,
            &RemoteDoc {
                rev: 4,
                ..doc.clone()
            },
        )
        .unwrap();
        assert_eq!(read(&file).unwrap().unwrap().rev, 4);
        let bak: RemoteDoc =
            serde_json::from_slice(&std::fs::read(file.with_extension("json.bak")).unwrap())
                .unwrap();
        assert_eq!(bak, doc);
        set_aside(&file);
        assert!(read(&file).is_none());
        assert!(file.with_extension("json.rejected").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn locked_from_overrides() {
        let keys = locked_keys(&[
            "station.id=s1".into(),
            "capture.fps=50".into(),
            "station.id=s2".into(),
        ]);
        assert_eq!(keys, ["capture.fps", "station.id"]);
    }

    #[test]
    fn v4l2_formats() {
        let text = "\
[video4linux2,v4l2 @ 0x55] Compressed:       mjpeg :          Motion-JPEG : 1920x1080 1280x720 720x576
[video4linux2,v4l2 @ 0x55] Raw       :     yuyv422 :           YUYV 4:2:2 : 720x576 640x480
/dev/video0: Immediate exit requested";
        let formats = parse_v4l2_formats(text);
        assert_eq!(formats.len(), 2);
        assert_eq!(formats[0].format, "mjpeg");
        assert_eq!(formats[0].sizes, ["1920x1080", "1280x720", "720x576"]);
        assert_eq!(formats[1].format, "yuyv422");
        assert_eq!(formats[1].sizes, ["720x576", "640x480"]);
    }
}

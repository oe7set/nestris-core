//! Station configuration: `/etc/nestris-station/station.toml`, then
//! `NESTRIS_STATION__SECTION__FIELD` environment overrides, then `--set`.
//!
//! The file is deep-merged over the station defaults (not deserialized on its
//! own), so a partial `[engine.fusion]` table keeps the station's engine
//! defaults for everything it doesn't mention. Unknown keys are errors.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use nestris_engine::config::EngineConfig;
use nestris_engine::integrity::IntegrityConfig;
use nestris_host::capture_ffmpeg::LiveOptions;
use nestris_host::capture_supervisor::SupervisorConfig;
use nestris_host::config_overlay;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const ENV_PREFIX: &str = "NESTRIS_STATION";
pub const DEFAULT_CONFIG: &str = "/etc/nestris-station/station.toml";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct StationSection {
    /// Unique per station; part of every MQTT topic and game id.
    pub id: String,
    /// Human-readable label (status payload only).
    pub name: String,
    /// Base directory for recordings and the MQTT spool. systemd's
    /// `StateDirectory=` (`$STATE_DIRECTORY`) wins when this is empty.
    pub state_dir: String,
}

impl Default for StationSection {
    fn default() -> Self {
        Self {
            id: "station-1".into(),
            name: String::new(),
            state_dir: String::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureSection {
    /// `/dev/v4l/by-id/...` (or `v4l2:<path>`), `dshow:<name>` on Windows,
    /// or `file:<path>` for tests.
    pub device: String,
    /// Device pixel format (`mjpeg`, `yuyv422`, ...); empty = driver default.
    pub input_format: String,
    /// Requested capture size / rate; 0 = driver default.
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// Frames are scaled to this size for the engine.
    pub scale_width: usize,
    pub scale_height: usize,
    /// No frame for this long restarts the capture.
    pub stall_timeout_s: f64,
    /// Maximum restart backoff.
    pub backoff_max_s: f64,
    /// `file:` sources only: play at native speed / loop forever.
    pub pace_files: bool,
    pub loop_files: bool,
}

impl Default for CaptureSection {
    fn default() -> Self {
        Self {
            device: String::new(),
            input_format: "mjpeg".into(),
            width: 1280,
            height: 720,
            fps: 60.0,
            scale_width: 1280,
            scale_height: 720,
            stall_timeout_s: 5.0,
            backoff_max_s: 30.0,
            pace_files: true,
            loop_files: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RfidSection {
    pub enabled: bool,
    /// Serial port, e.g. `/dev/serial/by-id/usb-Silicon_Labs_CP2102...`.
    pub port: String,
    pub baud: u32,
    /// No message from the reader for this long = offline (it reports every 750 ms).
    pub stale_after_s: f64,
    /// A card removed within this many seconds before a game starts still
    /// counts as that game's player.
    pub player_grace_s: f64,
}

impl Default for RfidSection {
    fn default() -> Self {
        Self {
            enabled: true,
            port: String::new(),
            baud: 115_200,
            stale_after_s: 3.0,
            player_grace_s: 60.0,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct MqttSection {
    pub host: String,
    pub port: u16,
    /// Empty = `nestris-<station.id>`.
    pub client_id: String,
    pub username: String,
    /// Prefer `password_file` or the environment; never logged.
    pub password: String,
    pub password_file: String,
    /// TLS (needs the `tls` build feature); `ca_file` = PEM CA bundle.
    pub tls: bool,
    pub ca_file: String,
    pub topic_prefix: String,
    pub keepalive_s: u64,
    /// Upper rate for the `live` topic (published on change only).
    pub live_max_hz: f64,
    pub status_interval_s: f64,
    pub reconnect_max_s: f64,
}

impl Default for MqttSection {
    fn default() -> Self {
        Self {
            host: "localhost".into(),
            port: 1883,
            client_id: String::new(),
            username: String::new(),
            password: String::new(),
            password_file: String::new(),
            tls: false,
            ca_file: String::new(),
            topic_prefix: "retroverse/nestris".into(),
            keepalive_s: 15,
            live_max_hz: 5.0,
            status_interval_s: 10.0,
            reconnect_max_s: 30.0,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RecordingSection {
    pub enabled: bool,
    /// Empty = `<state_dir>/recordings`.
    pub dir: String,
    pub gzip: bool,
    /// Pruning (0 = keep forever / unlimited).
    pub keep_days: u64,
    pub max_gb: f64,
}

impl Default for RecordingSection {
    fn default() -> Self {
        Self {
            enabled: true,
            dir: String::new(),
            gzip: true,
            keep_days: 30,
            max_gb: 20.0,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct SpoolSection {
    /// Empty = `<state_dir>/spool`.
    pub dir: String,
    /// Oldest messages are discarded beyond this many unsent files.
    pub max_files: usize,
}

impl Default for SpoolSection {
    fn default() -> Self {
        Self {
            dir: String::new(),
            max_files: 10_000,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionSection {
    /// Frames of game-over / menu screens before a game counts as ended.
    pub end_confirm_frames: u32,
    /// Games shorter than this many in-game frames are discarded (false starts).
    pub min_game_frames: u64,
    /// A game is closed after this long without a usable picture.
    pub signal_lost_end_s: f64,
}

impl Default for SessionSection {
    fn default() -> Self {
        Self {
            end_confirm_frames: 30,
            min_game_frames: 120,
            signal_lost_end_s: 30.0,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LogSection {
    /// `error`, `warn`, `info`, `debug`, `trace` or a full filter directive.
    pub level: String,
}

impl Default for LogSection {
    fn default() -> Self {
        Self {
            level: "info".into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct StationConfig {
    pub station: StationSection,
    pub capture: CaptureSection,
    pub rfid: RfidSection,
    pub mqtt: MqttSection,
    pub recording: RecordingSection,
    pub spool: SpoolSection,
    pub session: SessionSection,
    pub integrity: IntegrityConfig,
    pub engine: EngineConfig,
    pub log: LogSection,
}

impl Default for StationConfig {
    fn default() -> Self {
        Self {
            station: StationSection::default(),
            capture: CaptureSection::default(),
            rfid: RfidSection::default(),
            mqtt: MqttSection::default(),
            recording: RecordingSection::default(),
            spool: SpoolSection::default(),
            session: SessionSection::default(),
            integrity: IntegrityConfig::default(),
            engine: station_engine_defaults(),
            log: LogSection::default(),
        }
    }
}

/// Engine defaults for an unattended capture-card station: background
/// solves for acquisition too, and downscaled candidate detection, so the
/// pipeline never stalls while searching for the picture.
fn station_engine_defaults() -> EngineConfig {
    let mut cfg = EngineConfig::default();
    cfg.calibration.background_recalibration = true;
    cfg.calibration.background_acquisition = true;
    cfg.calibration.acquire_downscale_width = 640;
    cfg
}

impl StationConfig {
    /// Load: defaults ← file (deep merge) ← environment ← `--set`.
    pub fn load(path: Option<&Path>, overrides: &[String]) -> Result<StationConfig> {
        let defaults = serde_json::to_value(StationConfig::default())?;
        let mut value = defaults.clone();
        if let Some(path) = path {
            let file = config_overlay::file_to_value(path)?;
            check_keys(&defaults, &file, "")?;
            merge(&mut value, file);
        }
        let mut all = config_overlay::env_overrides(ENV_PREFIX);
        all.extend(overrides.iter().cloned());
        config_overlay::apply_overrides::<StationConfig>(&mut value, &all)?;
        let cfg: StationConfig =
            serde_json::from_value(value).context("config did not match the station schema")?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        let id = &self.station.id;
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!("station.id must be non-empty [A-Za-z0-9_-], got {id:?}");
        }
        if self.capture.device.trim().is_empty() {
            bail!("capture.device is required (e.g. /dev/v4l/by-id/...-video-index0)");
        }
        if self.rfid.enabled && self.rfid.port.trim().is_empty() {
            bail!("rfid.port is required when rfid.enabled = true");
        }
        if self.mqtt.host.trim().is_empty() {
            bail!("mqtt.host is required");
        }
        if self.mqtt.topic_prefix.contains(['#', '+']) {
            bail!("mqtt.topic_prefix must not contain wildcards");
        }
        if self.mqtt.tls && !cfg!(feature = "tls") {
            bail!("mqtt.tls = true needs a build with the `tls` feature");
        }
        if self.capture.stall_timeout_s <= 0.0 || self.mqtt.live_max_hz <= 0.0 {
            bail!("capture.stall_timeout_s and mqtt.live_max_hz must be positive");
        }
        Ok(())
    }

    /// The ffmpeg input string for `capture.device`.
    pub fn capture_input(&self) -> String {
        let device = self.capture.device.trim();
        if let Some(file) = device.strip_prefix("file:") {
            file.to_string()
        } else if device.starts_with("v4l2:") || device.starts_with("dshow:") {
            device.to_string()
        } else {
            format!("v4l2:{device}")
        }
    }

    pub fn supervisor(&self) -> SupervisorConfig {
        let c = &self.capture;
        let mut sup = SupervisorConfig::new(self.capture_input());
        sup.live = LiveOptions {
            input_format: (!c.input_format.is_empty()).then(|| c.input_format.clone()),
            capture_width: c.width,
            capture_height: c.height,
            fps: (c.fps > 0.0).then_some(c.fps),
            width: c.scale_width,
            height: c.scale_height,
        };
        sup.stall_timeout = Duration::from_secs_f64(c.stall_timeout_s);
        sup.backoff_max = Duration::from_secs_f64(c.backoff_max_s.max(1.0));
        sup.pace_files = c.pace_files;
        sup.loop_files = c.loop_files;
        sup
    }

    pub fn state_dir(&self) -> PathBuf {
        if !self.station.state_dir.is_empty() {
            return PathBuf::from(&self.station.state_dir);
        }
        // systemd StateDirectory= exports the absolute path(s), ':'-separated.
        if let Ok(dir) = std::env::var("STATE_DIRECTORY")
            && let Some(first) = dir.split(':').next().filter(|s| !s.is_empty())
        {
            return PathBuf::from(first);
        }
        if cfg!(unix) {
            PathBuf::from("/var/lib/nestris-station")
        } else {
            PathBuf::from("nestris-station-data")
        }
    }

    pub fn recording_dir(&self) -> PathBuf {
        or_sub(&self.recording.dir, self.state_dir(), "recordings")
    }

    pub fn spool_dir(&self) -> PathBuf {
        or_sub(&self.spool.dir, self.state_dir(), "spool")
    }

    pub fn client_id(&self) -> String {
        if self.mqtt.client_id.is_empty() {
            format!("nestris-{}", self.station.id)
        } else {
            self.mqtt.client_id.clone()
        }
    }

    /// `<prefix>/<station id>` — every topic lives below it.
    pub fn topic_base(&self) -> String {
        format!(
            "{}/{}",
            self.mqtt.topic_prefix.trim_end_matches('/'),
            self.station.id
        )
    }

    /// The MQTT password: `password_file` wins over `password`.
    pub fn mqtt_password(&self) -> Result<Option<String>> {
        if !self.mqtt.password_file.is_empty() {
            let raw = std::fs::read_to_string(&self.mqtt.password_file)
                .with_context(|| format!("read {}", self.mqtt.password_file))?;
            return Ok(Some(raw.trim_end_matches(['\r', '\n']).to_string()));
        }
        Ok((!self.mqtt.password.is_empty()).then(|| self.mqtt.password.clone()))
    }

    /// Resolved config for display, secrets masked.
    pub fn masked(&self) -> StationConfig {
        let mut cfg = self.clone();
        if !cfg.mqtt.password.is_empty() {
            cfg.mqtt.password = "********".into();
        }
        cfg
    }
}

fn or_sub(explicit: &str, base: PathBuf, sub: &str) -> PathBuf {
    if explicit.is_empty() {
        base.join(sub)
    } else {
        PathBuf::from(explicit)
    }
}

/// Recursively overlay `patch` onto `base` (tables merge, everything else replaces).
fn merge(base: &mut Value, patch: Value) {
    match (base, patch) {
        (Value::Object(base), Value::Object(patch)) => {
            for (key, value) in patch {
                match base.get_mut(&key) {
                    Some(slot) => merge(slot, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (slot, value) => *slot = value,
    }
}

/// Reject keys the schema doesn't know (a typo must not be silently ignored).
/// `null` schema entries (unset `Option`s) accept any value.
fn check_keys(schema: &Value, file: &Value, path: &str) -> Result<()> {
    let (Value::Object(schema), Value::Object(file)) = (schema, file) else {
        return Ok(());
    };
    for (key, value) in file {
        let here = if path.is_empty() {
            key.clone()
        } else {
            format!("{path}.{key}")
        };
        match schema.get(key) {
            Some(sub) => check_keys(sub, value, &here)?,
            None => {
                let known = schema.keys().cloned().collect::<Vec<_>>().join(", ");
                bail!("unknown config key {here:?} (known here: {known})");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(name: &str, contents: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("nestris-station-config-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::File::create(&path)
            .unwrap()
            .write_all(contents.as_bytes())
            .unwrap();
        path
    }

    const MINIMAL: &str = "[capture]\ndevice = \"/dev/video0\"\n[rfid]\nport = \"/dev/ttyUSB0\"\n";

    #[test]
    fn example_config_parses() {
        let example = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/packaging/station.example.toml"
        );
        let cfg = StationConfig::load(Some(Path::new(example)), &[]).unwrap();
        assert!(cfg.capture_input().starts_with("v4l2:/dev/"));
        assert_eq!(cfg.mqtt.topic_prefix, "retroverse/nestris");
        assert_eq!(cfg.integrity.cheat_points, 10_000);
    }

    #[test]
    fn partial_engine_table_keeps_station_engine_defaults() {
        let path = write_temp(
            "partial.toml",
            &format!("{MINIMAL}[engine.fusion]\nvote_window = 7\n"),
        );
        let cfg = StationConfig::load(Some(&path), &[]).unwrap();
        assert_eq!(cfg.engine.fusion.vote_window, 7);
        assert!(cfg.engine.calibration.background_acquisition);
        assert_eq!(cfg.engine.calibration.acquire_downscale_width, 640);
    }

    #[test]
    fn unknown_file_key_is_an_error() {
        let path = write_temp("typo.toml", &format!("{MINIMAL}[mqtt]\nhots = \"x\"\n"));
        let err = StationConfig::load(Some(&path), &[]).unwrap_err();
        assert!(format!("{err:#}").contains("mqtt.hots"));
    }

    #[test]
    fn set_overrides_win_and_are_validated() {
        let path = write_temp("set.toml", MINIMAL);
        let cfg = StationConfig::load(
            Some(&path),
            &["mqtt.host=10.1.1.1".into(), "station.id=buehne-2".into()],
        )
        .unwrap();
        assert_eq!(cfg.mqtt.host, "10.1.1.1");
        assert_eq!(cfg.topic_base(), "retroverse/nestris/buehne-2");
        assert_eq!(cfg.client_id(), "nestris-buehne-2");
        assert!(StationConfig::load(Some(&path), &["mqtt.hots=1".into()]).is_err());
    }

    #[test]
    fn missing_device_is_an_error() {
        assert!(StationConfig::load(None, &[]).is_err());
    }

    #[test]
    fn device_prefixes() {
        let mut cfg = StationConfig::default();
        cfg.capture.device = "file:/tmp/x.mp4".into();
        assert_eq!(cfg.capture_input(), "/tmp/x.mp4");
        cfg.capture.device = "dshow:USB Video".into();
        assert_eq!(cfg.capture_input(), "dshow:USB Video");
        cfg.capture.device = "/dev/video2".into();
        assert_eq!(cfg.capture_input(), "v4l2:/dev/video2");
    }

    #[test]
    fn password_is_masked() {
        let mut cfg = StationConfig::default();
        cfg.mqtt.password = "secret".into();
        assert_eq!(cfg.masked().mqtt.password, "********");
    }
}

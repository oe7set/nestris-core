//! Updates of the station package and of its RFID reader, started from
//! NestrisLTM (`docs/STATION.md`, "Updates"; plan in
//! `nestris-ltm/docs/UPDATES.md`).
//!
//! NestrisLTM publishes on `<base>/cmd`:
//!
//! ```json
//! {"type":"update","target":"station","release":"v0.3.0","version":"0.2.1"}
//! {"type":"update","target":"reader","release":"v1.1.0","version":"1.1.0","mode":"app"}
//! ```
//!
//! The station downloads the release files from the host (which caches the
//! GitHub release; stations need no internet), verifies the Ed25519
//! signature of `SHA256SUMS.txt` against the compiled-in release key and the
//! checksum of every file, then:
//!
//! - **station**: leaves the `.deb` in `<state_dir>/updates/` and writes the
//!   `request` file; the systemd path unit `nestris-station-update.path`
//!   starts the root helper, which verifies again (`nestris-station
//!   verify-update`), runs `apt-get install` and restarts the station. The
//!   helper's `result` file is reported after the restart.
//! - **reader**: the station closes the reader's port, runs esptool with the
//!   image and offset from the signed release manifest and waits for the
//!   reader's `hello` with the new version.
//!
//! Progress goes out on the retained topic `<base>/update`.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tracing::{info, warn};

/// Accepted release-signing public keys (raw Ed25519, base64); the same list
/// as `nestris-ltm/src/nestris_ltm/core/releases.py`.
pub const RELEASE_PUBLIC_KEYS: [&str; 1] = ["CQqYvIf/DIFS0ctwZaWQEK2wCdG7Oy3jN+YkF7nRgNQ="];
pub const STATION_REPO: &str = "nestris-core";
pub const READER_REPO: &str = "nestris-rfid-reader";
pub const SUMS: &str = "SHA256SUMS.txt";
pub const SIG: &str = "SHA256SUMS.txt.sig";
pub const REQUEST_FILE: &str = "request";
pub const RESULT_FILE: &str = "result";
const MAX_DOWNLOAD: u64 = 200 * 1024 * 1024;
const APP_OFFSET_MIN: u32 = 0x10000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    Station,
    Reader,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub target: Target,
    /// Release tag, e.g. `v0.3.0` (names the download folder on the host).
    pub release: String,
    /// Version of the station package / reader firmware in that release.
    pub version: String,
    /// Reader only: the factory image (offset 0, resets the reader's settings).
    pub factory: bool,
}

fn safe_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 40
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'))
        && !s.starts_with('.')
}

impl Request {
    pub fn parse(cmd: &Value) -> Result<Request, String> {
        let text = |key: &str| cmd.get(key).and_then(Value::as_str).unwrap_or_default();
        let target = match text("target") {
            "station" => Target::Station,
            "reader" => Target::Reader,
            other => return Err(format!("unknown update target {other:?}")),
        };
        let (release, version) = (text("release"), text("version"));
        if !safe_token(release) || !safe_token(version) {
            return Err("release and version must be plain version strings".into());
        }
        let factory = match text("mode") {
            "" | "app" => false,
            "factory" => true,
            other => return Err(format!("unknown mode {other:?}")),
        };
        Ok(Request {
            target,
            release: release.into(),
            version: version.trim_start_matches('v').into(),
            factory,
        })
    }

    pub fn repo(&self) -> &'static str {
        match self.target {
            Target::Station => STATION_REPO,
            Target::Reader => READER_REPO,
        }
    }
}

/// Payload of the retained `<base>/update` topic.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct UpdateEvent {
    pub station: String,
    pub target: Target,
    pub version: String,
    /// `downloading`, `verifying`, `installing`, `flashing`, `waiting`, `done`, `failed`.
    pub state: &'static str,
    pub detail: Option<String>,
    pub progress: Option<f64>,
    pub ts: String,
}

// ---------------------------------------------------------------- verification

/// True if `signature_b64` signs `data` with one of the release keys.
pub fn verify_signature(data: &[u8], signature_b64: &str, keys: &[&str]) -> bool {
    let b64 = base64::engine::general_purpose::STANDARD;
    let Ok(raw) = b64.decode(signature_b64.trim()) else {
        return false;
    };
    let Ok(signature) = Signature::from_slice(&raw) else {
        return false;
    };
    keys.iter().any(|key| {
        b64.decode(key)
            .ok()
            .and_then(|k| <[u8; 32]>::try_from(k).ok())
            .and_then(|k| VerifyingKey::from_bytes(&k).ok())
            .is_some_and(|k| k.verify(data, &signature).is_ok())
    })
}

/// `SHA256SUMS.txt` → {file name: lowercase sha256}; bad lines are skipped.
pub fn parse_sums(text: &str) -> BTreeMap<String, String> {
    let mut sums = BTreeMap::new();
    for line in text.lines() {
        let mut parts = line.trim().splitn(2, char::is_whitespace);
        let (Some(hash), Some(name)) = (parts.next(), parts.next()) else {
            continue;
        };
        if hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
            let name = name.trim().trim_start_matches('*');
            sums.insert(name.to_string(), hash.to_ascii_lowercase());
        }
    }
    sums
}

pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Debian architecture of this build.
pub fn deb_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "arm" => "armhf",
        other => other,
    }
}

/// The station package for `version` and `arch` in a release's checksum
/// list: `nestris-station_<v>_<arch>.deb` or with a Debian revision
/// (`nestris-station_<v>-1_<arch>.deb`, what cargo-deb writes).
pub fn find_deb(sums: &BTreeMap<String, String>, version: &str, arch: &str) -> Option<String> {
    let prefix = format!("nestris-station_{version}");
    let suffix = format!("_{arch}.deb");
    sums.keys()
        .find(|name| {
            name.strip_prefix(&prefix)
                .and_then(|rest| rest.strip_suffix(&suffix))
                .is_some_and(|rev| {
                    rev.is_empty()
                        || (rev.starts_with('-') && rev[1..].chars().all(|c| c.is_ascii_digit()))
                })
        })
        .cloned()
}

/// Image file and flash offset from a reader release manifest
/// (`nestris-rfid-reader/tools/make_release.py`).
pub fn parse_manifest(text: &str, factory: bool) -> Result<(String, u32), String> {
    let manifest: Value = serde_json::from_str(text).map_err(|e| format!("manifest: {e}"))?;
    let kind = if factory { "factory" } else { "app" };
    if let Some(chip) = manifest.get("chip").and_then(Value::as_str)
        && chip != "esp32"
    {
        return Err(format!("firmware is for {chip}, not esp32"));
    }
    let entry = manifest
        .get("files")
        .and_then(Value::as_array)
        .and_then(|files| {
            files
                .iter()
                .find(|f| f.get("kind").and_then(Value::as_str) == Some(kind))
        })
        .ok_or_else(|| format!("manifest has no '{kind}' image"))?;
    let name = entry
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let offset_text = entry
        .get("offset")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let offset = u32::from_str_radix(offset_text.trim_start_matches("0x"), 16)
        .map_err(|_| format!("bad offset {offset_text:?}"))?;
    if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
        return Err(format!("bad file name {name:?}"));
    }
    // Only the two layouts the reader uses.
    if (factory && offset != 0) || (!factory && offset < APP_OFFSET_MIN) {
        return Err(format!("unexpected offset {offset:#x} for '{kind}'"));
    }
    Ok((name.to_string(), offset))
}

/// `nestris-station verify-update <dir> <file>`: the root helper's check of
/// a downloaded package (signature of the checksum list, then the file).
pub fn verify_files(dir: &Path, file: &str) -> Result<()> {
    if file.contains(['/', '\\']) {
        bail!("bad file name {file:?}");
    }
    let sums = fs::read(dir.join(SUMS)).context("read SHA256SUMS.txt")?;
    let sig = fs::read_to_string(dir.join(SIG)).context("read SHA256SUMS.txt.sig")?;
    if !verify_signature(&sums, &sig, &RELEASE_PUBLIC_KEYS) {
        bail!("SHA256SUMS.txt is not signed by the release key");
    }
    let expected = parse_sums(&String::from_utf8_lossy(&sums))
        .remove(file)
        .with_context(|| format!("{file} is not in SHA256SUMS.txt"))?;
    let actual = sha256_file(&dir.join(file)).with_context(|| format!("read {file}"))?;
    if actual != expected {
        bail!("{file}: checksum mismatch");
    }
    Ok(())
}

/// The root helper's report from the last station update, removed once read.
pub fn take_result(dir: &Path) -> Option<(bool, String, String)> {
    let path = dir.join(RESULT_FILE);
    let text = fs::read_to_string(&path).ok()?;
    let _ = fs::remove_file(&path);
    let v: Value = serde_json::from_str(&text).ok()?;
    Some((
        v.get("ok").and_then(Value::as_bool).unwrap_or(false),
        v.get("version")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        v.get("detail")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    ))
}

// ---------------------------------------------------------------- the worker

/// Messages from the update threads to the main loop.
#[derive(Debug)]
pub enum Msg {
    State {
        state: &'static str,
        detail: Option<String>,
        progress: Option<f64>,
    },
    /// Reader image verified: the main loop frees the port and flashes.
    Flash {
        image: PathBuf,
        offset: u32,
    },
    /// esptool finished.
    Flashed(Result<(), String>),
    /// The request file for the root helper is written.
    Requested,
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct HostAccess {
    pub base_url: String,
    pub station: String,
    pub token: String,
    pub timeout: Duration,
}

impl HostAccess {
    fn url(&self, repo: &str, release: &str, file: &str) -> String {
        format!(
            "{}/api/stations/{}/updates/{repo}/{release}/{file}",
            self.base_url.trim_end_matches('/'),
            self.station
        )
    }

    fn get(&self, agent: &ureq::Agent, repo: &str, release: &str, file: &str) -> Result<Vec<u8>> {
        let url = self.url(repo, release, file);
        let mut response = agent
            .get(&url)
            .header("Authorization", &format!("Bearer {}", self.token))
            .call()
            .with_context(|| format!("GET {file}"))?;
        let status = response.status().as_u16();
        if status != 200 {
            bail!("host answered {status} for {file}");
        }
        response
            .body_mut()
            .with_config()
            .limit(MAX_DOWNLOAD)
            .read_to_vec()
            .with_context(|| format!("download {file}"))
    }
}

/// Download and verify in a thread; reports through `tx`.
pub fn spawn(req: Request, host: HostAccess, dir: PathBuf, tx: Sender<Msg>) {
    let thread = std::thread::Builder::new()
        .name("update".into())
        .spawn(move || {
            if let Err(e) = work(&req, &host, &dir, &tx) {
                let _ = tx.send(Msg::Failed(format!("{e:#}")));
            }
        });
    if let Err(e) = thread {
        warn!(error = %e, "could not start the update thread");
    }
}

fn state(tx: &Sender<Msg>, state: &'static str, detail: impl Into<String>) {
    let _ = tx.send(Msg::State {
        state,
        detail: Some(detail.into()),
        progress: None,
    });
}

fn work(req: &Request, host: &HostAccess, dir: &Path, tx: &Sender<Msg>) -> Result<()> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(host.timeout))
        .http_status_as_error(false)
        .build()
        .into();
    let repo = req.repo();
    fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    // A stale request must never be picked up with new files.
    let _ = fs::remove_file(dir.join(REQUEST_FILE));

    state(tx, "downloading", SUMS);
    let sums_raw = host.get(&agent, repo, &req.release, SUMS)?;
    let sig = host.get(&agent, repo, &req.release, SIG)?;
    state(tx, "verifying", "signature");
    if !verify_signature(
        &sums_raw,
        &String::from_utf8_lossy(&sig),
        &RELEASE_PUBLIC_KEYS,
    ) {
        bail!("SHA256SUMS.txt is not signed by the release key");
    }
    let sums = parse_sums(&String::from_utf8_lossy(&sums_raw));

    let fetch = |file: &str| -> Result<PathBuf> {
        let expected = sums
            .get(file)
            .with_context(|| format!("{file} is not in SHA256SUMS.txt"))?;
        state(tx, "downloading", file);
        let data = host.get(&agent, repo, &req.release, file)?;
        if &sha256_hex(&data) != expected {
            bail!("{file}: checksum mismatch");
        }
        let path = dir.join(file);
        fs::write(&path, &data).with_context(|| format!("write {}", path.display()))?;
        Ok(path)
    };

    match req.target {
        Target::Station => {
            let deb = find_deb(&sums, &req.version, deb_arch()).with_context(|| {
                format!(
                    "release {} has no nestris-station {} for {}",
                    req.release,
                    req.version,
                    deb_arch()
                )
            })?;
            fetch(&deb)?;
            fs::write(dir.join(SUMS), &sums_raw)?;
            fs::write(dir.join(SIG), &sig)?;
            // Atomic: the path unit fires on the final name only.
            let tmp = dir.join("request.tmp");
            fs::write(&tmp, format!("deb={deb}\nversion={}\n", req.version))?;
            fs::rename(&tmp, dir.join(REQUEST_FILE))?;
            info!(deb, "update requested from the root helper");
            let _ = tx.send(Msg::Requested);
        }
        Target::Reader => {
            let manifest_name = format!("nestris-rfid-reader-{}-manifest.json", req.version);
            let manifest = fetch(&manifest_name)?;
            let (image_name, offset) = parse_manifest(&fs::read_to_string(&manifest)?, req.factory)
                .map_err(anyhow::Error::msg)?;
            let image = fetch(&image_name)?;
            let _ = fs::remove_file(&manifest);
            let _ = tx.send(Msg::Flash { image, offset });
        }
    }
    Ok(())
}

/// Flash the reader with esptool in a thread (the caller has closed the port).
pub fn spawn_flash(
    esptool: String,
    port: String,
    baud: u32,
    image: PathBuf,
    offset: u32,
    tx: Sender<Msg>,
) {
    let thread = std::thread::Builder::new()
        .name("flash".into())
        .spawn(move || {
            let progress_tx = tx.clone();
            let result = run_esptool(&esptool, &port, baud, &image, offset, |p| {
                let _ = progress_tx.send(Msg::State {
                    state: "flashing",
                    detail: None,
                    progress: Some(p),
                });
            });
            let _ = fs::remove_file(&image);
            let _ = tx.send(Msg::Flashed(result));
        });
    if let Err(e) = thread {
        warn!(error = %e, "could not start the flash thread");
    }
}

/// esptool 4.x (Debian 12) and 5.x accept the underscore spellings.
pub fn esptool_args(port: &str, baud: u32, image: &Path, offset: u32) -> Vec<String> {
    vec![
        "--chip".into(),
        "esp32".into(),
        "--port".into(),
        port.into(),
        "--baud".into(),
        baud.to_string(),
        "--before".into(),
        "default_reset".into(),
        "--after".into(),
        "hard_reset".into(),
        "write_flash".into(),
        format!("{offset:#x}"),
        image.display().to_string(),
    ]
}

pub fn run_esptool(
    esptool: &str,
    port: &str,
    baud: u32,
    image: &Path,
    offset: u32,
    mut progress: impl FnMut(f64),
) -> Result<(), String> {
    let mut child = Command::new(esptool)
        .args(esptool_args(port, baud, image, offset))
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {esptool:?}: {e} (apt install esptool)"))?;
    let mut stderr = child.stderr.take();
    let err_thread = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(s) = stderr.as_mut() {
            let _ = s.read_to_string(&mut text);
        }
        text
    });
    let mut output = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let mut buf = [0u8; 512];
        let mut pending = String::new();
        let mut last = -1.0;
        while let Ok(n) = stdout.read(&mut buf) {
            if n == 0 {
                break;
            }
            pending.push_str(&String::from_utf8_lossy(&buf[..n]));
            // Progress lines are redrawn with \r.
            while let Some(pos) = pending.find(['\r', '\n']) {
                let line: String = pending.drain(..=pos).collect();
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Some(p) = percent(line)
                    && line.to_ascii_lowercase().contains("writing")
                    && p - last >= 0.05
                {
                    last = p;
                    progress(p);
                }
                output.push_str(line);
                output.push('\n');
            }
        }
    }
    let status = child.wait().map_err(|e| format!("esptool: {e}"))?;
    let stderr = err_thread.join().unwrap_or_default();
    if status.success() {
        return Ok(());
    }
    output.push_str(&stderr);
    Err(format!(
        "esptool exit {}: {}",
        status.code().unwrap_or(-1),
        esptool_error(&output)
    ))
}

/// A percentage like `(37 %)` or `37.5%` in an esptool line, as 0..1.
fn percent(line: &str) -> Option<f64> {
    let end = line.find('%')?;
    let digits: String = line[..end]
        .trim_end()
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    digits
        .parse::<f64>()
        .ok()
        .filter(|p| (0.0..=100.0).contains(p))
        .map(|p| p / 100.0)
}

/// The gist of a failed esptool run (its last error, without box drawing).
pub fn esptool_error(output: &str) -> String {
    let lines: Vec<&str> = output
        .lines()
        .map(|l| l.trim_matches(|c: char| c.is_whitespace() || "│┌┐└┘─".contains(c)))
        .filter(|l| !l.is_empty() && !l.starts_with("Error"))
        .collect();
    let pick = lines
        .iter()
        .rposition(|l| {
            let l = l.to_ascii_lowercase();
            l.contains("error") || l.contains("failed")
        })
        .map(|i| lines[i..lines.len().min(i + 3)].join(" "))
        .unwrap_or_else(|| lines[lines.len().saturating_sub(2)..].join(" "));
    pick.chars().take(300).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn key() -> (SigningKey, String) {
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let pk = base64::engine::general_purpose::STANDARD.encode(sk.verifying_key().to_bytes());
        (sk, pk)
    }

    fn sign(sk: &SigningKey, data: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(sk.sign(data).to_bytes())
    }

    #[test]
    fn signature_check() {
        let (sk, pk) = key();
        let data = b"abc  file\n";
        assert!(verify_signature(data, &sign(&sk, data), &[pk.as_str()]));
        assert!(!verify_signature(
            b"tampered",
            &sign(&sk, data),
            &[pk.as_str()]
        ));
        assert!(!verify_signature(data, "not base64!", &[pk.as_str()]));
        // The real release key does not accept another key's signature.
        assert!(!verify_signature(
            data,
            &sign(&sk, data),
            &RELEASE_PUBLIC_KEYS
        ));
    }

    #[test]
    fn real_release_signature() {
        // The published nestris-rfid-reader v1.0.0 checksum list, signed by
        // the release workflow (Python): the station must accept it.
        let sums = include_bytes!("../tests/data/reader-v1.0.0-SHA256SUMS.txt");
        let sig = include_str!("../tests/data/reader-v1.0.0-SHA256SUMS.txt.sig");
        assert!(verify_signature(sums, sig, &RELEASE_PUBLIC_KEYS));
        let mut tampered = sums.to_vec();
        tampered[0] ^= 1;
        assert!(!verify_signature(&tampered, sig, &RELEASE_PUBLIC_KEYS));
        let zero = base64::engine::general_purpose::STANDARD.encode([0u8; 64]);
        assert!(!verify_signature(sums, &zero, &RELEASE_PUBLIC_KEYS));
    }

    #[test]
    fn sums_and_debs() {
        let a = "a".repeat(64);
        let text = format!(
            "{a}  nestris-station_0.2.1-1_amd64.deb\n{a}  nestris-station_0.2.1-1_arm64.deb\n\
             {a} *nestris-station_0.2.10_amd64.deb\nbroken\n{a}  nestris-cli-v0.3.0-linux-x64.zip\n"
        );
        let sums = parse_sums(&text);
        assert_eq!(sums.len(), 4);
        assert_eq!(
            find_deb(&sums, "0.2.1", "amd64").as_deref(),
            Some("nestris-station_0.2.1-1_amd64.deb")
        );
        assert_eq!(
            find_deb(&sums, "0.2.1", "arm64").as_deref(),
            Some("nestris-station_0.2.1-1_arm64.deb")
        );
        assert_eq!(
            find_deb(&sums, "0.2.10", "amd64").as_deref(),
            Some("nestris-station_0.2.10_amd64.deb")
        );
        assert_eq!(find_deb(&sums, "0.2", "amd64"), None);
    }

    #[test]
    fn requests() {
        let ok = Request::parse(&serde_json::json!({
            "type": "update", "target": "reader", "release": "v1.1.0", "version": "v1.1.0", "mode": "factory"
        }))
        .unwrap();
        assert_eq!(ok.target, Target::Reader);
        assert_eq!(ok.version, "1.1.0");
        assert!(ok.factory);
        assert_eq!(ok.repo(), READER_REPO);
        for bad in [
            serde_json::json!({"target": "toaster", "release": "v1", "version": "1"}),
            serde_json::json!({"target": "station", "release": "../etc", "version": "1"}),
            serde_json::json!({"target": "station", "release": "v1", "version": ""}),
            serde_json::json!({"target": "reader", "release": "v1", "version": "1", "mode": "x"}),
        ] {
            assert!(Request::parse(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn manifests() {
        let m = r#"{"chip":"esp32","files":[
            {"kind":"factory","name":"f.bin","offset":"0x0"},
            {"kind":"app","name":"a.bin","offset":"0x10000"}]}"#;
        assert_eq!(parse_manifest(m, false), Ok(("a.bin".into(), 0x10000)));
        assert_eq!(parse_manifest(m, true), Ok(("f.bin".into(), 0)));
        assert!(parse_manifest(&m.replace("0x10000", "0x1000"), false).is_err());
        assert!(parse_manifest(&m.replace("a.bin", "../a.bin"), false).is_err());
        assert!(parse_manifest(&m.replace("\"esp32\"", "\"esp32s3\""), false).is_err());
        assert!(parse_manifest("nope", false).is_err());
    }

    #[test]
    fn verify_files_checks_signature_and_checksum() {
        let dir = std::env::temp_dir().join(format!("nestris-verify-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let deb = b"!<arch>\nfake deb";
        fs::write(dir.join("pkg.deb"), deb).unwrap();
        let sums = format!("{}  pkg.deb\n", sha256_hex(deb));
        fs::write(dir.join(SUMS), &sums).unwrap();
        // Signed with a test key: the compiled-in release key must refuse it.
        let (sk, _) = key();
        fs::write(dir.join(SIG), sign(&sk, sums.as_bytes())).unwrap();
        let err = verify_files(&dir, "pkg.deb").unwrap_err().to_string();
        assert!(err.contains("not signed"), "{err}");
        assert!(verify_files(&dir, "../pkg.deb").is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_files_accepts_a_real_release_file() {
        let dir = std::env::temp_dir().join(format!("nestris-real-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
        fs::copy(data.join("reader-v1.0.0-SHA256SUMS.txt"), dir.join(SUMS)).unwrap();
        fs::copy(data.join("reader-v1.0.0-SHA256SUMS.txt.sig"), dir.join(SIG)).unwrap();
        let name = "nestris-rfid-reader-1.0.0-manifest.json";
        fs::copy(data.join(name), dir.join(name)).unwrap();
        verify_files(&dir, name).unwrap();
        fs::write(dir.join(name), b"{}").unwrap();
        assert!(verify_files(&dir, name).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn esptool_output() {
        assert_eq!(percent("Writing at 0x00010000... (37 %)"), Some(0.37));
        assert_eq!(
            percent("Writing at 0x00010000 [===] 37.5% 1/2"),
            Some(0.375)
        );
        assert_eq!(percent("no number %"), None);
        let boxed = "Connecting....\n┌─ Error ───┐\n│ Failed to connect to ESP32: No serial data received. │\n└───────────┘\n";
        assert_eq!(
            esptool_error(boxed),
            "Failed to connect to ESP32: No serial data received."
        );
        assert_eq!(esptool_error("a\nb\nc"), "b c");
        let args = esptool_args("/dev/ttyUSB0", 460800, Path::new("/x/a.bin"), 0x10000);
        assert_eq!(args[10..], ["write_flash", "0x10000", "/x/a.bin"]);
    }

    #[test]
    fn helper_result_file() {
        let dir = std::env::temp_dir().join(format!("nestris-result-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(RESULT_FILE),
            r#"{"ok":true,"version":"0.2.1-1","detail":"installed"}"#,
        )
        .unwrap();
        assert_eq!(
            take_result(&dir),
            Some((true, "0.2.1-1".into(), "installed".into()))
        );
        assert_eq!(take_result(&dir), None); // removed once read
        let _ = fs::remove_dir_all(&dir);
    }
}

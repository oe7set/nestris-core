//! ESP32 RFID reader over USB serial (firmware in
//! `RFID_ESP/ESP32_CARD_READER`, protocol unchanged).
//!
//! The reader prints `{"type":"login","username":..,"uid":..,"scoreloaded":..}`
//! every 750 ms (plus plain-text boot lines, which are ignored). No card on
//! the reader is `username = "Unbekannt"` without a `uid`. Lines written to
//! the reader (`{"type":"highscore",...}`, `{"type":"setname",...}`) are
//! forwarded verbatim.
//!
//! The worker thread reconnects forever: a missing port (unplugged), an I/O
//! error or silence longer than `stale_after` all close and reopen the port.

use std::io::ErrorKind;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::config::RfidSection;

/// The reader's placeholder name for "no card" / unreadable card.
const NO_NAME: &str = "Unbekannt";
/// Longest accepted line; anything longer is garbage on the wire.
const MAX_LINE: usize = 1024;
const RECONNECT_MIN: Duration = Duration::from_secs(1);
const RECONNECT_MAX: Duration = Duration::from_secs(15);
/// Extra silence tolerated right after opening the port (ESP32 boot).
const BOOT_GRACE: Duration = Duration::from_secs(5);

/// A player identified by card.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Player {
    pub uid: String,
    /// Name stored on the card; `None` for a blank / unreadable card.
    pub name: Option<String>,
}

/// Snapshot of the reader for the main loop.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RfidSnapshot {
    pub connected: bool,
    /// The card currently on the reader.
    pub present: Option<Player>,
    /// The last card seen and when it left (or was last seen).
    pub last_seen: Option<(Player, Instant)>,
}

impl RfidSnapshot {
    /// The player to attribute a game to: the card on the reader, else a
    /// card removed less than `grace` ago.
    pub fn player_for_game(&self, grace: Duration) -> Option<Player> {
        if let Some(p) = &self.present {
            return Some(p.clone());
        }
        self.last_seen
            .as_ref()
            .filter(|(_, at)| at.elapsed() < grace)
            .map(|(p, _)| p.clone())
    }
}

#[derive(Deserialize)]
struct LoginLine {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    uid: String,
}

/// Parse one line from the reader. `Some(None)` = a login line without a
/// card; `None` = not a login line (boot text, errors, other JSON).
pub fn parse_line(line: &str) -> Option<Option<Player>> {
    let line = line.trim();
    if !line.starts_with('{') {
        return None;
    }
    let login: LoginLine = serde_json::from_str(line).ok()?;
    if login.kind != "login" {
        return None;
    }
    let uid = login.uid.trim().to_ascii_uppercase();
    if uid.is_empty() {
        return Some(None);
    }
    let name = login.username.trim();
    let name = (!name.is_empty() && name != NO_NAME).then(|| name.to_string());
    Some(Some(Player { uid, name }))
}

pub struct RfidReader {
    state: Arc<Mutex<RfidSnapshot>>,
    commands: Sender<String>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl RfidReader {
    pub fn start(cfg: RfidSection) -> RfidReader {
        let state: Arc<Mutex<RfidSnapshot>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let handle = {
            let (state, stop) = (state.clone(), stop.clone());
            std::thread::Builder::new()
                .name("rfid".into())
                .spawn(move || worker(cfg, state, rx, stop))
                .expect("spawn rfid thread")
        };
        RfidReader {
            state,
            commands: tx,
            stop,
            handle: Some(handle),
        }
    }

    pub fn snapshot(&self) -> RfidSnapshot {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Send one JSON line to the reader (dropped while disconnected).
    pub fn send(&self, line: String) {
        let _ = self.commands.send(line);
    }
}

impl Drop for RfidReader {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn worker(
    cfg: RfidSection,
    state: Arc<Mutex<RfidSnapshot>>,
    commands: Receiver<String>,
    stop: Arc<AtomicBool>,
) {
    let stale_after = Duration::from_secs_f64(cfg.stale_after_s.max(0.5));
    let mut backoff = RECONNECT_MIN;
    let mut waiting_logged = false;
    while !stop.load(Ordering::Relaxed) {
        set_connected(&state, false);
        if !Path::new(&cfg.port).exists() && !cfg.port.starts_with("COM") {
            if !waiting_logged {
                warn!(port = %cfg.port, "RFID port missing, waiting for the reader");
                waiting_logged = true;
            }
            sleep_responsive(&stop, Duration::from_millis(500));
            continue;
        }
        waiting_logged = false;
        let port = serialport::new(&cfg.port, cfg.baud)
            .timeout(Duration::from_millis(200))
            .dtr_on_open(false)
            .open();
        let mut port = match port {
            Ok(port) => port,
            Err(e) => {
                warn!(port = %cfg.port, error = %e, retry_s = backoff.as_secs(), "RFID open failed");
                sleep_responsive(&stop, backoff);
                backoff = (backoff * 2).min(RECONNECT_MAX);
                continue;
            }
        };
        // Deasserted DTR/RTS keep an ESP32 dev board out of its auto-reset.
        let _ = port.write_request_to_send(false);
        info!(port = %cfg.port, "RFID reader connected");
        let started = Instant::now();
        let reason = session(&mut *port, &state, &commands, &stop, stale_after);
        if started.elapsed() > Duration::from_secs(10) {
            backoff = RECONNECT_MIN;
        }
        if !stop.load(Ordering::Relaxed) {
            warn!(port = %cfg.port, %reason, "RFID reader disconnected, reconnecting");
            sleep_responsive(&stop, backoff);
            backoff = (backoff * 2).min(RECONNECT_MAX);
        }
    }
    set_connected(&state, false);
}

/// Read lines until an error, silence or shutdown; returns the reason.
fn session(
    port: &mut dyn serialport::SerialPort,
    state: &Arc<Mutex<RfidSnapshot>>,
    commands: &Receiver<String>,
    stop: &AtomicBool,
    stale_after: Duration,
) -> String {
    let mut line: Vec<u8> = Vec::with_capacity(128);
    let mut buf = [0u8; 256];
    // Opening the port may reset the ESP32: allow for its boot time once.
    let mut last_login = Instant::now() + BOOT_GRACE;
    let mut connected = false;
    // Commands queued while disconnected are stale (the display state they
    // targeted is gone).
    while commands.try_recv().is_ok() {}
    while !stop.load(Ordering::Relaxed) {
        while let Ok(cmd) = commands.try_recv() {
            let mut out = cmd.trim().to_string();
            out.push('\n');
            if let Err(e) = port.write_all(out.as_bytes()).and_then(|_| port.flush()) {
                return format!("write failed: {e}");
            }
            debug!(command = %cmd.trim(), "sent to RFID reader");
        }
        match port.read(&mut buf) {
            Ok(0) => return "port closed".into(),
            Ok(n) => {
                for &b in &buf[..n] {
                    if b == b'\n' {
                        let text = String::from_utf8_lossy(&line).into_owned();
                        line.clear();
                        if let Some(player) = parse_line(&text) {
                            last_login = Instant::now();
                            if !connected {
                                connected = true;
                                set_connected(state, true);
                            }
                            update_player(state, player);
                        } else if !text.trim().is_empty() {
                            debug!(line = %text.trim(), "RFID reader output");
                        }
                    } else if line.len() < MAX_LINE {
                        line.push(b);
                    } else {
                        line.clear();
                    }
                }
            }
            Err(e) if e.kind() == ErrorKind::TimedOut || e.kind() == ErrorKind::WouldBlock => {}
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return format!("read failed: {e}"),
        }
        if last_login.elapsed() > stale_after {
            return format!("no status for {:.1}s", stale_after.as_secs_f64());
        }
    }
    "shutdown".into()
}

fn set_connected(state: &Arc<Mutex<RfidSnapshot>>, connected: bool) {
    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
    s.connected = connected;
    if !connected && let Some(p) = s.present.take() {
        // An unknown card state is treated like a removed card.
        s.last_seen = Some((p, Instant::now()));
    }
}

fn update_player(state: &Arc<Mutex<RfidSnapshot>>, player: Option<Player>) {
    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
    match player {
        Some(p) => {
            if s.present.as_ref() != Some(&p) {
                info!(uid = %p.uid, name = ?p.name, "card on reader");
            }
            s.last_seen = Some((p.clone(), Instant::now()));
            s.present = Some(p);
        }
        None => {
            if let Some(p) = s.present.take() {
                info!(uid = %p.uid, "card removed");
                s.last_seen = Some((p, Instant::now()));
            }
        }
    }
}

fn sleep_responsive(stop: &AtomicBool, total: Duration) {
    let until = Instant::now() + total;
    while Instant::now() < until && !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_firmware_lines() {
        assert_eq!(
            parse_line(
                r#"{"type":"login","username":"Erv","uid":"a1b2c3d4","scoreloaded":"0","source":"rfid"}"#
            ),
            Some(Some(Player {
                uid: "A1B2C3D4".into(),
                name: Some("Erv".into())
            }))
        );
        // No card on the reader.
        assert_eq!(
            parse_line(r#"{"type":"login","username":"Unbekannt","source":"rfid"}"#),
            Some(None)
        );
        // Blank card: uid known, no name.
        assert_eq!(
            parse_line(
                r#"{"type":"login","username":"Unbekannt","uid":"0A0B","scoreloaded":"0","source":"rfid"}"#
            ),
            Some(Some(Player {
                uid: "0A0B".into(),
                name: None
            }))
        );
    }

    #[test]
    fn ignores_non_login_output() {
        assert_eq!(parse_line("Device start..."), None);
        assert_eq!(parse_line("setup finished."), None);
        assert_eq!(parse_line("JSON parse error: InvalidInput"), None);
        assert_eq!(parse_line(r#"{"type":"highscore","value":"1"}"#), None);
        // A name with an unescaped quote breaks the firmware's JSON.
        assert_eq!(
            parse_line(r#"{"type":"login","username":"a"b","uid":"01"}"#),
            None
        );
    }

    #[test]
    fn grace_period_keeps_a_removed_card() {
        let p = Player {
            uid: "01".into(),
            name: Some("A".into()),
        };
        let snap = RfidSnapshot {
            connected: true,
            present: None,
            last_seen: Some((p.clone(), Instant::now())),
        };
        assert_eq!(snap.player_for_game(Duration::from_secs(60)), Some(p));
        assert_eq!(snap.player_for_game(Duration::ZERO), None);
    }
}

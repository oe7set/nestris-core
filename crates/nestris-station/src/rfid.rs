//! Retroverse card reader over USB serial, protocol v2 (firmware and
//! contract: `nestris-rfid-reader`, `docs/PROTOCOL.md`).
//!
//! The reader introduces itself with `hello` (firmware, serial, protocol),
//! reports card changes as `card` events (`present` / `removed`) and sends a
//! `status` heartbeat every 2 s. The station sends `hello` after opening the
//! port, a `ping` every 3 s, and forwards `show` / `write` / `config`
//! commands from MQTT. A reader that does not speak protocol 2 (e.g. the old
//! v1 sketch) is reported with `protocol_error` and never counts as
//! connected.
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
use tracing::{debug, error, info, warn};

use crate::config::RfidSection;

/// The protocol version this station speaks.
pub const PROTOCOL: u32 = 2;
/// Longest accepted line; anything longer is garbage on the wire.
const MAX_LINE: usize = 1024;
const RECONNECT_MIN: Duration = Duration::from_secs(1);
const RECONNECT_MAX: Duration = Duration::from_secs(15);
/// Extra silence tolerated right after opening the port (ESP32 boot).
const BOOT_GRACE: Duration = Duration::from_secs(5);
const PING_EVERY: Duration = Duration::from_secs(3);
/// Re-ask for `hello` while the reader has not introduced itself.
const HELLO_EVERY: Duration = Duration::from_secs(2);

/// A player identified by card.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Player {
    pub uid: String,
    /// Name stored on the card; `None` for a blank / unreadable card.
    pub name: Option<String>,
}

/// What the reader told about itself in `hello`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReaderInfo {
    pub fw: String,
    pub serial: String,
    pub display: String,
    pub chip: String,
}

/// Snapshot of the reader for the main loop.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RfidSnapshot {
    /// A protocol-2 reader is connected and has introduced itself.
    pub connected: bool,
    /// The card currently on the reader.
    pub present: Option<Player>,
    /// The last card seen and when it left (or was last seen).
    pub last_seen: Option<(Player, Instant)>,
    pub info: Option<ReaderInfo>,
    /// Set when the device on the port speaks another protocol (old firmware).
    pub protocol_error: Option<String>,
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

/// One message from the reader.
#[derive(Clone, Debug, PartialEq)]
pub enum ReaderMsg {
    Hello {
        proto: u32,
        info: ReaderInfo,
        card: Option<Player>,
    },
    CardPresent(Player),
    CardRemoved(String),
    Status {
        card: Option<Player>,
    },
    Result {
        id: Option<i64>,
        ok: bool,
        error: Option<String>,
        detail: Option<String>,
    },
    Log {
        level: String,
        msg: String,
    },
}

#[derive(Deserialize)]
struct CardJson {
    uid: String,
    #[serde(default)]
    name: Option<String>,
}

impl CardJson {
    fn player(self) -> Option<Player> {
        let uid = self.uid.trim().to_ascii_uppercase();
        if uid.is_empty() {
            return None;
        }
        let name = self
            .name
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty());
        Some(Player { uid, name })
    }
}

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type")]
    kind: String,
    // hello
    #[serde(default)]
    proto: Option<u32>,
    #[serde(default)]
    fw: String,
    #[serde(default)]
    serial: String,
    #[serde(default)]
    display: String,
    #[serde(default)]
    chip: String,
    // hello, status
    #[serde(default)]
    card: Option<CardJson>,
    // card
    #[serde(default)]
    state: String,
    #[serde(default)]
    uid: Option<String>,
    #[serde(default)]
    name: Option<String>,
    // result
    #[serde(default)]
    id: Option<i64>,
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    detail: Option<String>,
    // log
    #[serde(default)]
    level: String,
    #[serde(default)]
    msg: String,
}

/// Parse one line from the reader; `None` for boot text, garbage and
/// messages this station does not know.
pub fn parse_line(line: &str) -> Option<ReaderMsg> {
    let line = line.trim();
    if !line.starts_with('{') {
        return None;
    }
    let l: Line = serde_json::from_str(line).ok()?;
    match l.kind.as_str() {
        "hello" => Some(ReaderMsg::Hello {
            proto: l.proto.unwrap_or(0),
            info: ReaderInfo {
                fw: l.fw,
                serial: l.serial,
                display: l.display,
                chip: l.chip,
            },
            card: l.card.and_then(CardJson::player),
        }),
        "card" => {
            let uid = l.uid?;
            match l.state.as_str() {
                "present" => CardJson { uid, name: l.name }
                    .player()
                    .map(ReaderMsg::CardPresent),
                "removed" => Some(ReaderMsg::CardRemoved(uid.trim().to_ascii_uppercase())),
                _ => None,
            }
        }
        "status" => Some(ReaderMsg::Status {
            card: l.card.and_then(CardJson::player),
        }),
        "result" => Some(ReaderMsg::Result {
            id: l.id,
            ok: l.ok,
            error: l.error,
            detail: l.detail,
        }),
        "log" => Some(ReaderMsg::Log {
            level: l.level,
            msg: l.msg,
        }),
        _ => None,
    }
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
        info!(port = %cfg.port, "RFID port open");
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

/// Per-connection protocol state, separate from the I/O so it can be tested.
pub(crate) struct Link {
    next_id: i64,
    last_ping: Option<Instant>,
    last_hello_request: Option<Instant>,
    ready: bool,
}

impl Link {
    pub(crate) fn new() -> Link {
        Link {
            next_id: 1,
            last_ping: None,
            last_hello_request: None,
            ready: false,
        }
    }

    fn id(&mut self) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Lines the station has to send now (hello until introduced, pings).
    pub(crate) fn due(&mut self, now: Instant) -> Vec<String> {
        let mut out = Vec::new();
        if !self.ready
            && self
                .last_hello_request
                .is_none_or(|t| now.duration_since(t) >= HELLO_EVERY)
        {
            self.last_hello_request = Some(now);
            out.push(format!(r#"{{"type":"hello","id":{}}}"#, self.id()));
        }
        if self
            .last_ping
            .is_none_or(|t| now.duration_since(t) >= PING_EVERY)
        {
            self.last_ping = Some(now);
            out.push(format!(r#"{{"type":"ping","id":{}}}"#, self.id()));
        }
        out
    }

    /// Apply one message to the shared snapshot.
    pub(crate) fn handle(&mut self, msg: ReaderMsg, state: &Arc<Mutex<RfidSnapshot>>) {
        match msg {
            ReaderMsg::Hello { proto, info, card } => {
                if proto != PROTOCOL {
                    let problem = format!(
                        "reader speaks protocol {}, the station needs {PROTOCOL}: update the reader firmware",
                        if proto == 0 {
                            "1 (old firmware)".to_string()
                        } else {
                            proto.to_string()
                        }
                    );
                    let mut s = lock(state);
                    if s.protocol_error.as_deref() != Some(problem.as_str()) {
                        error!(%problem, fw = %info.fw, "RFID reader refused");
                    }
                    s.protocol_error = Some(problem);
                    drop(s);
                    self.ready = false;
                    set_connected(state, false);
                    return;
                }
                info!(fw = %info.fw, serial = %info.serial, display = %info.display, "RFID reader ready");
                {
                    let mut s = lock(state);
                    s.protocol_error = None;
                    s.info = Some(info);
                }
                self.ready = true;
                set_connected(state, true);
                update_player(state, card);
            }
            // Before the reader introduced itself its card reports are not trusted.
            _ if !self.ready => {}
            ReaderMsg::CardPresent(p) => update_player(state, Some(p)),
            ReaderMsg::CardRemoved(_) => update_player(state, None),
            // The heartbeat is the truth after missed lines.
            ReaderMsg::Status { card } => update_player(state, card),
            ReaderMsg::Result {
                id,
                ok,
                error,
                detail,
            } => {
                if ok {
                    debug!(?id, "RFID command ok");
                } else {
                    warn!(?id, error = ?error, detail = ?detail, "RFID command failed");
                }
            }
            ReaderMsg::Log { level, msg } => match level.as_str() {
                "error" => error!(%msg, "RFID reader"),
                "warn" => warn!(%msg, "RFID reader"),
                "debug" => debug!(%msg, "RFID reader"),
                _ => info!(%msg, "RFID reader"),
            },
        }
    }
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
    let mut last_message = Instant::now() + BOOT_GRACE;
    let mut link = Link::new();
    // Commands queued while disconnected are stale (the display state they
    // targeted is gone).
    while commands.try_recv().is_ok() {}
    while !stop.load(Ordering::Relaxed) {
        let mut outgoing = link.due(Instant::now());
        if link.ready {
            outgoing.extend(commands.try_iter());
        }
        for cmd in outgoing {
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
                        if let Some(msg) = parse_line(&text) {
                            last_message = Instant::now();
                            link.handle(msg, state);
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
        if last_message.elapsed() > stale_after {
            return format!("no message for {:.1}s", stale_after.as_secs_f64());
        }
    }
    "shutdown".into()
}

fn lock(state: &Arc<Mutex<RfidSnapshot>>) -> std::sync::MutexGuard<'_, RfidSnapshot> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

fn set_connected(state: &Arc<Mutex<RfidSnapshot>>, connected: bool) {
    let mut s = lock(state);
    s.connected = connected;
    if !connected && let Some(p) = s.present.take() {
        // An unknown card state is treated like a removed card.
        s.last_seen = Some((p, Instant::now()));
    }
}

fn update_player(state: &Arc<Mutex<RfidSnapshot>>, player: Option<Player>) {
    let mut s = lock(state);
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

    fn erv() -> Player {
        Player {
            uid: "04A1B2C3".into(),
            name: Some("Erv".into()),
        }
    }

    const HELLO: &str = r#"{"type":"hello","proto":2,"fw":"1.0.0","board":"esp32dev","serial":"A4CF","display":"128x32","reader":"ok","chip":"0x92","card":null}"#;

    #[test]
    fn parses_v2_lines() {
        match parse_line(HELLO) {
            Some(ReaderMsg::Hello { proto, info, card }) => {
                assert_eq!(proto, 2);
                assert_eq!(info.fw, "1.0.0");
                assert_eq!(card, None);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            parse_line(
                r#"{"type":"card","state":"present","uid":"04a1b2c3","name":"Erv","format":"retroverse"}"#
            ),
            Some(ReaderMsg::CardPresent(erv()))
        );
        assert_eq!(
            parse_line(
                r#"{"type":"card","state":"present","uid":"0A0B0C0D","name":null,"format":"blank"}"#
            ),
            Some(ReaderMsg::CardPresent(Player {
                uid: "0A0B0C0D".into(),
                name: None
            }))
        );
        assert_eq!(
            parse_line(r#"{"type":"card","state":"removed","uid":"04A1B2C3"}"#),
            Some(ReaderMsg::CardRemoved("04A1B2C3".into()))
        );
        assert_eq!(
            parse_line(
                r#"{"type":"status","card":{"uid":"04A1B2C3","name":"Erv","format":"legacy"},"reader":"ok","uptime_s":3,"host":true}"#
            ),
            Some(ReaderMsg::Status { card: Some(erv()) })
        );
        assert_eq!(
            parse_line(r#"{"type":"result","id":7,"ok":false,"error":"timeout"}"#),
            Some(ReaderMsg::Result {
                id: Some(7),
                ok: false,
                error: Some("timeout".into()),
                detail: None
            })
        );
    }

    #[test]
    fn ignores_noise_and_v1_lines() {
        assert_eq!(parse_line("ets Jun  8 2016 00:22:57"), None);
        assert_eq!(parse_line("{broken"), None);
        assert_eq!(
            parse_line(r#"{"type":"login","username":"Erv","uid":"04A1B2C3","source":"rfid"}"#),
            None
        );
        assert_eq!(parse_line(r#"{"type":"card","state":"present"}"#), None);
    }

    #[test]
    fn link_follows_the_reader() {
        let state: Arc<Mutex<RfidSnapshot>> = Arc::default();
        let mut link = Link::new();
        // Card reports before hello are ignored.
        link.handle(ReaderMsg::CardPresent(erv()), &state);
        assert_eq!(lock(&state).present, None);
        link.handle(parse_line(HELLO).unwrap(), &state);
        assert!(lock(&state).connected);
        assert_eq!(lock(&state).info.as_ref().unwrap().serial, "A4CF");
        link.handle(ReaderMsg::CardPresent(erv()), &state);
        assert_eq!(lock(&state).present, Some(erv()));
        // A missed "removed": the heartbeat corrects it.
        link.handle(ReaderMsg::Status { card: None }, &state);
        let s = lock(&state).clone();
        assert_eq!(s.present, None);
        assert_eq!(s.last_seen.map(|(p, _)| p), Some(erv()));
    }

    #[test]
    fn old_firmware_is_refused() {
        let state: Arc<Mutex<RfidSnapshot>> = Arc::default();
        let mut link = Link::new();
        let v3 = HELLO.replace(r#""proto":2"#, r#""proto":3"#);
        link.handle(parse_line(&v3).unwrap(), &state);
        let s = lock(&state).clone();
        assert!(!s.connected);
        assert!(s.protocol_error.unwrap().contains("protocol 3"));
        assert!(!link.ready);
    }

    #[test]
    fn link_sends_hello_until_ready_and_pings() {
        let mut link = Link::new();
        let t0 = Instant::now();
        let first = link.due(t0);
        assert_eq!(first.len(), 2);
        assert!(first[0].contains(r#""type":"hello""#) && first[1].contains(r#""type":"ping""#));
        assert!(link.due(t0 + Duration::from_millis(500)).is_empty());
        assert_eq!(link.due(t0 + HELLO_EVERY).len(), 1); // hello again, no ping yet
        link.ready = true;
        let later = link.due(t0 + PING_EVERY);
        assert_eq!(later.len(), 1);
        assert!(later[0].contains("ping"));
    }

    #[test]
    fn grace_period_keeps_a_removed_card() {
        let snap = RfidSnapshot {
            connected: true,
            present: None,
            last_seen: Some((erv(), Instant::now())),
            ..Default::default()
        };
        assert_eq!(snap.player_for_game(Duration::from_secs(60)), Some(erv()));
        assert_eq!(snap.player_for_game(Duration::ZERO), None);
    }
}

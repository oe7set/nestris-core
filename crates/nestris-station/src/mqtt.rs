//! MQTT link to the host.
//!
//! One background thread owns the connection and reconnects forever with
//! exponential backoff. Every connection attempt uses a fresh client, so
//! packet ids never carry over between connections and the spool's
//! "publish → PUBACK → delete" bookkeeping stays exact:
//!
//! - **durable** messages (game start/end, cheats) go through the on-disk
//!   [`Spool`] and are deleted only after the broker's PUBACK; after a
//!   reconnect (or a station restart) everything unacknowledged is resent;
//! - **retained** messages (status, player) are cached and re-published on
//!   every connect;
//! - **live** messages are QoS 0 and dropped while offline.
//!
//! The broker publishes the retained last will (`state: offline`) if the
//! station vanishes without a clean shutdown.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Result;
use rumqttc::{Client, ConnectReturnCode, Event, Incoming, LastWill, MqttOptions, Outgoing, QoS};
use tracing::{debug, info, warn};

use crate::config::StationConfig;
use crate::payload::Offline;
use crate::spool::{Spool, SpoolId, SpooledMessage};

/// Spooled messages handed to the client per pump (bounds request-queue use).
const PUMP_BATCH: usize = 32;
const REQUEST_CAPACITY: usize = 256;
const MAX_PACKET: usize = 256 * 1024;

pub mod topic {
    pub const STATUS: &str = "status";
    pub const PLAYER: &str = "player";
    pub const LIVE: &str = "live";
    pub const GAME_START: &str = "event/game_start";
    pub const GAME_END: &str = "event/game_end";
    pub const CHEAT: &str = "event/cheat";
    pub const CMD: &str = "cmd";
}

struct Conn {
    client: Client,
    connected: bool,
    /// One entry per publish handed to the client, in order: the spool id
    /// for durable messages. Matched against `Outgoing::Publish(pkid)`.
    fifo: VecDeque<Option<SpoolId>>,
    inflight: HashMap<u16, SpoolId>,
    sent: HashSet<SpoolId>,
}

impl Conn {
    fn publish(
        &mut self,
        topic: &str,
        qos: QoS,
        retain: bool,
        payload: &str,
        id: Option<SpoolId>,
    ) -> bool {
        self.fifo.push_back(id);
        match self
            .client
            .try_publish(topic, qos, retain, payload.as_bytes().to_vec())
        {
            Ok(()) => true,
            Err(e) => {
                self.fifo.pop_back();
                debug!(topic, error = %e, "publish deferred (request queue full)");
                false
            }
        }
    }
}

#[derive(Default)]
struct Shared {
    conn: Option<Conn>,
    retained: BTreeMap<String, String>,
}

pub struct MqttLink {
    base: String,
    shared: Arc<Mutex<Shared>>,
    spool: Arc<Spool>,
    stop: Arc<AtomicBool>,
    inbound: Receiver<String>,
    handle: Option<JoinHandle<()>>,
}

impl MqttLink {
    pub fn start(cfg: &StationConfig, spool: Arc<Spool>) -> Result<MqttLink> {
        let base = cfg.topic_base();
        let options = options(cfg)?;
        let shared: Arc<Mutex<Shared>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let worker = Worker {
            options,
            cmd_topic: format!("{base}/{}", topic::CMD),
            shared: shared.clone(),
            spool: spool.clone(),
            stop: stop.clone(),
            inbound: tx,
            backoff_max: Duration::from_secs_f64(cfg.mqtt.reconnect_max_s.max(1.0)),
        };
        let handle = std::thread::Builder::new()
            .name("mqtt".into())
            .spawn(move || worker.run())?;
        info!(host = %cfg.mqtt.host, port = cfg.mqtt.port, base = %base, "MQTT link started");
        Ok(MqttLink {
            base,
            shared,
            spool,
            stop,
            inbound: rx,
            handle: Some(handle),
        })
    }

    pub fn connected(&self) -> bool {
        lock(&self.shared)
            .conn
            .as_ref()
            .is_some_and(|c| c.connected)
    }

    pub fn topic(&self, suffix: &str) -> String {
        format!("{}/{suffix}", self.base)
    }

    /// Retained state (status, player): cached and re-sent on reconnect.
    pub fn publish_retained(&self, suffix: &str, payload: String) {
        let topic = self.topic(suffix);
        let mut shared = lock(&self.shared);
        if let Some(conn) = shared.conn.as_mut().filter(|c| c.connected) {
            conn.publish(&topic, QoS::AtLeastOnce, true, &payload, None);
        }
        shared.retained.insert(topic, payload);
    }

    /// Best-effort live data (QoS 0, dropped while offline).
    pub fn publish_live(&self, suffix: &str, payload: &str) {
        let topic = self.topic(suffix);
        let mut shared = lock(&self.shared);
        if let Some(conn) = shared.conn.as_mut().filter(|c| c.connected) {
            conn.publish(&topic, QoS::AtMostOnce, false, payload, None);
        }
    }

    /// Must-deliver message: persisted first, deleted after the broker's ack.
    pub fn publish_durable(&self, suffix: &str, payload: String) -> Result<()> {
        self.spool.put(&SpooledMessage {
            topic: self.topic(suffix),
            payload,
            retain: false,
        })?;
        pump(&mut lock(&self.shared), &self.spool);
        Ok(())
    }

    /// Durable messages not yet acknowledged by the broker.
    pub fn pending(&self) -> usize {
        self.spool.list().len()
    }

    /// Commands received on `<base>/cmd` since the last call.
    pub fn take_commands(&self) -> Vec<String> {
        self.inbound.try_iter().collect()
    }

    /// Clean shutdown: retained offline status, disconnect, join (bounded).
    pub fn shutdown(mut self, station: &str) {
        let offline = serde_json::to_string(&Offline {
            state: "offline",
            station,
            ts: Some(crate::payload::now()),
        })
        .unwrap_or_default();
        self.stop.store(true, Ordering::Relaxed);
        {
            let topic = self.topic(topic::STATUS);
            let mut shared = lock(&self.shared);
            if let Some(conn) = shared.conn.as_mut().filter(|c| c.connected) {
                conn.publish(&topic, QoS::AtLeastOnce, true, &offline, None);
                let _ = conn.client.try_disconnect();
            }
        }
        if let Some(handle) = self.handle.take() {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !handle.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
    }
}

fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

/// Hand unsent spool entries to the client (connected only).
fn pump(shared: &mut Shared, spool: &Spool) {
    let Some(conn) = shared.conn.as_mut().filter(|c| c.connected) else {
        return;
    };
    let mut handed = 0;
    for id in spool.list() {
        if handed >= PUMP_BATCH {
            break;
        }
        if conn.sent.contains(&id) {
            continue;
        }
        let Some(msg) = spool.get(&id) else {
            continue;
        };
        if !conn.publish(
            &msg.topic,
            QoS::AtLeastOnce,
            msg.retain,
            &msg.payload,
            Some(id.clone()),
        ) {
            break;
        }
        conn.sent.insert(id);
        handed += 1;
    }
}

fn options(cfg: &StationConfig) -> Result<MqttOptions> {
    let m = &cfg.mqtt;
    let mut opts = MqttOptions::new(cfg.client_id(), m.host.clone(), m.port);
    opts.set_keep_alive(Duration::from_secs(m.keepalive_s.max(5)));
    opts.set_clean_session(true);
    opts.set_max_packet_size(MAX_PACKET, MAX_PACKET);
    if !m.username.is_empty() {
        opts.set_credentials(m.username.clone(), cfg.mqtt_password()?.unwrap_or_default());
    }
    let will = serde_json::to_string(&Offline {
        state: "offline",
        station: &cfg.station.id,
        ts: None,
    })?;
    opts.set_last_will(LastWill::new(
        format!("{}/{}", cfg.topic_base(), topic::STATUS),
        will,
        QoS::AtLeastOnce,
        true,
    ));
    #[cfg(feature = "tls")]
    if m.tls {
        use anyhow::Context;
        let ca = std::fs::read(&m.ca_file)
            .with_context(|| format!("read mqtt.ca_file {:?}", m.ca_file))?;
        opts.set_transport(rumqttc::Transport::tls_with_config(
            rumqttc::TlsConfiguration::Simple {
                ca,
                alpn: None,
                client_auth: None,
            },
        ));
    }
    Ok(opts)
}

struct Worker {
    options: MqttOptions,
    cmd_topic: String,
    shared: Arc<Mutex<Shared>>,
    spool: Arc<Spool>,
    stop: Arc<AtomicBool>,
    inbound: Sender<String>,
    backoff_max: Duration,
}

impl Worker {
    fn run(self) {
        let mut backoff = Duration::from_secs(1);
        while !self.stop.load(Ordering::Relaxed) {
            let (client, mut connection) = Client::new(self.options.clone(), REQUEST_CAPACITY);
            // Queued now, sent right after CONNACK.
            let _ = client.try_subscribe(self.cmd_topic.clone(), QoS::AtLeastOnce);
            lock(&self.shared).conn = Some(Conn {
                client,
                connected: false,
                fifo: VecDeque::new(),
                inflight: HashMap::new(),
                sent: HashSet::new(),
            });
            let connected_at = Instant::now();
            let mut was_connected = false;
            for notification in connection.iter() {
                match notification {
                    Ok(Event::Incoming(Incoming::ConnAck(ack))) => {
                        if ack.code != ConnectReturnCode::Success {
                            warn!(code = ?ack.code, "MQTT broker refused the connection");
                            break;
                        }
                        was_connected = true;
                        info!("MQTT connected");
                        let mut shared = lock(&self.shared);
                        let retained: Vec<(String, String)> = shared
                            .retained
                            .iter()
                            .map(|(t, p)| (t.clone(), p.clone()))
                            .collect();
                        if let Some(conn) = shared.conn.as_mut() {
                            conn.connected = true;
                            for (topic, payload) in &retained {
                                conn.publish(topic, QoS::AtLeastOnce, true, payload, None);
                            }
                        }
                        pump(&mut shared, &self.spool);
                    }
                    Ok(Event::Outgoing(Outgoing::Publish(pkid))) => {
                        let mut shared = lock(&self.shared);
                        if let Some(conn) = shared.conn.as_mut()
                            && let Some(slot) = conn.fifo.pop_front()
                            && let Some(id) = slot
                            && pkid != 0
                        {
                            conn.inflight.insert(pkid, id);
                        }
                    }
                    Ok(Event::Incoming(Incoming::PubAck(ack))) => {
                        let mut shared = lock(&self.shared);
                        if let Some(conn) = shared.conn.as_mut()
                            && let Some(id) = conn.inflight.remove(&ack.pkid)
                        {
                            self.spool.remove(&id);
                            conn.sent.remove(&id);
                            debug!(id, "durable message acknowledged");
                        }
                        pump(&mut shared, &self.spool);
                    }
                    Ok(Event::Incoming(Incoming::Publish(p))) if p.topic == self.cmd_topic => {
                        let _ = self
                            .inbound
                            .send(String::from_utf8_lossy(&p.payload).into_owned());
                    }
                    Ok(Event::Outgoing(Outgoing::Disconnect)) => break,
                    Ok(_) => {}
                    Err(e) => {
                        if !self.stop.load(Ordering::Relaxed) {
                            warn!(error = %e, retry_s = backoff.as_secs(), "MQTT connection lost");
                        }
                        break;
                    }
                }
                if self.stop.load(Ordering::Relaxed) && !was_connected {
                    break;
                }
            }
            lock(&self.shared).conn = None;
            if self.stop.load(Ordering::Relaxed) {
                break;
            }
            if was_connected && connected_at.elapsed() > Duration::from_secs(30) {
                backoff = Duration::from_secs(1);
            }
            let until = Instant::now() + backoff;
            while Instant::now() < until && !self.stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(100));
            }
            backoff = (backoff * 2).min(self.backoff_max);
        }
    }
}

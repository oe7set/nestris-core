//! Output sinks: JSONL file / stdout, and a WebSocket broadcast server.
//!
//! The WebSocket sink is a small threaded broadcaster on plain `tungstenite`
//! (no async runtime): one accept thread, one writer thread per client, a
//! bounded per-client queue with drop-oldest so a slow consumer can never
//! stall the engine.

use std::collections::VecDeque;
use std::io::Write;
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};

use anyhow::{Context, Result};

/// Per-client outbound queue (frames); oldest dropped when full.
const CLIENT_QUEUE: usize = 8;

pub trait Sink {
    fn publish(&mut self, json: &str) -> Result<()>;
    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

pub struct JsonlSink {
    writer: Box<dyn Write>,
}

impl JsonlSink {
    pub fn to_file(path: &Path) -> Result<JsonlSink> {
        Ok(JsonlSink {
            writer: Box::new(std::io::BufWriter::new(
                std::fs::File::create(path).context("create jsonl")?,
            )),
        })
    }

    pub fn to_stdout() -> JsonlSink {
        JsonlSink {
            writer: Box::new(std::io::stdout().lock()),
        }
    }
}

impl Sink for JsonlSink {
    fn publish(&mut self, json: &str) -> Result<()> {
        self.writer.write_all(json.as_bytes())?;
        self.writer.write_all(b"\n")?;
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.writer.flush()?;
        Ok(())
    }
}

struct ClientQueue {
    frames: Mutex<(VecDeque<String>, bool)>, // (queue, closed)
    ready: Condvar,
}

/// Threaded WebSocket broadcast server.
pub struct WebSocketSink {
    clients: Arc<Mutex<Vec<Arc<ClientQueue>>>>,
}

impl WebSocketSink {
    pub fn bind(addr: &str) -> Result<WebSocketSink> {
        let listener = TcpListener::bind(addr).with_context(|| format!("ws bind {addr}"))?;
        eprintln!("WebSocket sink listening on ws://{addr}");
        let clients: Arc<Mutex<Vec<Arc<ClientQueue>>>> = Arc::new(Mutex::new(Vec::new()));
        let accept_clients = clients.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let Ok(mut ws) = tungstenite::accept(stream) else {
                    continue;
                };
                let queue = Arc::new(ClientQueue {
                    frames: Mutex::new((VecDeque::new(), false)),
                    ready: Condvar::new(),
                });
                accept_clients.lock().unwrap().push(queue.clone());
                std::thread::spawn(move || {
                    loop {
                        let msg = {
                            let mut guard = queue.frames.lock().unwrap();
                            loop {
                                if let Some(msg) = guard.0.pop_front() {
                                    break Some(msg);
                                }
                                if guard.1 {
                                    break None;
                                }
                                guard = queue.ready.wait(guard).unwrap();
                            }
                        };
                        let Some(msg) = msg else { break };
                        if ws.send(tungstenite::Message::text(msg)).is_err() {
                            let mut guard = queue.frames.lock().unwrap();
                            guard.1 = true;
                            break;
                        }
                    }
                });
            }
        });
        Ok(WebSocketSink { clients })
    }
}

impl Sink for WebSocketSink {
    fn publish(&mut self, json: &str) -> Result<()> {
        let mut clients = self.clients.lock().unwrap();
        clients.retain(|client| {
            let mut guard = client.frames.lock().unwrap();
            if guard.1 {
                return false; // writer thread marked it dead
            }
            if guard.0.len() >= CLIENT_QUEUE {
                guard.0.pop_front(); // drop-oldest: never stall the engine
            }
            guard.0.push_back(json.to_string());
            client.ready.notify_one();
            true
        });
        Ok(())
    }
}

/// Fan-out to several sinks; publish errors on one sink don't stop others.
pub struct MultiSink {
    pub sinks: Vec<Box<dyn Sink>>,
}

impl MultiSink {
    pub fn publish(&mut self, json: &str) {
        for sink in &mut self.sinks {
            let _ = sink.publish(json);
        }
    }

    pub fn flush(&mut self) {
        for sink in &mut self.sinks {
            let _ = sink.flush();
        }
    }
}

//! Forwarder between the pipeline worker's mpsc channel and the Qt event
//! loop. Lifecycle messages are queued in order; frame updates go through
//! a single latest-wins slot so a slow UI never accumulates a backlog.

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use cxx_qt::CxxQtThread;
use nestris_gui_core::worker::{GuiUpdate, WorkerMsg};

use crate::bridge::app_bridge::qobject::AppBridge;

/// Drain the worker channel until it closes. `generation` stamps every
/// queued call so the bridge can drop messages from a superseded source.
pub fn forward(updates: Receiver<WorkerMsg>, qt: CxxQtThread<AppBridge>, generation: u64) {
    let slot: Arc<Mutex<Option<Box<GuiUpdate>>>> = Arc::new(Mutex::new(None));
    while let Ok(first) = updates.recv() {
        let mut latest: Option<Box<GuiUpdate>> = None;
        let handle = |msg: WorkerMsg, latest: &mut Option<Box<GuiUpdate>>| match msg {
            WorkerMsg::Update(update) => *latest = Some(update),
            other => {
                let _ = qt.queue(move |bridge| bridge.handle_worker_msg(other, generation));
            }
        };
        handle(first, &mut latest);
        while let Ok(msg) = updates.try_recv() {
            handle(msg, &mut latest);
        }

        if let Some(update) = latest {
            let was_empty = {
                let mut pending = slot.lock().unwrap();
                let was_empty = pending.is_none();
                *pending = Some(update);
                was_empty
            };
            // Only one drain call is ever in flight; a full slot just got
            // fresher data for the drain that is already queued.
            if was_empty {
                let slot = Arc::clone(&slot);
                let _ = qt.queue(move |bridge| {
                    let update = slot.lock().unwrap().take();
                    if let Some(update) = update {
                        bridge.apply_update(update, generation);
                    }
                });
            }
        }
    }
}

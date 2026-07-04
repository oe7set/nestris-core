//! Event-stream formatting shared by the GUIs: per-frame OutputFrame
//! events become display rows (`mm:ss.s field reason → new`, TETRIS clears
//! flagged gold) with a bounded backlog.

use nestris_engine::output::OutputFrame;

/// One formatted event-stream row.
pub struct EventRow {
    pub text: String,
    /// Display severity: `info` / `warn` / `error` / `gold` (tetris clear).
    pub severity: String,
}

/// Kept rows are capped so long sessions don't grow without bound.
const MAX_EVENTS: usize = 300;

/// Append this frame's events as display rows (oldest rows drop first).
pub fn push_events(events: &mut Vec<EventRow>, output: &OutputFrame) {
    for ev in &output.events {
        let mm = (ev.ts / 60.0) as u32;
        let ss = ev.ts % 60.0;
        let extra = ev
            .new
            .as_ref()
            .map(|v| format!(" → {v}"))
            .unwrap_or_default();
        events.push(EventRow {
            text: format!("{mm}:{ss:04.1} {} {}{}", ev.field, ev.reason, extra),
            severity: if ev.reason == "clear_tetris" {
                "gold".into()
            } else {
                ev.severity.clone()
            },
        });
    }
    if events.len() > MAX_EVENTS {
        let excess = events.len() - MAX_EVENTS;
        events.drain(..excess);
    }
}

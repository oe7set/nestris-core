//! End-to-end smoke test for the shared pipeline worker on the replay
//! path (no ffmpeg needed): a synthetic .ngf plays through `worker::spawn`
//! and the UI-facing channel must deliver Opened, live REPLAY updates,
//! instant seeks, and a clean Stop/join.

use std::time::Duration;

use nestris_engine::config::EngineConfig;
use nestris_engine::enums::Piece;
use nestris_gui_core::worker::{self, Cmd, SinkOptions, WorkerMsg};
use nestris_ngf::codec::{NgfFrame, encode_v3};

fn write_sample_ngf(frames: u32) -> std::path::PathBuf {
    let mut buf = Vec::new();
    for i in 0..frames {
        let mut field = [0u8; 200];
        for c in 0..10 {
            field[190 + c] = ((c + i as usize) % 4) as u8;
        }
        let frame = NgfFrame {
            gameid: 1,
            ctime_ms: i * 16,
            lines: Some(60 + i as u16),
            level: Some(18),
            score: Some(100_000 + i * 1200),
            preview: Some(Piece::T),
            cur_piece: Some(Piece::I),
            counts: [
                Some(10),
                Some(11),
                Some(12),
                Some(13),
                Some(14),
                Some(15),
                Some(16),
            ],
            field,
            ..NgfFrame::default()
        };
        encode_v3(&frame, &mut buf);
    }
    let path =
        std::env::temp_dir().join(format!("nestris-worker-smoke-{}.ngf", std::process::id()));
    std::fs::write(&path, &buf).expect("write sample ngf");
    path
}

#[test]
fn replay_worker_delivers_updates_and_stops() {
    let path = write_sample_ngf(30);
    let handle = worker::spawn(
        path.to_string_lossy().into_owned(),
        EngineConfig::default(),
        SinkOptions {
            record: false,
            ..SinkOptions::default()
        },
        0.0,
        -1.0, // max speed: no pacing sleeps
    );

    let recv = |what: &str| {
        handle
            .updates
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
    };

    let WorkerMsg::Opened { duration_s, live } = recv("Opened") else {
        panic!("first message must be Opened");
    };
    assert!(!live);
    assert!(duration_s.unwrap_or(0.0) > 0.0, "replay reports a duration");

    // Drain until a real frame update arrives.
    let update = loop {
        match recv("first update") {
            WorkerMsg::Update(update) => break update,
            WorkerMsg::Error(err) => panic!("worker error: {err}"),
            _ => {}
        }
    };
    assert_eq!(update.lock_state, "REPLAY");
    assert!(update.raw_rgba.is_empty(), "replay has no source preview");
    assert_eq!(update.output.fields.level, Some(18));
    assert!(update.output.fields.score.is_some());

    // Instant seek back to the start: another update must follow.
    handle.cmd.send(Cmd::Seek(0.0)).expect("send seek");
    loop {
        match recv("post-seek update") {
            WorkerMsg::Update(update) => {
                assert_eq!(update.lock_state, "REPLAY");
                break;
            }
            WorkerMsg::Error(err) => panic!("worker error: {err}"),
            _ => {}
        }
    }

    handle.cmd.send(Cmd::Stop).expect("send stop");
    handle.join.join().expect("worker joins cleanly");
    let _ = std::fs::remove_file(&path);
}

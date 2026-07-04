//! Crash-safe NGF recording to disk, shared by the CLI and the desktop GUI.
//!
//! While a game is being recorded, every encoded frame is streamed to a
//! `<name>.ngf.part` file (raw, uncompressed) so a crash or power loss
//! leaves a readable recording behind. On game end the finished bytes are
//! written as `<name>.ngf.gz` (or `.ngf`) and the `.part` file is removed.

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use nestris_ngf::recorder::{GameRecorder, RecorderEvent};

/// Flush the `.part` stream roughly every two seconds of 60 fps play.
const FLUSH_EVERY_BYTES: usize = 120 * nestris_ngf::codec::V3_FRAME_SIZE;

/// `Documents\nestris-recordings` (or the platform equivalent), falling back
/// to `./nestris-recordings` when no home directory is known.
pub fn default_recording_dir() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from);
    match home {
        Some(home) => home.join("Documents").join("nestris-recordings"),
        None => PathBuf::from("nestris-recordings"),
    }
}

struct PartFile {
    path: PathBuf,
    base: String,
    writer: BufWriter<File>,
    unflushed: usize,
}

/// Applies [`RecorderEvent`]s to the filesystem and streams pending frames
/// into the active `.part` file.
pub struct RecordingSink {
    dir: PathBuf,
    gzip: bool,
    part: Option<PartFile>,
}

impl RecordingSink {
    pub fn new(dir: PathBuf, gzip: bool) -> Result<Self> {
        fs::create_dir_all(&dir)
            .with_context(|| format!("create recording dir {}", dir.display()))?;
        Ok(Self {
            dir,
            gzip,
            part: None,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Process the events returned by [`GameRecorder::push`], then stream any
    /// newly encoded frames to the `.part` file. Returns the paths of
    /// recordings finalized by this call.
    pub fn handle(
        &mut self,
        recorder: &mut GameRecorder,
        events: Vec<RecorderEvent>,
    ) -> Result<Vec<PathBuf>> {
        let mut saved = Vec::new();
        for event in events {
            match event {
                RecorderEvent::GameStarted { gameid } => self.open_part(gameid)?,
                RecorderEvent::GameFinished {
                    frames: _, bytes, ..
                } => {
                    if let Some(path) = self.write_final(&bytes)? {
                        saved.push(path);
                    }
                }
            }
        }

        // A too-short game is discarded without a finish event; drop its part.
        if !recorder.recording() && self.part.is_some() {
            self.drop_part();
        }

        let pending = recorder.drain_pending();
        if !pending.is_empty()
            && let Some(part) = &mut self.part
        {
            part.writer.write_all(pending).context("write .ngf.part")?;
            part.unflushed += pending.len();
            if part.unflushed >= FLUSH_EVERY_BYTES {
                part.writer.flush().context("flush .ngf.part")?;
                part.unflushed = 0;
            }
        }
        Ok(saved)
    }

    /// Finish and persist the active recording (stream end / shutdown).
    pub fn finalize(&mut self, recorder: &mut GameRecorder) -> Result<Option<PathBuf>> {
        // Drain what the part file is still owed before finalizing.
        self.handle(recorder, Vec::new())?;
        let events = recorder.finalize().into_iter().collect();
        Ok(self.handle(recorder, events)?.pop())
    }

    /// Discard the active recording and its `.part` file (seek / source switch).
    pub fn abort(&mut self, recorder: &mut GameRecorder) {
        recorder.abort();
        self.drop_part();
    }

    fn open_part(&mut self, gameid: u16) -> Result<()> {
        self.drop_part();
        let base = format!(
            "{}_g{:03}",
            chrono::Local::now().format("%Y%m%d-%H%M%S"),
            gameid
        );
        let path = self.dir.join(format!("{base}.ngf.part"));
        let file = File::create(&path).with_context(|| format!("create {}", path.display()))?;
        self.part = Some(PartFile {
            path,
            base,
            writer: BufWriter::new(file),
            unflushed: 0,
        });
        Ok(())
    }

    fn write_final(&mut self, bytes: &[u8]) -> Result<Option<PathBuf>> {
        let Some(part) = self.part.take() else {
            return Ok(None);
        };
        drop(part.writer);
        let path = if self.gzip {
            let path = self.dir.join(format!("{}.ngf.gz", part.base));
            let gz = nestris_ngf::io::compress_gz(bytes).context("gzip recording")?;
            fs::write(&path, gz).with_context(|| format!("write {}", path.display()))?;
            path
        } else {
            let path = self.dir.join(format!("{}.ngf", part.base));
            fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
            path
        };
        let _ = fs::remove_file(&part.path);
        Ok(Some(path))
    }

    fn drop_part(&mut self) {
        if let Some(part) = self.part.take() {
            drop(part.writer);
            let _ = fs::remove_file(&part.path);
        }
    }
}

impl Drop for RecordingSink {
    fn drop(&mut self) {
        // Keep the .part file on unexpected teardown: it is the crash-safety
        // artifact. Only explicit finalize/abort remove it.
        if let Some(part) = &mut self.part {
            let _ = part.writer.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nestris_engine::enums::GameState;
    use nestris_engine::output::{Event, Fields, OutputFrame};
    use nestris_ngf::recorder::RecorderConfig;

    fn frame(seq: i64, state: GameState, new_game: bool) -> OutputFrame {
        let mut events = Vec::new();
        if new_game {
            events.push(Event {
                ts: seq as f64 / 60.0,
                field: "game".into(),
                reason: "new_game".into(),
                severity: "info".into(),
                old: None,
                new: None,
                confidence: None,
            });
        }
        OutputFrame {
            schema_version: 4,
            seq,
            ts: seq as f64 / 60.0,
            region: nestris_engine::enums::Region::Ntsc,
            game_state: state,
            fields: Fields {
                score: Some(100),
                ..Default::default()
            },
            stats: Default::default(),
            stats_ext: None,
            confidence: Default::default(),
            events,
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("nestris-recording-tests")
            .join(name);
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn records_part_then_finalizes_gz() {
        let dir = temp_dir("finalize");
        let mut recorder = GameRecorder::new(RecorderConfig {
            min_frames: 5,
            end_confirm_frames: 3,
            ..Default::default()
        });
        let mut sink = RecordingSink::new(dir.clone(), true).unwrap();

        let events = recorder.push(&frame(0, GameState::InGame, true));
        sink.handle(&mut recorder, events).unwrap();
        let parts: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].extension().unwrap(), "part");

        let mut saved = Vec::new();
        for i in 1..10 {
            let ev = recorder.push(&frame(i, GameState::InGame, false));
            saved.extend(sink.handle(&mut recorder, ev).unwrap());
        }
        for i in 10..14 {
            let ev = recorder.push(&frame(i, GameState::GameOver, false));
            saved.extend(sink.handle(&mut recorder, ev).unwrap());
        }
        assert_eq!(saved.len(), 1);
        assert!(saved[0].to_string_lossy().ends_with(".ngf.gz"));
        // Part removed, final file decodes.
        let entries: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(entries, vec![saved[0].clone()]);
        let frames = nestris_ngf::io::decode_all(&fs::read(&saved[0]).unwrap()).unwrap();
        assert!(frames.len() >= 10);
    }

    #[test]
    fn short_game_part_is_cleaned_up() {
        let dir = temp_dir("short");
        let mut recorder = GameRecorder::new(RecorderConfig {
            min_frames: 100,
            ..Default::default()
        });
        let mut sink = RecordingSink::new(dir.clone(), true).unwrap();
        let ev = recorder.push(&frame(0, GameState::InGame, true));
        sink.handle(&mut recorder, ev).unwrap();
        for i in 1..5 {
            let ev = recorder.push(&frame(i, GameState::InGame, false));
            sink.handle(&mut recorder, ev).unwrap();
        }
        assert!(sink.finalize(&mut recorder).unwrap().is_none());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0, "no leftovers");
    }

    #[test]
    fn part_survives_without_finalize() {
        let dir = temp_dir("crash");
        let mut recorder = GameRecorder::new(RecorderConfig {
            min_frames: 1,
            ..Default::default()
        });
        {
            let mut sink = RecordingSink::new(dir.clone(), true).unwrap();
            let ev = recorder.push(&frame(0, GameState::InGame, true));
            sink.handle(&mut recorder, ev).unwrap();
            // sink dropped here without finalize — simulated crash.
        }
        let parts: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(parts.len(), 1);
        let frames = nestris_ngf::io::decode_all(&fs::read(&parts[0]).unwrap()).unwrap();
        assert_eq!(frames.len(), 1, "part file is a readable raw NGF");
    }
}

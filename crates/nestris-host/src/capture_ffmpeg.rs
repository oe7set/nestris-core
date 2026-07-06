//! Video decoding via an ffmpeg subprocess writing raw BGR24 to a pipe.
//!
//! Chosen over linking FFmpeg (`ffmpeg-next`): no bindgen/LLVM build
//! dependency on Windows, the ffmpeg binary handles every fixture codec
//! (H264/AV1 in various containers), and live capture uses the same code
//! path (`-f dshow` / `-f v4l2`).

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result, bail};
use nestris_engine::frame::Frame;
use nestris_vision::Image;

/// Locate ffmpeg/ffprobe, in priority order: `NESTRIS_FFMPEG` dir override,
/// bundled next to the executable (`ffmpeg/` subdir, the exe dir itself, or
/// a shared `ffmpeg/` dir one level up in the combined release bundle), the
/// standard winget links directory, then PATH.
fn tool_path(tool: &str) -> String {
    let exe_name = format!("{tool}{}", std::env::consts::EXE_SUFFIX);
    if let Ok(dir) = std::env::var("NESTRIS_FFMPEG") {
        let p = Path::new(&dir).join(&exe_name);
        if p.exists() {
            return p.to_string_lossy().into_owned();
        }
    }
    if let Ok(me) = std::env::current_exe()
        && let Some(dir) = me.parent()
    {
        let mut candidates = vec![dir.join("ffmpeg").join(&exe_name), dir.join(&exe_name)];
        if let Some(parent) = dir.parent() {
            candidates.push(parent.join("ffmpeg").join(&exe_name));
        }
        for p in candidates {
            if p.exists() {
                return p.to_string_lossy().into_owned();
            }
        }
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let p = Path::new(&local)
            .join("Microsoft/WinGet/Links")
            .join(&exe_name);
        if p.exists() {
            return p.to_string_lossy().into_owned();
        }
    }
    tool.to_string()
}

/// A [`Command`] for one of the bundled tools. On Windows the child gets
/// `CREATE_NO_WINDOW`: ffmpeg/ffprobe are console binaries and would
/// otherwise flash a console window when spawned from the GUIs.
fn tool_command(tool: &str) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(tool_path(tool));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Probed stream properties.
pub struct VideoInfo {
    pub width: usize,
    pub height: usize,
    pub fps: f64,
    /// Container duration in seconds (files only; `None` for live devices).
    pub duration_s: Option<f64>,
}

pub fn probe(input: &str) -> Result<VideoInfo> {
    let output = tool_command("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height,r_frame_rate,avg_frame_rate:format=duration",
            "-print_format",
            "json",
            input,
        ])
        .output()
        .context("spawn ffprobe (install ffmpeg or set NESTRIS_FFMPEG)")?;
    if !output.status.success() {
        bail!(
            "ffprobe failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let stream = &value["streams"][0];
    let width = stream["width"].as_u64().context("probe width")? as usize;
    let height = stream["height"].as_u64().context("probe height")? as usize;
    let rate = stream["avg_frame_rate"]
        .as_str()
        .filter(|s| *s != "0/0")
        .or_else(|| stream["r_frame_rate"].as_str())
        .context("probe frame rate")?;
    let fps = match rate.split_once('/') {
        Some((num, den)) => {
            let (num, den): (f64, f64) = (num.parse()?, den.parse()?);
            if den > 0.0 { num / den } else { 30.0 }
        }
        None => rate.parse()?,
    };
    let duration_s = value["format"]["duration"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok());
    Ok(VideoInfo {
        width,
        height,
        fps,
        duration_s,
    })
}

/// List DirectShow capture devices (Windows) via ffmpeg.
pub fn list_devices() -> Result<String> {
    let output = tool_command("ffmpeg")
        .args([
            "-hide_banner",
            "-list_devices",
            "true",
            "-f",
            "dshow",
            "-i",
            "dummy",
        ])
        .output()
        .context("spawn ffmpeg (install ffmpeg or set NESTRIS_FFMPEG)")?;
    // ffmpeg prints the device list on stderr and exits non-zero by design.
    Ok(String::from_utf8_lossy(&output.stderr).into_owned())
}

/// Streaming decoder: raw BGR24 frames read from an ffmpeg pipe.
/// `dshow:<device name>` opens a live DirectShow device; anything else is a
/// file/URL ffmpeg can read.
pub struct VideoDecoder {
    child: Child,
    info: VideoInfo,
    frame_bytes: usize,
    next_seq: i64,
    /// Live sources have no reliable per-frame pts on the raw pipe; stamp
    /// with the wall clock instead of seq/fps.
    live: bool,
    started: std::time::Instant,
}

impl VideoDecoder {
    /// Open `input` (file path or `dshow:...` device) at `start` seconds.
    pub fn open(input: &str, start: f64) -> Result<VideoDecoder> {
        let live = input.starts_with("dshow:");
        let mut cmd = tool_command("ffmpeg");
        cmd.arg("-v").arg("error");
        let info;
        if live {
            let device = input.trim_start_matches("dshow:");
            cmd.args(["-f", "dshow", "-i", &format!("video={device}")]);
            // dshow cannot be ffprobe'd before opening; scale to a known size.
            info = VideoInfo {
                width: 0,
                height: 0,
                fps: 60.0,
                duration_s: None,
            };
        } else {
            info = probe(input)?;
            if start > 0.0 {
                cmd.arg("-ss").arg(format!("{start}"));
            }
            cmd.args(["-i", input]);
        }
        let mut info = info;
        if live {
            // Normalize live input to 1280x720 so the frame size is known.
            cmd.args(["-vf", "scale=1280:720"]);
            info.width = 1280;
            info.height = 720;
        }
        if !live {
            // Emit each decoded frame exactly once. Without this, ffmpeg's
            // default CFR behavior duplicates frames on variable-frame-rate
            // sources (phone captures), shifting frame indexes against any
            // decode-order consumer (PyAV in the Python oracle drifted a
            // full second on the WIN fixtures).
            cmd.args(["-fps_mode", "passthrough"]);
        }
        cmd.args(["-f", "rawvideo", "-pix_fmt", "bgr24", "-"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(Stdio::null());
        let child = cmd.spawn().context("spawn ffmpeg")?;
        let frame_bytes = info.width * info.height * 3;
        Ok(VideoDecoder {
            child,
            info,
            frame_bytes,
            next_seq: 0,
            live,
            started: std::time::Instant::now(),
        })
    }

    pub fn info(&self) -> &VideoInfo {
        &self.info
    }

    /// Read the next frame; `None` at end of stream. `ts` defaults to
    /// `seq / fps` unless the caller overrides it afterwards.
    pub fn next_frame(&mut self) -> Result<Option<Frame>> {
        let stdout = self.child.stdout.as_mut().context("ffmpeg stdout")?;
        let mut buf = vec![0u8; self.frame_bytes];
        let mut filled = 0usize;
        while filled < buf.len() {
            let n = stdout.read(&mut buf[filled..])?;
            if n == 0 {
                return Ok(None);
            }
            filled += n;
        }
        let image = Image::from_vec(buf, self.info.width, self.info.height, 3);
        let seq = self.next_seq;
        self.next_seq += 1;
        let ts = if self.live {
            self.started.elapsed().as_secs_f64()
        } else {
            seq as f64 / self.info.fps
        };
        Ok(Some(Frame::new(image, seq, ts)))
    }
}

impl Drop for VideoDecoder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

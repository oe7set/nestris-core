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

/// Locate ffmpeg/ffprobe: `NESTRIS_FFMPEG` dir override, PATH, or the
/// standard winget links directory.
fn tool_path(tool: &str) -> String {
    if let Ok(dir) = std::env::var("NESTRIS_FFMPEG") {
        let p = Path::new(&dir).join(format!("{tool}.exe"));
        if p.exists() {
            return p.to_string_lossy().into_owned();
        }
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let p = Path::new(&local)
            .join("Microsoft/WinGet/Links")
            .join(format!("{tool}.exe"));
        if p.exists() {
            return p.to_string_lossy().into_owned();
        }
    }
    tool.to_string()
}

/// Probed stream properties.
pub struct VideoInfo {
    pub width: usize,
    pub height: usize,
    pub fps: f64,
}

pub fn probe(input: &str) -> Result<VideoInfo> {
    let output = Command::new(tool_path("ffprobe"))
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height,r_frame_rate,avg_frame_rate",
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
    Ok(VideoInfo { width, height, fps })
}

/// Streaming decoder: raw BGR24 frames read from an ffmpeg pipe.
pub struct VideoDecoder {
    child: Child,
    info: VideoInfo,
    frame_bytes: usize,
    next_seq: i64,
}

impl VideoDecoder {
    /// Open `input` (file path or `dshow:...` device) at `start` seconds.
    pub fn open(input: &str, start: f64) -> Result<VideoDecoder> {
        let info = probe(input)?;
        let mut cmd = Command::new(tool_path("ffmpeg"));
        cmd.arg("-v").arg("error");
        if start > 0.0 {
            cmd.arg("-ss").arg(format!("{start}"));
        }
        cmd.args(["-i", input, "-f", "rawvideo", "-pix_fmt", "bgr24", "-"])
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
        })
    }

    #[allow(dead_code)] // used by the live-source status path (Phase 6)
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
        Ok(Some(Frame::new(image, seq, seq as f64 / self.info.fps)))
    }
}

impl Drop for VideoDecoder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

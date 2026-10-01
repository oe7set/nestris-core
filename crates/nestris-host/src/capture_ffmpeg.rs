//! Video decoding via an ffmpeg subprocess writing raw BGR24 to a pipe.
//!
//! Chosen over linking FFmpeg (`ffmpeg-next`): no bindgen/LLVM build
//! dependency on Windows, the ffmpeg binary handles every fixture codec
//! (H264/AV1 in various containers), and live capture uses the same code
//! path (`-f dshow` / `-f v4l2`, or a network stream URL such as a local
//! go2rtc MJPEG/RTSP restream).

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};

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

/// Video4Linux capture devices: the stable `/dev/v4l/by-id` links first
/// (they survive re-plugging into another port), then raw `/dev/video*`.
pub fn list_v4l2_devices() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in ["/dev/v4l/by-id", "/dev"] {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut found: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                dir != "/dev"
                    || p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("video"))
            })
            .collect();
        found.sort();
        out.extend(found);
    }
    out
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

/// Live-capture options (ignored for files). Zero / `None` leaves the
/// choice to the device driver.
#[derive(Clone, Debug)]
pub struct LiveOptions {
    /// Device pixel format, e.g. `mjpeg` or `yuyv422` (v4l2 `-input_format`,
    /// dshow `-vcodec`).
    pub input_format: Option<String>,
    /// Requested capture size (`-video_size`); `0` = device default.
    pub capture_width: u32,
    pub capture_height: u32,
    /// Requested capture rate (`-framerate`); `None` = device default.
    pub fps: Option<f64>,
    /// Frames are scaled to this size so the frame layout is known up front.
    pub width: usize,
    pub height: usize,
}

impl Default for LiveOptions {
    fn default() -> Self {
        Self {
            input_format: None,
            capture_width: 0,
            capture_height: 0,
            fps: None,
            width: 1280,
            height: 720,
        }
    }
}

/// Source kinds recognized in an input string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputKind {
    File,
    /// `dshow:<device name>` (Windows DirectShow).
    DirectShow,
    /// `v4l2:<device path>` (Linux Video4Linux2).
    V4l2,
    /// A network stream (`rtsp://`, `http://`, ...), e.g. a go2rtc restream
    /// of the capture device. Live: no probe, wall-clock timestamps.
    Stream,
}

/// URL schemes treated as live network streams rather than files.
const STREAM_SCHEMES: [&str; 8] = [
    "rtsp://", "rtsps://", "rtmp://", "http://", "https://", "tcp://", "udp://", "srt://",
];

impl InputKind {
    pub fn of(input: &str) -> InputKind {
        if input.starts_with("dshow:") {
            InputKind::DirectShow
        } else if input.starts_with("v4l2:") {
            InputKind::V4l2
        } else if STREAM_SCHEMES.iter().any(|scheme| {
            input.len() > scheme.len() && input[..scheme.len()].eq_ignore_ascii_case(scheme)
        }) {
            InputKind::Stream
        } else {
            InputKind::File
        }
    }

    pub fn is_live(self) -> bool {
        self != InputKind::File
    }
}

/// The local device path a live input depends on (`v4l2:` only), so hosts
/// can wait for a re-plugged device instead of hammering ffmpeg.
pub fn device_path(input: &str) -> Option<&Path> {
    input.strip_prefix("v4l2:").map(Path::new)
}

/// ffmpeg arguments that open a live `input` (device or network stream) up
/// to and including the fixed-size scale filter.
fn live_input_args(input: &str, kind: InputKind, opts: &LiveOptions) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    let mut push = |items: &[&str]| args.extend(items.iter().map(|s| s.to_string()));
    // Low latency: never buffer ahead of the engine.
    push(&["-fflags", "nobuffer"]);
    if kind == InputKind::Stream {
        push(&["-flags", "low_delay"]);
        let lower = input.to_ascii_lowercase();
        if lower.starts_with("rtsp") {
            // TCP: no lost RTP packets (torn frames) on a busy LAN.
            push(&["-rtsp_transport", "tcp"]);
        } else if lower.starts_with("http") {
            // A hung server errors out instead of wedging the pipe; the
            // supervisor restarts it.
            push(&["-rw_timeout", "5000000"]);
        }
        push(&["-i", input, "-an"]);
    } else {
        push(&["-thread_queue_size", "64"]);
        let (format, device) = match kind {
            InputKind::DirectShow => ("dshow", format!("video={}", &input["dshow:".len()..])),
            _ => ("v4l2", input["v4l2:".len()..].to_string()),
        };
        push(&["-f", format]);
        if let Some(fmt) = opts.input_format.as_deref().filter(|f| !f.is_empty()) {
            let key = if kind == InputKind::V4l2 {
                "-input_format"
            } else {
                "-vcodec"
            };
            push(&[key, fmt]);
        }
        if opts.capture_width > 0 && opts.capture_height > 0 {
            let size = format!("{}x{}", opts.capture_width, opts.capture_height);
            push(&["-video_size", &size]);
        }
        if let Some(fps) = opts.fps.filter(|f| *f > 0.0) {
            push(&["-framerate", &format!("{fps}")]);
        }
        push(&["-i", &device]);
    }
    // Live sources cannot be ffprobe'd before opening; scale to a known size
    // so the frame layout is fixed.
    push(&["-vf", &format!("scale={}:{}", opts.width, opts.height)]);
    args
}

/// Lines of ffmpeg stderr kept for diagnostics.
const STDERR_TAIL: usize = 20;

/// Kills the ffmpeg child from another thread (unblocks a pending read).
#[derive(Clone)]
pub struct DecoderKiller {
    child: Arc<Mutex<Child>>,
}

impl DecoderKiller {
    pub fn kill(&self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
        }
    }
}

/// Streaming decoder: raw BGR24 frames read from an ffmpeg pipe.
/// `dshow:<device name>` / `v4l2:<device path>` open a live device,
/// `rtsp://` / `http://` / ... a live network stream; anything else is a
/// file ffmpeg can read.
pub struct VideoDecoder {
    child: Arc<Mutex<Child>>,
    stdout: ChildStdout,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    info: VideoInfo,
    frame_bytes: usize,
    next_seq: i64,
    /// Live sources have no reliable per-frame pts on the raw pipe; stamp
    /// with the wall clock instead of seq/fps.
    live: bool,
    started: std::time::Instant,
}

impl VideoDecoder {
    /// Open `input` (file path or live device) at `start` seconds with the
    /// default live options.
    pub fn open(input: &str, start: f64) -> Result<VideoDecoder> {
        Self::open_with(input, start, &LiveOptions::default())
    }

    /// Open `input` with explicit live-capture options.
    pub fn open_with(input: &str, start: f64, opts: &LiveOptions) -> Result<VideoDecoder> {
        let kind = InputKind::of(input);
        let live = kind.is_live();
        let mut cmd = tool_command("ffmpeg");
        cmd.args(["-hide_banner", "-nostdin", "-v", "error"]);
        let mut info;
        if live {
            cmd.args(live_input_args(input, kind, opts));
            info = VideoInfo {
                width: opts.width,
                height: opts.height,
                fps: opts.fps.unwrap_or(60.0),
                duration_s: None,
            };
        } else {
            info = probe(input)?;
            if start > 0.0 {
                cmd.arg("-ss").arg(format!("{start}"));
            }
            cmd.args(["-i", input]);
            // Emit each decoded frame exactly once. Without this, ffmpeg's
            // default CFR behavior duplicates frames on variable-frame-rate
            // sources (phone captures), shifting frame indexes against any
            // decode-order consumer (PyAV in the Python oracle drifted a
            // full second on the WIN fixtures).
            cmd.args(["-fps_mode", "passthrough"]);
        }
        if info.width == 0 || info.height == 0 {
            bail!("invalid frame size {}x{}", info.width, info.height);
        }
        info.fps = info.fps.max(1.0);
        cmd.args(["-f", "rawvideo", "-pix_fmt", "bgr24", "-"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());
        let mut child = cmd.spawn().context("spawn ffmpeg")?;
        let stdout = child.stdout.take().context("ffmpeg stdout")?;
        let stderr_tail: Arc<Mutex<VecDeque<String>>> = Arc::default();
        if let Some(stderr) = child.stderr.take() {
            let tail = stderr_tail.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(|l| l.ok()) {
                    let mut tail = tail.lock().unwrap_or_else(|e| e.into_inner());
                    if tail.len() >= STDERR_TAIL {
                        tail.pop_front();
                    }
                    tail.push_back(line);
                }
            });
        }
        let frame_bytes = info.width * info.height * 3;
        Ok(VideoDecoder {
            child: Arc::new(Mutex::new(child)),
            stdout,
            stderr_tail,
            info,
            frame_bytes,
            next_seq: 0,
            live,
            started: std::time::Instant::now(),
        })
    }

    /// Open a file whose container frame rate is wrong (e.g. a 50 fps
    /// capture muxed as 25 fps): frames are stamped `seq / fps` and `start`
    /// is in real seconds.
    pub fn open_file_with_fps(input: &str, start: f64, fps: f64) -> Result<VideoDecoder> {
        if InputKind::of(input).is_live() {
            bail!("--fps only applies to files");
        }
        if fps.is_nan() || fps <= 0.0 {
            bail!("invalid fps {fps}");
        }
        let tagged = probe(input)?.fps.max(1.0);
        let mut decoder = Self::open(input, start * fps / tagged)?;
        decoder.info.fps = fps;
        decoder.info.duration_s = decoder.info.duration_s.map(|d| d * tagged / fps);
        Ok(decoder)
    }

    pub fn info(&self) -> &VideoInfo {
        &self.info
    }

    /// Handle that kills this decoder's ffmpeg from another thread.
    pub fn killer(&self) -> DecoderKiller {
        DecoderKiller {
            child: self.child.clone(),
        }
    }

    /// The last lines ffmpeg wrote to stderr (why a device failed).
    pub fn stderr_tail(&self) -> Vec<String> {
        self.stderr_tail
            .lock()
            .map(|t| t.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Read the next frame; `None` at end of stream. `ts` defaults to
    /// `seq / fps` unless the caller overrides it afterwards.
    pub fn next_frame(&mut self) -> Result<Option<Frame>> {
        let mut buf = vec![0u8; self.frame_bytes];
        let mut filled = 0usize;
        while filled < buf.len() {
            let n = self.stdout.read(&mut buf[filled..])?;
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
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_kinds() {
        assert_eq!(InputKind::of("v4l2:/dev/video0"), InputKind::V4l2);
        assert_eq!(InputKind::of("dshow:USB Video"), InputKind::DirectShow);
        assert_eq!(InputKind::of("/tmp/game.mp4"), InputKind::File);
        assert_eq!(InputKind::of(r"C:\x\game.mp4"), InputKind::File);
        for url in [
            "http://127.0.0.1:1984/api/stream.mjpeg?src=nes",
            "RTSP://127.0.0.1:8554/nes",
            "srt://10.0.0.2:9000",
        ] {
            assert_eq!(InputKind::of(url), InputKind::Stream, "{url}");
            assert!(InputKind::of(url).is_live());
            assert!(device_path(url).is_none());
        }
    }

    fn opts() -> LiveOptions {
        LiveOptions {
            input_format: Some("mjpeg".into()),
            capture_width: 720,
            capture_height: 576,
            fps: Some(50.0),
            width: 720,
            height: 576,
        }
    }

    #[test]
    fn v4l2_args() {
        let args = live_input_args("v4l2:/dev/video0", InputKind::V4l2, &opts());
        assert_eq!(
            args.join(" "),
            "-fflags nobuffer -thread_queue_size 64 -f v4l2 -input_format mjpeg \
             -video_size 720x576 -framerate 50 -i /dev/video0 -vf scale=720:576"
        );
    }

    #[test]
    fn stream_args_ignore_device_options() {
        let url = "http://127.0.0.1:1984/api/stream.mjpeg?src=nes";
        let args = live_input_args(url, InputKind::Stream, &opts()).join(" ");
        assert_eq!(
            args,
            format!(
                "-fflags nobuffer -flags low_delay -rw_timeout 5000000 -i {url} -an -vf scale=720:576"
            )
        );
        let args = live_input_args("rtsp://h/nes", InputKind::Stream, &opts()).join(" ");
        assert!(
            args.contains("-rtsp_transport tcp -i rtsp://h/nes"),
            "{args}"
        );
        assert!(!args.contains("-input_format"));
    }
}

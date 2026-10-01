//! `nestris screens`: screen-signature tooling.
//!
//! - `refs`: build the embedded reference set (`assets/screens/*.png`) from
//!   labelled capture ranges listed in a JSON spec.
//! - `eval`: run the full engine over a capture and score its `game_state`
//!   against a hand-labelled timeline (confusion matrix, mismatch runs,
//!   detected game starts).
//! - `dump`: write raw (and rectified) frames at given times for inspection.

use std::collections::BTreeMap;
use std::fs;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use nestris_engine::enums::GameState;
use nestris_engine::geometry_cal::calibration::Rectifier;
use nestris_engine::geometry_cal::lock::LockState;
use nestris_engine::palette::to_luma;
use nestris_engine::processor::FrameProcessor;
use nestris_engine::state::screen_sig::{
    COLS, ROWS, ScreenKind, ScreenRef, SignatureMatcher, TILES, TileGrid, content_box,
};
use nestris_host::capture_ffmpeg::VideoDecoder;
use nestris_host::recalib_thread::RecalibThread;
use nestris_vision::Image;

use crate::{config_load, engine_config};

#[derive(Subcommand)]
pub enum ScreensCmd {
    /// Build the screen reference PNGs from labelled capture ranges.
    Refs {
        /// JSON spec: `{"fps":50,"std_max":10,"inputs":[{"file":..,
        /// "geometry_at":s,"refs":{"title":[[start,end,stride?],..],..}}]}`.
        #[arg(long)]
        spec: PathBuf,
        /// Directory the spec's `file` entries are relative to.
        #[arg(long)]
        videos: PathBuf,
        /// Output directory (normally crates/nestris-engine/assets/screens).
        #[arg(long)]
        out: PathBuf,
        /// Also write one rectified sample frame per screen here.
        #[arg(long)]
        samples: Option<PathBuf>,
    },
    /// Score the engine's game_state against a labelled timeline.
    Eval {
        #[arg(long)]
        input: String,
        /// Real frame rate for files muxed with a wrong rate.
        #[arg(long)]
        fps: Option<f64>,
        /// Label file: `start end state [margin]` per line (seconds).
        #[arg(long)]
        labels: Option<PathBuf>,
        /// Engine config file.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Engine config overrides (`path=value`, repeatable).
        #[arg(long = "set", value_name = "PATH=VALUE")]
        set: Vec<String>,
        /// Print the predicted state timeline (runs of at least N frames).
        #[arg(long, default_value_t = 0)]
        timeline: u64,
        /// Print the N longest mismatch runs.
        #[arg(long, default_value_t = 25)]
        mismatches: usize,
        /// Per labelled state: signature scores of the rectified frames.
        #[arg(long)]
        sig_stats: bool,
    },
    /// Write raw frames (and, with --geometry-at, rectified ones) as PNG.
    Dump {
        #[arg(long)]
        input: String,
        #[arg(long)]
        fps: Option<f64>,
        /// Times in seconds (comma separated).
        #[arg(long, value_delimiter = ',')]
        at: Vec<f64>,
        /// Lock the geometry on gameplay at this time first.
        #[arg(long)]
        geometry_at: Option<f64>,
        #[arg(long)]
        out: PathBuf,
    },
}

pub fn run(cmd: ScreensCmd) -> Result<()> {
    match cmd {
        ScreensCmd::Refs {
            spec,
            videos,
            out,
            samples,
        } => refs(&spec, &videos, &out, samples.as_deref()),
        ScreensCmd::Eval {
            input,
            fps,
            labels,
            config,
            set,
            timeline,
            mismatches,
            sig_stats,
        } => {
            let cfg = config_load::load(config.as_deref(), None, &set)?;
            eval(
                &input,
                fps,
                labels.as_deref(),
                cfg,
                timeline,
                mismatches,
                sig_stats,
            )
            .map(|s| {
                if s.scored > 0 {
                    eprintln!(
                        "accuracy {:.4}, {} game starts",
                        s.accuracy(),
                        s.new_games.len()
                    );
                }
            })
        }
        ScreensCmd::Dump {
            input,
            fps,
            at,
            geometry_at,
            out,
        } => dump(&input, fps, &at, geometry_at, &out),
    }
}

pub fn open(input: &str, start: f64, fps: Option<f64>) -> Result<VideoDecoder> {
    match fps {
        Some(fps) => VideoDecoder::open_file_with_fps(input, start, fps),
        None => VideoDecoder::open(input, start),
    }
}

/// Lock the geometry on the gameplay around `at` and return its rectifier.
fn acquire_rectifier(input: &str, at: f64, fps: Option<f64>) -> Result<Rectifier> {
    let mut decoder = open(input, at, fps)?;
    let rate = decoder.info().fps;
    let mut processor = FrameProcessor::new(engine_config(true));
    let mut locked_for = 0u32;
    for _ in 0..(rate * 30.0) as usize {
        let Some(frame) = decoder.next_frame()? else {
            break;
        };
        processor.process(&frame);
        if processor.lock_state() == LockState::Locked {
            locked_for += 1;
            // Let the lock settle (revalidation/refinement) before taking it.
            if locked_for >= 25
                && let Some(r) = processor.lock().rectifier()
            {
                return Rectifier::new(*r.matrix(), r.undistort().cloned())
                    .context("rebuild rectifier");
            }
        } else {
            locked_for = 0;
        }
    }
    bail!("{input}: no geometry lock within 30 s of {at:.1}s")
}

/// Collected grids of one screen, tagged with their input.
struct Accum {
    grids: Vec<(usize, Vec<f32>)>,
    sample: Option<Image>,
}

/// Frames that correlate below this with the screen's mean are mislabelled
/// (a range crossed a screen change) and dropped before the final pass.
const KEEP_CORRELATION: f64 = 0.9;

impl Accum {
    fn new() -> Accum {
        Accum {
            grids: Vec::new(),
            sample: None,
        }
    }

    fn push(&mut self, input: usize, grid: &TileGrid) {
        self.grids.push((input, grid.luma.clone()));
    }

    fn mean_of(grids: &[&(usize, Vec<f32>)]) -> Vec<f64> {
        let mut mean = vec![0.0f64; TILES];
        for (_, g) in grids {
            for (m, &v) in mean.iter_mut().zip(g) {
                *m += v as f64;
            }
        }
        let n = grids.len().max(1) as f64;
        mean.iter_mut().for_each(|m| *m /= n);
        mean
    }

    fn correlation(a: &[f32], b: &[f64]) -> f64 {
        let n = a.len() as f64;
        let ma = a.iter().map(|&v| v as f64).sum::<f64>() / n;
        let mb = b.iter().sum::<f64>() / n;
        let (mut dot, mut sa, mut sb) = (0.0, 0.0, 0.0);
        for (&x, &y) in a.iter().zip(b) {
            let (dx, dy) = (x as f64 - ma, y - mb);
            dot += dx * dy;
            sa += dx * dx;
            sb += dy * dy;
        }
        if sa <= 0.0 || sb <= 0.0 {
            return 0.0;
        }
        dot / (sa.sqrt() * sb.sqrt())
    }

    /// Trim outliers, then the mean and the mask. The mask uses the variance
    /// *within* each input: two captures differ by gain/offset and a
    /// sub-tile geometry shift, neither of which makes a tile unstable.
    fn finish(&self, kind: ScreenKind, std_max: f64) -> (ScreenRef, usize, usize) {
        let mut kept: Vec<&(usize, Vec<f32>)> = self.grids.iter().collect();
        for _ in 0..3 {
            let mean = Self::mean_of(&kept);
            let next: Vec<_> = self
                .grids
                .iter()
                .filter(|(_, g)| Self::correlation(g, &mean) >= KEEP_CORRELATION)
                .collect();
            if next.is_empty() {
                break;
            }
            kept = next;
        }
        let mean = Self::mean_of(&kept);
        let mut inputs: BTreeMap<usize, Vec<&Vec<f32>>> = BTreeMap::new();
        for (input, g) in &kept {
            inputs.entry(*input).or_default().push(g);
        }
        let mut std = vec![0.0f64; TILES];
        for grids in inputs.values() {
            let n = grids.len() as f64;
            for (i, s) in std.iter_mut().enumerate() {
                let m = grids.iter().map(|g| g[i] as f64).sum::<f64>() / n;
                let var = grids.iter().map(|g| (g[i] as f64 - m).powi(2)).sum::<f64>() / n;
                *s += n * var.sqrt();
            }
        }
        let total = kept.len().max(1) as f64;
        let r = ScreenRef {
            kind,
            mean: mean.iter().map(|&m| m as f32).collect(),
            mask: std.iter().map(|&s| s / total <= std_max).collect(),
        };
        (r, kept.len(), self.grids.len())
    }
}

fn refs(spec_path: &Path, videos: &Path, out: &Path, samples: Option<&Path>) -> Result<()> {
    let spec: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(spec_path).with_context(|| format!("read {}", spec_path.display()))?,
    )?;
    let fps = spec["fps"].as_f64();
    let std_max = spec["std_max"].as_f64().unwrap_or(10.0);
    // Keyed by reference name: `<kind>` or `<kind>.<variant>` (several
    // references per screen, e.g. the A- and B-type endings).
    let mut acc: BTreeMap<String, (ScreenKind, Accum)> = BTreeMap::new();

    for (input_no, input) in spec["inputs"]
        .as_array()
        .context("spec.inputs")?
        .iter()
        .enumerate()
    {
        let file = videos.join(input["file"].as_str().context("input.file")?);
        let file = file.to_string_lossy().to_string();
        let geometry_at = input["geometry_at"].as_f64().context("input.geometry_at")?;
        // Per-input override; `null` = the container's own rate.
        let fps = match input.get("fps") {
            Some(v) => v.as_f64(),
            None => fps,
        };
        let rectifier = acquire_rectifier(&file, geometry_at, fps)?;
        eprintln!("{file}: geometry locked at {geometry_at}s");
        let refs = input["refs"].as_object().context("input.refs")?;
        for (name, ranges) in refs {
            let kind_name = name.split('.').next().unwrap_or(name);
            let kind = ScreenKind::from_name(kind_name)
                .with_context(|| format!("unknown screen kind {kind_name}"))?;
            let (_, a) = acc.entry(name.clone()).or_insert((kind, Accum::new()));
            for range in ranges.as_array().context("ranges")? {
                let r = range.as_array().context("range")?;
                let start = r[0].as_f64().context("range start")?;
                let end = r[1].as_f64().context("range end")?;
                let stride = r.get(2).and_then(|v| v.as_u64()).unwrap_or(1).max(1);
                let mut decoder = open(&file, start, fps)?;
                let frames = ((end - start) * decoder.info().fps) as u64;
                let mut taken = 0u64;
                for i in 0..frames {
                    let Some(frame) = decoder.next_frame()? else {
                        break;
                    };
                    if i % stride != 0 {
                        continue;
                    }
                    let canon = rectifier.rectify(&frame.image);
                    let grid = TileGrid::from_gray(&to_luma(&canon));
                    a.push(input_no, &grid);
                    taken += 1;
                    if a.sample.is_none() && i >= frames / 2 {
                        a.sample = Some(canon);
                    }
                }
                eprintln!("  {name}: {start:.1}-{end:.1}s -> {taken} frames");
            }
        }
    }

    fs::create_dir_all(out)?;
    if let Some(dir) = samples {
        fs::create_dir_all(dir)?;
    }
    for (name, (kind, a)) in &acc {
        if a.grids.is_empty() {
            bail!("{name}: no frames");
        }
        let (r, kept_frames, total) = a.finish(*kind, std_max);
        write_png(&out.join(format!("{name}.png")), &r.to_image())?;
        let kept = r.mask.iter().filter(|&&k| k).count();
        println!("{name}: {kept_frames}/{total} frames kept, {kept}/{TILES} tiles compared");
        if let (Some(dir), Some(sample)) = (samples, &a.sample) {
            write_png(&dir.join(format!("{name}.png")), sample)?;
            write_png(&dir.join(format!("{name}_mask.png")), &mask_view(&r))?;
        }
    }
    Ok(())
}

/// 8× upscaled reference view: compared tiles at their mean, masked ones red.
fn mask_view(r: &ScreenRef) -> Image {
    let mut img = Image::new(COLS * 8, ROWS * 8, 3);
    for y in 0..ROWS * 8 {
        for x in 0..COLS * 8 {
            let i = (y / 8) * COLS + x / 8;
            let v = r.mean[i].round().clamp(0.0, 255.0) as u8;
            let px = if r.mask[i] { [v, v, v] } else { [0, 0, 160] };
            img.pixel_mut(x, y).copy_from_slice(&px);
        }
    }
    img
}

/// PNG writer for 1-channel (gray) or 3-channel BGR images.
pub fn write_png(path: &Path, img: &Image) -> Result<()> {
    let file = fs::File::create(path).with_context(|| format!("create {}", path.display()))?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), img.width as u32, img.height as u32);
    encoder.set_depth(png::BitDepth::Eight);
    let data = match img.channels {
        1 => {
            encoder.set_color(png::ColorType::Grayscale);
            img.data.clone()
        }
        3 => {
            encoder.set_color(png::ColorType::Rgb);
            img.data
                .chunks_exact(3)
                .flat_map(|p| [p[2], p[1], p[0]])
                .collect()
        }
        c => bail!("cannot write {c}-channel png"),
    };
    encoder.write_header()?.write_image_data(&data)?;
    Ok(())
}

/// What `eval` measured.
pub struct EvalSummary {
    pub new_games: Vec<f64>,
    pub scored: u64,
    pub correct: u64,
}

impl EvalSummary {
    pub fn accuracy(&self) -> f64 {
        self.correct as f64 / self.scored.max(1) as f64
    }
}

struct Segment {
    start: f64,
    end: f64,
    /// Accepted states; empty = not scored.
    states: Vec<String>,
    margin: f64,
}

fn parse_labels(path: &Path) -> Result<Vec<Segment>> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 3 {
            bail!(
                "{}:{}: expected `start end state [margin]`",
                path.display(),
                n + 1
            );
        }
        let states = if cols[2] == "*" {
            Vec::new()
        } else {
            cols[2].split('|').map(str::to_string).collect()
        };
        out.push(Segment {
            start: cols[0].parse()?,
            end: cols[1].parse()?,
            states,
            margin: cols.get(3).map(|m| m.parse()).transpose()?.unwrap_or(0.3),
        });
    }
    Ok(out)
}

fn state_name(state: GameState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

#[derive(Default)]
struct SigStat {
    n: u64,
    best_kind: BTreeMap<&'static str, u64>,
    best_score_sum: f64,
    best_score_min: f64,
    margin_min: f64,
}

#[allow(clippy::too_many_arguments)]
fn eval(
    input: &str,
    fps: Option<f64>,
    labels: Option<&Path>,
    cfg: nestris_engine::config::EngineConfig,
    timeline: u64,
    mismatches: usize,
    sig_stats: bool,
) -> Result<EvalSummary> {
    let segments = labels.map(parse_labels).transpose()?.unwrap_or_default();
    let background = cfg.calibration.background_recalibration;
    let mut decoder = open(input, 0.0, fps)?;
    let mut processor = FrameProcessor::new(cfg);
    let mut recalib = background.then(RecalibThread::start);
    let matcher = SignatureMatcher::builtin();

    // (ts, predicted, label index)
    let mut frames: Vec<(f64, String, Option<usize>)> = Vec::new();
    let mut new_games: Vec<f64> = Vec::new();
    let mut sig: BTreeMap<String, SigStat> = BTreeMap::new();
    let started = std::time::Instant::now();
    while let Some(frame) = decoder.next_frame()? {
        let out = processor.process(&frame);
        if let Some(r) = &mut recalib {
            r.drive(&mut processor, &frame);
        }
        if out.events.iter().any(|e| e.reason == "new_game") {
            new_games.push(frame.ts);
        }
        let seg = segments
            .iter()
            .position(|s| frame.ts >= s.start && frame.ts < s.end);
        if sig_stats
            && let (Some(i), Some(canon)) = (seg, processor.last_canonical())
            && !segments[i].states.is_empty()
            && let Some(best) = matcher.best(&TileGrid::from_gray(&to_luma(canon)))
        {
            let s = sig.entry(segments[i].states.join("|")).or_insert(SigStat {
                best_score_min: f64::MAX,
                margin_min: f64::MAX,
                ..Default::default()
            });
            s.n += 1;
            *s.best_kind.entry(best.kind.name()).or_default() += 1;
            s.best_score_sum += best.score as f64;
            s.best_score_min = s.best_score_min.min(best.score as f64);
            s.margin_min = s.margin_min.min(best.margin as f64);
        }
        frames.push((frame.ts, state_name(out.game_state), seg));
    }
    let wall = started.elapsed().as_secs_f64();
    eprintln!(
        "{input}: {} frames in {wall:.1}s ({:.0} fps)",
        frames.len(),
        frames.len() as f64 / wall.max(1e-9)
    );

    if timeline > 0 {
        println!("-- timeline (runs >= {timeline} frames)");
        for (a, b, state) in runs(&frames, |f| f.1.clone()) {
            if (b - a + 1) as u64 >= timeline {
                println!(
                    "{:8.2}-{:8.2}s {:6}f  {state}",
                    frames[a].0,
                    frames[b].0,
                    b - a + 1
                );
            }
        }
    }
    println!(
        "-- new games at: {}",
        new_games
            .iter()
            .map(|t| format!("{t:.2}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut summary = EvalSummary {
        new_games: new_games.clone(),
        scored: 0,
        correct: 0,
    };
    if segments.is_empty() {
        return Ok(summary);
    }

    // Score: scored frames lie inside a labelled segment, away from its edges.
    let scored = |ts: f64, i: usize| {
        let s = &segments[i];
        !s.states.is_empty() && ts >= s.start + s.margin && ts < s.end - s.margin
    };
    let mut confusion: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let (mut total, mut correct) = (0u64, 0u64);
    let mut ok_flags: Vec<Option<bool>> = Vec::with_capacity(frames.len());
    for (ts, pred, seg) in &frames {
        let flag = seg.filter(|&i| scored(*ts, i)).map(|i| {
            let s = &segments[i];
            let ok = s.states.iter().any(|st| st == pred);
            *confusion
                .entry(s.states.join("|"))
                .or_default()
                .entry(pred.clone())
                .or_default() += 1;
            total += 1;
            correct += ok as u64;
            ok
        });
        ok_flags.push(flag);
    }
    println!("-- confusion (label -> predicted frames)");
    for (label, preds) in &confusion {
        let n: u64 = preds.values().sum();
        let ok: u64 = preds
            .iter()
            .filter(|(p, _)| label.split('|').any(|l| l == p.as_str()))
            .map(|(_, c)| c)
            .sum();
        let detail = preds
            .iter()
            .map(|(p, c)| format!("{p}={c}"))
            .collect::<Vec<_>>()
            .join(" ");
        println!(
            "{label:>20}: {:6.2}% of {n:6}  [{detail}]",
            100.0 * ok as f64 / n.max(1) as f64
        );
    }
    println!(
        "-- overall: {:.2}% of {total} scored frames",
        100.0 * correct as f64 / total.max(1) as f64
    );
    summary.scored = total;
    summary.correct = correct;

    let mut bad: Vec<(usize, usize)> = runs(&frames, |f| f.1.clone())
        .into_iter()
        .flat_map(|(a, b, _)| {
            // Split prediction runs into their mismatching sub-runs.
            let mut out = Vec::new();
            let mut start = None;
            for (i, flag) in ok_flags.iter().enumerate().take(b + 1).skip(a) {
                match (flag, start) {
                    (Some(false), None) => start = Some(i),
                    (Some(false), Some(_)) => {}
                    (_, Some(s)) => {
                        out.push((s, i - 1));
                        start = None;
                    }
                    _ => {}
                }
            }
            if let Some(s) = start {
                out.push((s, b));
            }
            out
        })
        .collect();
    bad.sort_by_key(|(a, b)| std::cmp::Reverse(b - a));
    if !bad.is_empty() {
        println!("-- longest mismatch runs");
    }
    for (a, b) in bad.into_iter().take(mismatches) {
        let label = frames[a]
            .2
            .map(|i| segments[i].states.join("|"))
            .unwrap_or_default();
        println!(
            "{:8.2}-{:8.2}s {:5}f  label={label} got={}",
            frames[a].0,
            frames[b].0,
            b - a + 1,
            frames[a].1
        );
    }

    if sig_stats {
        println!("-- signature scores on rectified frames (per label)");
        for (label, s) in &sig {
            println!(
                "{label:>20}: n={:6} mean={:.3} min={:.3} min_margin={:.3} best={:?}",
                s.n,
                s.best_score_sum / s.n.max(1) as f64,
                s.best_score_min,
                s.margin_min,
                s.best_kind
            );
        }
    }
    Ok(summary)
}

fn runs<T>(items: &[T], key: impl Fn(&T) -> String) -> Vec<(usize, usize, String)> {
    let mut out: Vec<(usize, usize, String)> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let k = key(item);
        match out.last_mut() {
            Some(last) if last.2 == k => last.1 = i,
            _ => out.push((i, i, k)),
        }
    }
    out
}

fn dump(
    input: &str,
    fps: Option<f64>,
    at: &[f64],
    geometry_at: Option<f64>,
    out: &Path,
) -> Result<()> {
    fs::create_dir_all(out)?;
    let rectifier = geometry_at
        .map(|t| acquire_rectifier(input, t, fps))
        .transpose()?;
    let matcher = SignatureMatcher::builtin();
    for &t in at {
        let mut decoder = open(input, t, fps)?;
        let Some(frame) = decoder.next_frame()? else {
            bail!("no frame at {t}s");
        };
        let stem = format!("t{t:08.2}");
        write_png(&out.join(format!("{stem}_raw.png")), &frame.image)?;
        match content_box(&frame.image) {
            Some((x, y, w, h)) => {
                let grid = TileGrid::from_bgr_region(&frame.image, x, y, w, h);
                let mut scores = matcher.scores_pooled(&grid);
                scores.sort_by(|a, b| b.1.total_cmp(&a.1));
                println!(
                    "{t:8.2}s  box=({x:.0},{y:.0},{w:.0}x{h:.0}) pooled: {}",
                    scores
                        .iter()
                        .take(3)
                        .map(|(k, s)| format!("{}={s:.3}", k.name()))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
            }
            None => println!("{t:8.2}s  no frame box"),
        }
        if let Some(r) = &rectifier {
            let canon = r.rectify(&frame.image);
            write_png(&out.join(format!("{stem}_canon.png")), &canon)?;
            let grid = TileGrid::from_gray(&to_luma(&canon));
            let mut scores = matcher.scores(&grid);
            scores.sort_by(|a, b| b.1.total_cmp(&a.1));
            println!(
                "{t:8.2}s  {}",
                scores
                    .iter()
                    .map(|(k, s)| format!("{}={s:.3}", k.name()))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Full station captures against their hand labels. Needs the videos:
    /// `NESTRIS_SCREEN_VIDEOS=<absolute dir with aufnahme_*.mkv> cargo test --release
    /// -p nestris-cli -- --ignored station_captures`.
    #[test]
    #[ignore = "needs the capture videos (NESTRIS_SCREEN_VIDEOS)"]
    fn station_captures() {
        let Ok(dir) = std::env::var("NESTRIS_SCREEN_VIDEOS") else {
            eprintln!("NESTRIS_SCREEN_VIDEOS not set, skipped");
            return;
        };
        let labels = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/screens");
        // (capture, real game starts)
        for (name, starts) in [
            ("aufnahme_20260930-233153", vec![50.5, 115.1]),
            (
                "aufnahme_20260930-231217",
                vec![162.75, 1005.35, 1056.05, 1099.75],
            ),
        ] {
            let input = PathBuf::from(&dir).join(format!("{name}.mkv"));
            let summary = eval(
                &input.to_string_lossy(),
                Some(50.0),
                Some(&labels.join(format!("{name}.labels.tsv"))),
                nestris_engine::config::EngineConfig::default(),
                0,
                10,
                false,
            )
            .unwrap();
            assert!(
                summary.accuracy() >= 0.995,
                "{name}: {:.4}",
                summary.accuracy()
            );
            assert_eq!(
                summary.new_games.len(),
                starts.len(),
                "{name}: {:?}",
                summary.new_games
            );
            for (got, want) in summary.new_games.iter().zip(&starts) {
                assert!((got - want).abs() < 0.5, "{name}: start {got} vs {want}");
            }
        }
    }
}

//! `nestris demo`: attract-mode demo fingerprints.
//!
//! - `learn`: build a fingerprint from two or more recordings of the same
//!   demo (`.ngf` / `.ngf.gz`), e.g. the demo games a station recorded
//!   before demo detection existed.
//! - `check`: show the detector's verdict for recordings.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use nestris_engine::integrity::demo::{
    DemoConfig, DemoDetector, DemoVerdict, Entry, Fingerprint, StepRecorder, builtin_fingerprints,
    consensus, parse_fingerprints,
};
use nestris_engine::output::OutputFrame;
use nestris_ngf::replay::{ReplayEngine, ReplayFile};

#[derive(Subcommand)]
pub enum DemoCmd {
    /// Build a fingerprint from recordings of the same demo.
    Learn {
        /// Two or more recordings of the demo.
        #[arg(required = true, num_args = 2..)]
        files: Vec<PathBuf>,
        /// Fingerprint id, e.g. `nes-ntsc-event`.
        #[arg(long)]
        id: String,
        /// Free text: which ROM the demo comes from.
        #[arg(long, default_value = "")]
        rom: String,
        /// Write the fingerprint here (default: print it).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Run the demo detector over recordings.
    Check {
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// How the game was entered: `title`, `menu` or `unknown`.
        /// Recordings hold no menu screens; `title` is what a station sees
        /// before a demo.
        #[arg(long, default_value = "title")]
        entry: String,
        /// Also cut each recording after this many seconds (an aborted
        /// demo); repeatable.
        #[arg(long = "cut")]
        cuts: Vec<f64>,
        /// Extra fingerprint files on top of the built-in ones.
        #[arg(long = "fingerprints")]
        fingerprints: Vec<PathBuf>,
    },
}

pub fn run(cmd: DemoCmd) -> Result<()> {
    match cmd {
        DemoCmd::Learn {
            files,
            id,
            rom,
            out,
        } => learn(&files, &id, &rom, out.as_deref()),
        DemoCmd::Check {
            files,
            entry,
            cuts,
            fingerprints,
        } => check(&files, &entry, &cuts, &fingerprints),
    }
}

fn frames(path: &Path) -> Result<Vec<OutputFrame>> {
    let file = ReplayFile::load(path).with_context(|| format!("reading {}", path.display()))?;
    if file.frames.is_empty() {
        bail!("{} holds no frames", path.display());
    }
    let mut engine = ReplayEngine::new(file);
    engine.extended_stats = false;
    Ok((0..engine.frame_count())
        .map(|i| engine.output_at(i))
        .collect())
}

fn learn(files: &[PathBuf], id: &str, rom: &str, out: Option<&Path>) -> Result<()> {
    let mut runs = Vec::new();
    for path in files {
        let mut rec = StepRecorder::new();
        for f in frames(path)? {
            rec.push(&f);
        }
        let steps = rec.finish();
        eprintln!("{}: {} steps", path.display(), steps.len());
        runs.push(steps);
    }
    let fp = consensus(id, rom, &runs).map_err(anyhow::Error::msg)?;
    let longest = runs.iter().map(Vec::len).max().unwrap_or(0);
    eprintln!(
        "fingerprint {id}: {} steps (longest recording {longest}), ends at score {} / lines {}",
        fp.steps.len(),
        fp.steps.last().map_or(0, |s| s.score),
        fp.steps.last().map_or(0, |s| s.lines),
    );
    let json = serde_json::to_string_pretty(&fp)?;
    match out {
        Some(path) => std::fs::write(path, json + "\n")
            .with_context(|| format!("writing {}", path.display()))?,
        None => println!("{json}"),
    }
    Ok(())
}

fn check(files: &[PathBuf], entry: &str, cuts: &[f64], extra: &[PathBuf]) -> Result<()> {
    let entry = match entry {
        "title" => Entry::Title,
        "menu" => Entry::Menu,
        "unknown" => Entry::Unknown,
        other => bail!("unknown entry {other:?} (title, menu, unknown)"),
    };
    let mut fps: Vec<Fingerprint> = builtin_fingerprints();
    for path in extra {
        let text = std::fs::read_to_string(path)?;
        fps.extend(
            parse_fingerprints(&text).with_context(|| format!("parsing {}", path.display()))?,
        );
    }
    for path in files {
        let all = frames(path)?;
        let t0 = all[0].ts;
        let mut limits: Vec<Option<f64>> = vec![None];
        limits.extend(cuts.iter().map(|c| Some(*c)));
        for limit in limits {
            let mut d = DemoDetector::new(DemoConfig::default(), fps.clone(), entry);
            let mut at = None;
            for f in &all {
                if limit.is_some_and(|l| f.ts - t0 > l) {
                    break;
                }
                if d.push(f) != &DemoVerdict::Pending && at.is_none() {
                    at = Some(f.ts - t0);
                }
            }
            let matched = d.matched_steps();
            let verdict = d.finish().clone();
            let label = match &verdict {
                DemoVerdict::Demo(id) => format!("DEMO ({id})"),
                DemoVerdict::Real(r) => format!("real ({})", r.as_str()),
                DemoVerdict::Pending => "pending".into(),
            };
            let cut = limit.map_or(String::new(), |l| format!(" cut at {l} s"));
            let when = at.map_or("at the end".to_string(), |t| format!("after {t:.1} s"));
            println!(
                "{}{cut}: {label}, {when}, {matched} steps matched",
                path.display()
            );
        }
    }
    Ok(())
}

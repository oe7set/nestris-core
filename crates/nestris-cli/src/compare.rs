//! `nestris compare`: per-field agreement between two `run --jsonl` outputs
//! of the same source, e.g. a downscaled run against the native one
//! (docs/DOWNSCALE.md).
//!
//! Frames pair up by `seq`. A field counts as agreeing when the other run
//! shows the same value within `slack` frames (a reading that settles one
//! frame later is a timing shift, not a misread). Field values are only
//! scored where both runs are in game.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;

/// Default tolerance in frames, as `verify`'s transition slack.
pub const DEFAULT_SLACK: usize = 3;

const FIELDS: [&str; 4] = ["score", "lines", "level", "next_piece"];

struct Row {
    seq: i64,
    ts: f64,
    state: String,
    fields: [Value; 4],
    playfield: Option<Vec<u8>>,
    new_game: bool,
}

fn load(path: &Path) -> Result<Vec<Row>> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut rows = Vec::new();
    for (n, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&line)
            .with_context(|| format!("{}:{}: not JSON", path.display(), n + 1))?;
        let f = &v["fields"];
        rows.push(Row {
            seq: v["seq"].as_i64().unwrap_or(n as i64),
            ts: v["ts"].as_f64().unwrap_or(0.0),
            state: v["game_state"].as_str().unwrap_or("").to_string(),
            fields: FIELDS.map(|k| f[k].clone()),
            playfield: f["playfield"].as_array().map(|rows| {
                rows.iter()
                    .flat_map(|r| r.as_array().into_iter().flatten())
                    .map(|c| c.as_u64().unwrap_or(0) as u8)
                    .collect()
            }),
            new_game: v["events"]
                .as_array()
                .is_some_and(|e| e.iter().any(|e| e["reason"] == "new_game")),
        });
    }
    Ok(rows)
}

#[derive(Default, Serialize)]
pub struct FieldAgreement {
    pub scored: u64,
    pub agree: u64,
    pub rate: f64,
}

impl FieldAgreement {
    fn add(&mut self, ok: bool) {
        self.scored += 1;
        self.agree += ok as u64;
    }

    fn finish(&mut self) {
        self.rate = self.agree as f64 / self.scored.max(1) as f64;
    }
}

#[derive(Serialize)]
pub struct Comparison {
    pub frames: u64,
    pub game_state: FieldAgreement,
    pub fields: BTreeMap<&'static str, FieldAgreement>,
    /// Playfield cells (of 200) equal, over frames where both read one.
    pub playfield_cells: FieldAgreement,
    /// Frames whose whole playfield matches (within the slack).
    pub playfield_exact: FieldAgreement,
    /// Frames where only one run read a playfield.
    pub playfield_missing_a: u64,
    pub playfield_missing_b: u64,
    pub new_games_a: Vec<f64>,
    pub new_games_b: Vec<f64>,
}

/// Does `b` show `pick(a[i])` at some index within `slack` of `j`?
fn near<T: PartialEq>(
    b: &[&Row],
    j: usize,
    slack: usize,
    want: &T,
    pick: impl Fn(&Row) -> T,
) -> bool {
    let lo = j.saturating_sub(slack);
    let hi = (j + slack).min(b.len() - 1);
    (lo..=hi).any(|k| pick(b[k]) == *want)
}

pub fn compare(a_path: &Path, b_path: &Path, slack: usize) -> Result<Comparison> {
    let a = load(a_path)?;
    let b = load(b_path)?;
    let b_by_seq: BTreeMap<i64, usize> = b.iter().enumerate().map(|(i, r)| (r.seq, i)).collect();
    let b_refs: Vec<&Row> = b.iter().collect();
    let mut out = Comparison {
        frames: 0,
        game_state: FieldAgreement::default(),
        fields: FIELDS
            .iter()
            .map(|k| (*k, FieldAgreement::default()))
            .collect(),
        playfield_cells: FieldAgreement::default(),
        playfield_exact: FieldAgreement::default(),
        playfield_missing_a: 0,
        playfield_missing_b: 0,
        new_games_a: a.iter().filter(|r| r.new_game).map(|r| r.ts).collect(),
        new_games_b: b.iter().filter(|r| r.new_game).map(|r| r.ts).collect(),
    };
    for ra in &a {
        let Some(&j) = b_by_seq.get(&ra.seq) else {
            continue;
        };
        let rb = b_refs[j];
        out.frames += 1;
        out.game_state
            .add(near(&b_refs, j, slack, &ra.state, |r| r.state.clone()));
        if ra.state != "in_game" || rb.state != "in_game" {
            continue;
        }
        for (i, key) in FIELDS.iter().enumerate() {
            if ra.fields[i].is_null() && rb.fields[i].is_null() {
                continue;
            }
            let ok = near(&b_refs, j, slack, &ra.fields[i], |r| r.fields[i].clone());
            out.fields.get_mut(key).expect("known field").add(ok);
        }
        match (&ra.playfield, &rb.playfield) {
            (Some(pa), Some(pb)) => {
                let equal = pa.iter().zip(pb).filter(|(x, y)| x == y).count() as u64;
                out.playfield_cells.scored += pa.len() as u64;
                out.playfield_cells.agree += equal;
                let exact = near(&b_refs, j, slack, &ra.playfield, |r| r.playfield.clone());
                out.playfield_exact.add(exact);
            }
            (Some(_), None) => out.playfield_missing_b += 1,
            (None, Some(_)) => out.playfield_missing_a += 1,
            (None, None) => {}
        }
    }
    out.game_state.finish();
    out.fields.values_mut().for_each(FieldAgreement::finish);
    out.playfield_cells.finish();
    out.playfield_exact.finish();
    Ok(out)
}

pub fn print(c: &Comparison) {
    let pct = |f: &FieldAgreement| format!("{:7.3}% of {:7}", 100.0 * f.rate, f.scored);
    println!("frames paired: {}", c.frames);
    println!("{:>16}: {}", "game_state", pct(&c.game_state));
    for (k, f) in &c.fields {
        println!("{k:>16}: {}", pct(f));
    }
    println!("{:>16}: {}", "playfield cells", pct(&c.playfield_cells));
    println!("{:>16}: {}", "playfield exact", pct(&c.playfield_exact));
    println!(
        "{:>16}: only a {} / only b {}",
        "playfield read", c.playfield_missing_b, c.playfield_missing_a
    );
    let starts = |v: &[f64]| {
        v.iter()
            .map(|t| format!("{t:.2}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    println!("new games a: {}", starts(&c.new_games_a));
    println!("new games b: {}", starts(&c.new_games_b));
}

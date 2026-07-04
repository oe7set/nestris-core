# Statistics reference

Exact definitions of every value the engine computes, so dashboard numbers
are auditable. Base statistics (`stats` in the output frame) are a verified
port of the Python implementation; extended statistics (`stats_ext`) are a
nestris-core addition, attached to the wire format only when
`output.extended_stats` is enabled (both GUIs always compute them).

## Base statistics (`stats`)

| Field | Definition |
|---|---|
| `pps` | pieces / active seconds |
| `score_per_min` | score / active minutes |
| `active_seconds` | in-game time only; per-frame gaps are capped at 0.5 s, so pauses, menus, and dropped capture never dilute pace metrics |
| `tetris_rate` | tetris-cleared lines / total cleared lines |
| `burn` | total cleared lines − tetris-cleared lines |
| `drought` / `max_drought` | pieces since the last I piece (current / all-time max), reconciled against the on-screen STATISTICS I-count |
| `clears` | count of singles / doubles / triples / tetrises. Merged multi-line jumps (dropped frames) are disambiguated by the concurrent score delta against the NES base scores 40/100/300/1200 × (level+1) |
| `pieces` | spawn-counted via NEXT transitions, reconciled against the STATISTICS rail total when it is readable and confident |

## Extended statistics (`stats_ext`)

### `points` — score attribution

Every attributed clear contributes `base(size) × (level+1)` points to its
bucket (`singles`/`doubles`/`triples`/`tetrises`, NES bases 40/100/300/1200).
`drops` is the remainder `score − Σ attributed`, clamped at ≥ 0 — soft-drop
points plus anything scored while the level was still unknown.

### `efficiency` (EFF)

`Σ base(size) / total cleared lines` — level-independent clear points per
line. The maximum is 300 (tetris-only play: 1200 points per 4 lines);
all-singles play scores 40. `null` before the first clear.

### `pace_score` (PACE)

Projected score at **line 230** assuming the observed per-line clear mix
continues. The projection walks future lines in 10-line chunks, bumping the
level (and therefore the multiplier) each chunk:

```
base_per_line = Σ over clear sizes: lines_cleared(size) × base(size)/size
                ÷ total cleared lines
pace = score + Σ chunks: chunk_lines × base_per_line × (level_at_chunk + 1)
```

This is a **documented approximation**, not NestrisChamps' exact formula:
it ignores future soft-drop points and assumes level += 1 per 10 lines.
`null` until score, lines, level, and at least one clear are known; once
line 230 is reached it reports the actual score.

### `i_drought` (I-DRT)

`current` pieces since the last I; `last` = length of the previous drought
when it ended; `max` all-time; `count` = number of droughts of **≥ 13
pieces** that have ended. Rail-proven I spawns (STATISTICS increments the
NEXT tracking missed) also end droughts.

### `board` — stack shape (falling piece masked out)

| Field | Definition |
|---|---|
| `max_height` / `avg_height` | column heights in rows (0–20), from the fused playfield with the current piece's cells removed |
| `holes` | empty cells with at least one filled cell above them in the same column |
| `tetris_ready` | some column has ≥ 4 consecutive rows that are full except for that single column (a ready well) |
| `double_well` | ≥ 2 columns are at least 3 rows deeper than all their neighbors |
| `clean_slope` | zero holes and monotone column heights across the board |

### Time series (bounded)

- `trt_trend` — `(total lines, tetris rate)` sampled after every clear,
  capped at 512 samples (thinned to every 2nd sample on overflow).
- `height_timeline` — `(ts, max_height, flag bits)` sampled at most ~4 Hz
  during play, capped at 2048 (thinning halves the sample rate too). Flag
  bits: 1 tetris-ready, 2 double well, 4 clean slope, 8 in drought (≥ 13).

### `piece_dist`

Per-piece counts in STATISTICS order (T J Z O S L I) — the on-screen rail
when readable and confident, spawn counting otherwise — plus per-piece
droughts (pieces since that type last spawned) and `deviation`, the
coefficient of variation of the counts (0 = perfectly even distribution).

## Session high scores

Both GUIs record every finished game (detected on the in-game → game-over
transition) with score, lines, final level, tetris rate, and play time:

- Desktop: `%APPDATA%\nestris-core\session_pbs.json`
- Web: browser `localStorage`

The stats views show the top games for TODAY (local date) and OVERALL.

# NGF recordings & replay

`nestris-core` records every detected game as an **NGF file** (NestrisChamps
Game Format, `.ngf` / `.ngf.gz`) and plays such files back with full
statistics. The codec lives in the sans-io `nestris-ngf` crate and is
bit-compatible with the NestrisChamps `BinaryFrame` encoding and the
NestrisLTM importer (`ngf_import_service.py`) — recordings produced here
import directly into the NestrisLTM database.

## File format

An NGF file is a plain concatenation of fixed-size frames (optionally
gzip-compressed as a whole; readers sniff the `1f 8b` magic rather than
trusting the extension). The frame version lives in the top 3 bits of the
first byte and determines the frame size:

| Version | Frame size | Written by nestris-core |
|---|---|---|
| 1 | 71 bytes | read only |
| 2 | 72 bytes | read only |
| 3 | 73 bytes | **read + write** |

### Version-3 frame layout (all fields big-endian bit-packed)

| Offset | Bits | Field |
|---|---|---|
| 0 | 3 + 2 + 3 | `version` (3), `game_type` (0 minimal / 1 classic / 2 DAS trainer), `player_num` |
| 1–2 | 16 | `gameid` (session-monotonic per recording session) |
| 3–6 | 28 + 4 | `ctime` in **milliseconds since game start**, then the high nibble of `lines` |
| 7 | 8 | `lines` low byte (12 bits total) |
| 8 | 8 | `level` |
| 9–11 | 24 | `score` |
| 12 | 5 + 3 | `instant_das`, `preview` piece |
| 13 | 5 + 3 | `cur_piece_das`, `cur_piece` |
| 14–22 | 7 × 10 + 2 pad | piece counts in order **T J Z O S L I**, MSB-first |
| 23–72 | 200 × 2 | playfield: 4 cells per byte, MSB-first, row-major from the top-left |

Piece codes: `T=0 J=1 Z=2 O=3 S=4 L=5 I=6`, `7` = none/unknown.

Playfield cell values are the engine's stable color ids verbatim:
`0` empty, `1` white, `2` accent A, `3` accent B.

Unknown values use all-ones sentinels: score `0xFFFFFF`, lines `0xFFF`,
level `0xFF`, piece counts `0x3FF`, DAS `0x1F`, piece codes `0b111`.
DAS is not observable via OCR, so nestris-core always writes the DAS
sentinel.

## Recording lifecycle

The sans-io `GameRecorder` (`nestris-ngf/src/recorder.rs`) consumes engine
output frames:

- **Start** — on the engine's `new_game` boundary event, or (with
  *record partial* enabled) on the first in-game frame when capture begins
  mid-game. The browser engine records partial games by default.
- **Record** — every in-game/paused frame, plus the game-over confirmation
  window so the top-out stays visible in the replay. When the playfield is
  momentarily unreadable (clear animation), the last known grid is held —
  NGF has no "field unknown" sentinel.
- **Finish** — after a stable game over, on a menu exit, when the next
  `new_game` arrives, or at stream end. Games shorter than a minimum frame
  count are discarded as noise.

### Crash safety (native hosts)

While recording, every encoded frame is streamed to `<name>.ngf.part`
(raw, uncompressed, flushed every ~2 s of play). On finish the recording is
written as `<name>.ngf.gz` and the `.part` file removed. A crash therefore
leaves a readable raw NGF behind — `.part` files open in the replay viewer
and the `nestris replay` command like any other recording.

Default output directory: `Documents\nestris-recordings`
(file names `YYYYMMDD-HHMMSS_gNNN.ngf.gz`). The browser downloads each
finished game as `nestris_<stamp>.ngf.gz` instead.

## Replay

`ReplayEngine` (`nestris-ngf/src/replay.rs`) maps recorded frames back onto
the engine's fused-state shape and re-derives **all statistics — base and
extended — through the same `StatsEngine` as live analysis**. Replays are
deterministic: the same file always produces the same output frames.
Forward playback is incremental; backward seeks re-run the pure integer
stats pipeline from the start of the game (fast enough for interactive
scrubbing).

- **Desktop GUI** — `Open replay…` (or drop an `.ngf`/`.ngf.gz`/`.part`
  onto the window). The transport bar works as with videos: pause, speed,
  frame stepping, and instant seeks.
- **Web GUI** — `Open replay…` or drag & drop; the replay driver paces
  snapshot requests through the engine worker.
- **CLI** — `nestris replay <file> [--jsonl out.jsonl] [--ws addr]
  [--speed 1.0]` re-emits the recording as schema-v4 frames, paced by the
  recorded timestamps (default: as fast as possible).

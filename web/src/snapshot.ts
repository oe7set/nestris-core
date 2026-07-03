// Binary frame-snapshot decoder — the mirror of the Rust encoder in
// crates/nestris-wasm/src/snapshot.rs. Bump/check SNAPSHOT_VERSION together.

import type { ExtendedStats, OutputFrame } from "./types";

export const SNAPSHOT_VERSION = 1;

const GAME_STATES = [
  "no_signal",
  "unknown",
  "title",
  "type_select",
  "level_select",
  "in_game",
  "paused",
  "game_over",
  "highscore_entry",
] as const;

const LOCK_STATES = [
  "UNLOCKED",
  "ACQUIRING",
  "LOCKED",
  "DRIFT",
  "LOST",
  "REPLAY",
] as const;

const PIECES = ["-", "I", "O", "T", "S", "Z", "J", "L"] as const;

const I32_MIN = -2147483648;
const I64_MIN = -9223372036854775808n;

/** A decoded snapshot: the frame plus host-level lock/recording info. */
export interface Snapshot {
  frame: OutputFrame;
  lockState: string;
  /** Source-space playfield corners (8 values) or empty. */
  lockQuad: Float64Array;
  recording: boolean;
}

class Reader {
  private view: DataView;
  private pos = 0;
  constructor(buf: ArrayBuffer) {
    this.view = new DataView(buf);
  }
  u8(): number {
    return this.view.getUint8(this.pos++);
  }
  u16(): number {
    const v = this.view.getUint16(this.pos, true);
    this.pos += 2;
    return v;
  }
  u32(): number {
    const v = this.view.getUint32(this.pos, true);
    this.pos += 4;
    return v;
  }
  i32(): number {
    const v = this.view.getInt32(this.pos, true);
    this.pos += 4;
    return v;
  }
  i64(): number {
    const v = this.view.getBigInt64(this.pos, true);
    this.pos += 8;
    return Number(v);
  }
  i64opt(): number | null {
    const v = this.view.getBigInt64(this.pos, true);
    this.pos += 8;
    return v === I64_MIN ? null : Number(v);
  }
  f32(): number {
    const v = this.view.getFloat32(this.pos, true);
    this.pos += 4;
    return v;
  }
  f64(): number {
    const v = this.view.getFloat64(this.pos, true);
    this.pos += 8;
    return v;
  }
  f64opt(): number | null {
    const v = this.f64();
    return Number.isNaN(v) ? null : v;
  }
  i32opt(): number | null {
    const v = this.i32();
    return v === I32_MIN ? null : v;
  }
  bytes(n: number): Uint8Array {
    const out = new Uint8Array(this.view.buffer, this.pos, n);
    this.pos += n;
    return out;
  }
}

function pieceOf(code: number): string | null {
  return code === 255 ? null : (PIECES[code] ?? null);
}

export function decodeSnapshot(buf: ArrayBuffer): Snapshot {
  const r = new Reader(buf);
  const version = r.u8();
  if (version !== SNAPSHOT_VERSION) {
    throw new Error(
      `snapshot version mismatch: got ${version}, expected ${SNAPSHOT_VERSION} — rebuild the wasm pkg`,
    );
  }
  const lockState = LOCK_STATES[r.u8()] ?? "UNLOCKED";
  const gameState = GAME_STATES[r.u8()] ?? "unknown";
  const flags = r.u8();
  const hasPlayfield = (flags & 1) !== 0;
  const hasExt = (flags & 2) !== 0;
  const recording = (flags & 4) !== 0;
  const hasQuad = (flags & 8) !== 0;
  const hasStatsMap = (flags & 16) !== 0;

  const seq = r.i64();
  const ts = r.f64();
  const score = r.i32opt();
  const lines = r.i32opt();
  const level = r.i32opt();
  const nextPiece = pieceOf(r.u8());
  const currentPiece = pieceOf(r.u8());
  let currentPiecePos: [number, number] | null = null;
  if (r.u8() === 1) {
    currentPiecePos = [r.u16(), r.u16()];
  }
  const nCells = r.u8();
  const cells: [number, number][] = [];
  for (let i = 0; i < nCells; i++) {
    cells.push([r.u8(), r.u8()]);
  }

  const confNames = [
    "score",
    "lines",
    "level",
    "next_piece",
    "playfield",
    "statistics",
    "current_piece",
    "geometry",
    "overall",
  ];
  const confidence: Record<string, number> = {};
  for (const name of confNames) {
    confidence[name] = r.f32();
  }

  let playfield: number[][] | null = null;
  if (hasPlayfield) {
    playfield = [];
    for (let row = 0; row < 20; row++) {
      playfield.push(Array.from(r.bytes(10)));
    }
  }

  const stats: OutputFrame["stats"] = {
    pps: r.f64opt(),
    tetris_rate: r.f64opt(),
    burn: r.i64(),
    drought: r.u32(),
    max_drought: r.u32(),
    clears: {
      single: r.u32(),
      double: r.u32(),
      triple: r.u32(),
      tetris: r.u32(),
    },
    score_per_min: r.f64opt(),
    pieces: r.i64(),
    active_seconds: r.f64opt(),
  };

  let statistics: Record<string, number | null> | null = null;
  if (hasStatsMap) {
    statistics = {};
    const n = r.u8();
    for (let i = 0; i < n; i++) {
      const piece = pieceOf(r.u8()) ?? "-";
      statistics[piece] = r.i32opt();
    }
  }

  let statsExt: ExtendedStats | null = null;
  if (hasExt) {
    const points = {
      drops: r.i64(),
      singles: r.i64(),
      doubles: r.i64(),
      triples: r.i64(),
      tetrises: r.i64(),
    };
    const efficiency = r.f64opt();
    const pace = r.i64opt();
    const iDrought = {
      current: r.u32(),
      last: r.u32(),
      max: r.u32(),
      count: r.u32(),
    };
    const board = {
      max_height: r.u8(),
      avg_height: r.f32(),
      holes: r.u16(),
      ...(() => {
        const bf = r.u8();
        return {
          tetris_ready: (bf & 1) !== 0,
          double_well: (bf & 2) !== 0,
          clean_slope: (bf & 4) !== 0,
        };
      })(),
    };
    const trt: [number, number][] = [];
    const nTrt = r.u16();
    for (let i = 0; i < nTrt; i++) {
      trt.push([r.u32(), r.f32()]);
    }
    const heights: [number, number, number][] = [];
    const nHeights = r.u16();
    for (let i = 0; i < nHeights; i++) {
      heights.push([r.f32(), r.u8(), r.u8()]);
    }
    const counts: number[] = [];
    for (let i = 0; i < 7; i++) counts.push(r.u32());
    const drought: number[] = [];
    for (let i = 0; i < 7; i++) drought.push(r.u32());
    const deviation = r.f64();
    statsExt = {
      points,
      efficiency,
      pace_score: pace,
      i_drought: iDrought,
      board,
      trt_trend: trt,
      height_timeline: heights,
      piece_dist: { counts, drought, deviation },
    };
  }

  const lockQuad = new Float64Array(hasQuad ? 8 : 0);
  if (hasQuad) {
    for (let i = 0; i < 8; i++) lockQuad[i] = r.f32();
  }

  let events: OutputFrame["events"] = [];
  const eventsLen = r.u16();
  if (eventsLen > 0) {
    const json = new TextDecoder().decode(r.bytes(eventsLen));
    events = JSON.parse(json) as OutputFrame["events"];
  }

  const frame: OutputFrame = {
    schema_version: 4,
    seq,
    ts,
    region: "NTSC",
    game_state: gameState,
    fields: {
      score,
      lines,
      level,
      next_piece: nextPiece,
      current_piece: currentPiece,
      current_piece_pos: currentPiecePos,
      current_piece_cells: cells.length ? cells : null,
      playfield,
      statistics,
    },
    stats,
    stats_ext: statsExt,
    confidence,
    events,
  };

  return { frame, lockState, lockQuad, recording };
}

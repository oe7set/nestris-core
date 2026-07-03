// Cross-language codec test: decode the binary snapshot produced by the
// Rust encoder (fixture regenerated via
// `cargo run -p nestris-wasm --example gen_snapshot_fixture -- web/src/snapshot.fixture.json`)
// and compare against the frame's JSON serialization.

import { describe, expect, it } from "vitest";

import fixture from "./snapshot.fixture.json";
import { decodeSnapshot } from "./snapshot";
import type { OutputFrame } from "./types";

function hexToBuffer(hex: string): ArrayBuffer {
  const bytes = new Uint8Array(hex.length / 2);
  for (let i = 0; i < bytes.length; i++) {
    bytes[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return bytes.buffer;
}

describe("snapshot decoder vs Rust encoder", () => {
  const expected = fixture.frame_json as unknown as OutputFrame;
  const decoded = decodeSnapshot(hexToBuffer(fixture.snapshot_hex));

  it("decodes lock/recording metadata", () => {
    expect(decoded.lockState).toBe(fixture.lock_state);
    expect(decoded.recording).toBe(fixture.recording);
    expect(Array.from(decoded.lockQuad).map((v) => Math.round(v * 100) / 100)).toEqual(
      fixture.lock_quad,
    );
  });

  it("decodes header and fields", () => {
    const f = decoded.frame;
    expect(f.seq).toBe(expected.seq);
    expect(f.ts).toBeCloseTo(expected.ts, 9);
    expect(f.game_state).toBe(expected.game_state);
    expect(f.fields.score).toBe(expected.fields.score);
    expect(f.fields.lines).toBe(expected.fields.lines);
    expect(f.fields.level).toBe(expected.fields.level);
    expect(f.fields.next_piece).toBe(expected.fields.next_piece);
    expect(f.fields.current_piece).toBe(expected.fields.current_piece);
    expect(f.fields.current_piece_pos).toEqual(expected.fields.current_piece_pos);
    expect(f.fields.current_piece_cells).toEqual(expected.fields.current_piece_cells);
    expect(f.fields.playfield).toEqual(expected.fields.playfield);
    expect(f.fields.statistics).toEqual(expected.fields.statistics);
  });

  it("decodes confidences (f32 precision)", () => {
    for (const [key, value] of Object.entries(expected.confidence)) {
      expect(decoded.frame.confidence[key]).toBeCloseTo(value as number, 5);
    }
  });

  it("decodes base stats", () => {
    const s = decoded.frame.stats;
    const e = expected.stats;
    expect(s.pps).toBeCloseTo(e.pps!, 9);
    expect(s.tetris_rate).toBeCloseTo(e.tetris_rate!, 9);
    expect(s.burn).toBe(e.burn);
    expect(s.drought).toBe(e.drought);
    expect(s.max_drought).toBe(e.max_drought);
    expect(s.clears).toEqual(e.clears);
    expect(s.score_per_min).toBeNull();
    expect(s.pieces).toBe(e.pieces);
    expect(s.active_seconds).toBeCloseTo(e.active_seconds!, 9);
  });

  it("decodes extended stats", () => {
    const x = decoded.frame.stats_ext!;
    const e = expected.stats_ext!;
    expect(x.points).toEqual(e.points);
    expect(x.efficiency).toBeCloseTo(e.efficiency!, 9);
    expect(x.pace_score).toBe(e.pace_score);
    expect(x.i_drought).toEqual(e.i_drought);
    expect(x.board.max_height).toBe(e.board.max_height);
    expect(x.board.avg_height).toBeCloseTo(e.board.avg_height, 5);
    expect(x.board.holes).toBe(e.board.holes);
    expect(x.board.tetris_ready).toBe(e.board.tetris_ready);
    expect(x.board.double_well).toBe(e.board.double_well);
    expect(x.board.clean_slope).toBe(e.board.clean_slope);
    expect(x.trt_trend.length).toBe(e.trt_trend.length);
    for (let i = 0; i < e.trt_trend.length; i++) {
      expect(x.trt_trend[i][0]).toBe(e.trt_trend[i][0]);
      expect(x.trt_trend[i][1]).toBeCloseTo(e.trt_trend[i][1], 5);
    }
    expect(x.height_timeline.length).toBe(e.height_timeline.length);
    for (let i = 0; i < e.height_timeline.length; i++) {
      expect(x.height_timeline[i][0]).toBeCloseTo(e.height_timeline[i][0], 5);
      expect(x.height_timeline[i][1]).toBe(e.height_timeline[i][1]);
      expect(x.height_timeline[i][2]).toBe(e.height_timeline[i][2]);
    }
    expect(x.piece_dist.counts).toEqual(e.piece_dist.counts);
    expect(x.piece_dist.drought).toEqual(e.piece_dist.drought);
    expect(x.piece_dist.deviation).toBeCloseTo(e.piece_dist.deviation, 9);
  });

  it("decodes events", () => {
    expect(decoded.frame.events).toEqual(expected.events);
  });
});

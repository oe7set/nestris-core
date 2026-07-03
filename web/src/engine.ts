// Thin wrapper around the wasm Engine: one RGBA copy per frame.

import init, { Engine } from "nestris-wasm";
import type { OutputFrame } from "./types";

export class NestrisEngine {
  private engine!: Engine;
  private memory!: WebAssembly.Memory;

  async load(): Promise<void> {
    const wasm = await init();
    this.memory = wasm.memory;
    this.engine = new Engine("");
  }

  /** Process one RGBA frame; returns the parsed OutputFrame. */
  process(data: Uint8ClampedArray, width: number, height: number, ts: number): OutputFrame {
    const ptr = this.engine.frame_ptr(width, height);
    // The memory view must be rebuilt after frame_ptr: the buffer may have
    // grown (and detached previous views).
    new Uint8Array(this.memory.buffer, ptr, width * height * 4).set(data);
    return JSON.parse(this.engine.process(ts)) as OutputFrame;
  }

  lockState(): string {
    return this.engine.lock_state();
  }

  /** Source-space corners of the canonical raster (8 values), or empty. */
  lockQuad(): Float64Array {
    return this.engine.lock_quad();
  }

  /** Last canonical frame as RGBA (256*240*4), or empty. */
  canonicalRgba(): Uint8Array {
    return this.engine.canonical_rgba();
  }

  wantsBackgroundSolve(): boolean {
    return this.engine.wants_background_solve();
  }

  offerSolution(h: number[], confidence: number): void {
    this.engine.offer_solution(new Float64Array(h), confidence);
  }

  markDiscontinuity(): void {
    this.engine.mark_discontinuity();
  }

  resetLock(): void {
    this.engine.reset_lock();
  }
}

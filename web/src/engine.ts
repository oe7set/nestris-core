// Thin wrapper around the wasm Engine: one RGBA copy per frame.

import init, { Engine } from "nestris-wasm";
import type { OutputFrame } from "./types";

export class NestrisEngine {
  private engine!: Engine;
  private memory!: WebAssembly.Memory;

  async load(configJson = ""): Promise<void> {
    const wasm = await init();
    this.memory = wasm.memory;
    this.engine = new Engine(configJson);
  }

  /** Rebuild the engine with a new configuration (lock re-acquires). */
  setConfig(configJson: string): void {
    this.engine.free();
    this.engine = new Engine(configJson);
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

  /** Current background-solve pacing (ms); fast while tracking urgency. */
  solveIntervalMs(): number {
    return this.engine.solve_interval_ms();
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

  /** Toggle per-game NGF recording (enabled by default). */
  setRecording(enabled: boolean): void {
    this.engine.set_recording(enabled);
  }

  hasFinishedGame(): boolean {
    return this.engine.has_finished_game();
  }

  /** Oldest finished recording as gzipped .ngf.gz bytes (empty when none). */
  takeFinishedGame(): Uint8Array {
    return this.engine.take_finished_game();
  }
}

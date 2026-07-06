// Recalibration Web Worker: its own wasm instance runs estimate_geometry on
// posted frame snapshots (newest-wins) and returns {h, confidence}.
// h is null for a failed solve (background acquisition resets its streak on
// those); hard errors post nothing.

import init, { Solver } from "nestris-wasm";

interface SolveRequest {
  data?: ArrayBuffer;
  width?: number;
  height?: number;
  seed?: number;
  /** Engine config JSON; applies the solve options (downscale width). */
  config?: string;
}

let solver: Solver | null = null;
let memory: WebAssembly.Memory | null = null;
let pending: SolveRequest | null = null;
let configJson = "";
let busy = false;

async function ensureInit(): Promise<void> {
  if (!solver) {
    const wasm = await init();
    memory = wasm.memory;
    solver = new Solver();
    solver.set_config(configJson);
  }
}

async function drain(): Promise<void> {
  if (busy) return;
  busy = true;
  try {
    await ensureInit();
    while (pending) {
      const req = pending;
      pending = null;
      const ptr = solver!.frame_ptr(req.width!, req.height!);
      new Uint8Array(memory!.buffer, ptr, req.width! * req.height! * 4).set(
        new Uint8Array(req.data!),
      );
      // seed arrives as ts*1000 and may be fractional; BigInt() throws on
      // non-integers.
      const result = solver!.solve(BigInt(Math.round(req.seed!)));
      if (result !== "null") {
        const parsed = JSON.parse(result) as { h: number[] | null; confidence: number };
        self.postMessage(parsed);
      }
    }
  } finally {
    busy = false;
  }
}

self.onmessage = (ev: MessageEvent<SolveRequest>) => {
  if (ev.data.config !== undefined) {
    configJson = ev.data.config;
    solver?.set_config(configJson);
    return;
  }
  pending = ev.data; // newest-wins
  void drain();
};

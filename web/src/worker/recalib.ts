// Recalibration Web Worker: its own wasm instance runs estimate_geometry on
// posted frame snapshots (newest-wins) and returns {h, confidence}.

import init, { Solver } from "nestris-wasm";

interface SolveRequest {
  data: ArrayBuffer;
  width: number;
  height: number;
  seed: number;
}

let solver: Solver | null = null;
let memory: WebAssembly.Memory | null = null;
let pending: SolveRequest | null = null;
let busy = false;

async function ensureInit(): Promise<void> {
  if (!solver) {
    const wasm = await init();
    memory = wasm.memory;
    solver = new Solver();
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
      const ptr = solver!.frame_ptr(req.width, req.height);
      new Uint8Array(memory!.buffer, ptr, req.width * req.height * 4).set(
        new Uint8Array(req.data),
      );
      const result = solver!.solve(BigInt(req.seed));
      if (result !== "null") {
        const parsed = JSON.parse(result) as { h: number[]; confidence: number };
        self.postMessage(parsed);
      }
    }
  } finally {
    busy = false;
  }
}

self.onmessage = (ev: MessageEvent<SolveRequest>) => {
  pending = ev.data; // newest-wins
  void drain();
};

// The engine Web Worker: owns the wasm Engine (and the nested recalibration
// worker), so the full per-frame CV pipeline runs off the main thread.
//
// Protocol (main -> worker):
//   {t:"init", config}                load wasm, build the engine
//   {t:"frame", frame|bitmap, ts}     process one video frame (transferred;
//                                     newest-wins back-pressure is enforced
//                                     by the client). A VideoFrame is copied
//                                     straight into wasm memory (copyTo); an
//                                     ImageBitmap goes through the canvas.
//   {t:"setConfig", config}           rebuild the engine (lock re-acquires)
//   {t:"resetLock"} {t:"discontinuity"} {t:"setRecording", enabled}
//   {t:"replayLoad", bytes}           load an .ngf replay
//   {t:"replayFrame", index}          emit one replay frame as a result
//
// (worker -> main):
//   {t:"ready"}                       init done / engine idle again
//   {t:"result", snapshot, canonical, recordings, engineMs}
//   {t:"replayLoaded", frameCount, durationMs}
//   {t:"error", message}

import init, { Engine, Replay } from "nestris-wasm";

let engine: Engine | null = null;
let replay: Replay | null = null;
let memory: WebAssembly.Memory | null = null;
let recalib: Worker | null = null;
let canvas: OffscreenCanvas | null = null;
let ctx: OffscreenCanvasRenderingContext2D | null = null;
let lastSolvePost = 0;
/** VideoFrame.copyTo with RGBA conversion works here (until proven not to). */
let copyToUsable = true;

export interface EngineInMsg {
  t:
    | "init"
    | "frame"
    | "setConfig"
    | "resetLock"
    | "discontinuity"
    | "setRecording"
    | "replayLoad"
    | "replayFrame";
  config?: string;
  bitmap?: ImageBitmap;
  frame?: VideoFrame;
  ts?: number;
  enabled?: boolean;
  bytes?: ArrayBuffer;
  index?: number;
}

function post(msg: object, transfer: Transferable[] = []): void {
  (self as unknown as Worker).postMessage(msg, transfer);
}

function fail(err: unknown): void {
  post({ t: "error", message: String(err) });
}

/** Copy a region of wasm memory into a fresh transferable buffer. */
function copyOut(ptr: number, len: number): ArrayBuffer {
  const out = new Uint8Array(len);
  out.set(new Uint8Array(memory!.buffer, ptr, len));
  return out.buffer;
}

function startRecalib(config: string): void {
  recalib = new Worker(new URL("./recalib.ts", import.meta.url), {
    type: "module",
  });
  recalib.onmessage = (ev: MessageEvent<{ h: number[] | null; confidence: number }>) => {
    // h === null reports a failed solve; background acquisition needs those
    // to reset its adoption streak (maintenance mode ignores them).
    const h = ev.data.h ? new Float64Array(ev.data.h) : new Float64Array(0);
    engine?.offer_solution(h, ev.data.confidence);
  };
  recalib.postMessage({ config });
}

/** Copy a VideoFrame straight into wasm memory; false = use the canvas
 * fallback (unsupported conversion, unexpected layout, or a prior failure). */
async function videoFrameToWasm(
  frame: VideoFrame,
  w: number,
  h: number,
  view: Uint8Array,
): Promise<boolean> {
  if (!copyToUsable) return false;
  try {
    const layout = await frame.copyTo(view, { format: "RGBA" });
    if (layout.length === 1 && layout[0].offset === 0 && layout[0].stride === w * 4) {
      return true;
    }
  } catch {
    /* e.g. Firefox/Safari without copyTo format conversion */
  }
  copyToUsable = false;
  return false;
}

async function processFrame(source: ImageBitmap | VideoFrame, ts: number): Promise<void> {
  const vf =
    typeof VideoFrame === "function" && source instanceof VideoFrame ? source : null;
  if (!engine || !memory) {
    source.close();
    post({ t: "ready" });
    return;
  }
  const started = performance.now();
  const w = vf ? vf.displayWidth : (source as ImageBitmap).width;
  const h = vf ? vf.displayHeight : (source as ImageBitmap).height;

  const framePtr = engine.frame_ptr(w, h);
  // Views must be rebuilt after frame_ptr: the buffer may have grown.
  const view = new Uint8Array(memory.buffer, framePtr, w * h * 4);
  let direct = false;
  if (vf) {
    const engineBefore = engine;
    direct = await videoFrameToWasm(vf, w, h, view);
    if (engine !== engineBefore) {
      // setConfig rebuilt the engine while copyTo was in flight; the frame
      // buffer we wrote belongs to the old instance — drop this frame.
      source.close();
      post({ t: "ready" });
      return;
    }
  }
  if (!direct) {
    // Canvas fallback: rasterize + getImageData (ImageBitmap always; a
    // VideoFrame is a valid CanvasImageSource too).
    if (!canvas || canvas.width !== w || canvas.height !== h) {
      canvas = new OffscreenCanvas(w, h);
      ctx = canvas.getContext("2d", { willReadFrequently: true })!;
    }
    ctx!.drawImage(source, 0, 0);
    view.set(ctx!.getImageData(0, 0, w, h).data);
  }
  source.close();
  const snapLen = engine.process_snapshot(ts);
  const snapshot = copyOut(engine.snapshot_ptr(), snapLen);

  const canonLen = engine.canonical_update();
  const canonical = canonLen > 0 ? copyOut(engine.canonical_ptr(), canonLen) : null;

  // Background geometry solves, paced by the engine's urgency hint. During
  // acquisition the hint is 0 (solve back-to-back); clamp posts to ~50 ms so
  // the per-post frame copy doesn't eat the engine worker's frame budget
  // (the recalib worker keeps only the newest request anyway).
  const now = performance.now();
  if (
    recalib &&
    engine.wants_background_solve() &&
    now - lastSolvePost >= Math.max(engine.solve_interval_ms(), 50)
  ) {
    lastSolvePost = now;
    // Re-read the frame from wasm memory (fresh view — the buffer object may
    // have been replaced if wasm memory grew during process_snapshot).
    const copy = new Uint8Array(memory.buffer, framePtr, w * h * 4).slice().buffer;
    recalib.postMessage({ data: copy, width: w, height: h, seed: ts * 1000 }, [copy]);
  }

  const recordings: ArrayBuffer[] = [];
  while (engine.has_finished_game()) {
    recordings.push((engine.take_finished_game() as Uint8Array).buffer as ArrayBuffer);
  }

  const transfers: Transferable[] = [snapshot, ...recordings];
  if (canonical) transfers.push(canonical);
  post(
    {
      t: "result",
      snapshot,
      canonical,
      recordings,
      engineMs: performance.now() - started,
    },
    transfers,
  );
}

self.onmessage = async (ev: MessageEvent<EngineInMsg>) => {
  const msg = ev.data;
  try {
    switch (msg.t) {
      case "init": {
        const wasm = await init();
        memory = wasm.memory;
        engine = new Engine(msg.config ?? "");
        startRecalib(msg.config ?? "");
        post({ t: "ready" });
        break;
      }
      case "frame":
        await processFrame(msg.frame ?? msg.bitmap!, msg.ts ?? 0);
        break;
      case "setConfig":
        engine?.free();
        engine = new Engine(msg.config ?? "");
        recalib?.postMessage({ config: msg.config ?? "" });
        post({ t: "ready" });
        break;
      case "resetLock":
        engine?.reset_lock();
        break;
      case "discontinuity":
        engine?.mark_discontinuity();
        break;
      case "setRecording":
        engine?.set_recording(msg.enabled ?? true);
        break;
      case "replayLoad": {
        replay?.free();
        replay = new Replay(new Uint8Array(msg.bytes!));
        post({
          t: "replayLoaded",
          frameCount: replay.frame_count(),
          durationMs: replay.duration_ms(),
        });
        break;
      }
      case "replayFrame": {
        if (!replay || !memory) break;
        const len = replay.snapshot_at(msg.index ?? 0);
        const snapshot = copyOut(replay.snapshot_ptr(), len);
        post(
          { t: "result", snapshot, canonical: null, recordings: [], engineMs: 0 },
          [snapshot],
        );
        break;
      }
    }
  } catch (err) {
    fail(err);
  }
};

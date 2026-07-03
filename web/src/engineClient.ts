// Main-thread handle to the engine Web Worker: newest-wins back-pressure,
// snapshot decoding, and typed callbacks. The heavy work (canvas readback,
// CV, recording) all happens inside the worker.

import { type Snapshot, decodeSnapshot } from "./snapshot";

export interface FrameResult {
  snapshot: Snapshot;
  /** Canonical 256x240 RGBA bytes, when locked. */
  canonical: Uint8Array | null;
  /** Finished game recordings (gzipped .ngf.gz), if any ended this frame. */
  recordings: Uint8Array[];
  /** Worker-side processing time for this frame (ms). */
  engineMs: number;
}

interface WorkerOutMsg {
  t: "ready" | "result" | "replayLoaded" | "error";
  snapshot?: ArrayBuffer;
  canonical?: ArrayBuffer | null;
  recordings?: ArrayBuffer[];
  engineMs?: number;
  frameCount?: number;
  durationMs?: number;
  message?: string;
}

export class EngineClient {
  private worker: Worker;
  private busy = false;
  private initResolve: (() => void) | null = null;
  private initReject: ((err: Error) => void) | null = null;
  private replayResolve:
    | ((info: { frameCount: number; durationMs: number }) => void)
    | null = null;

  /** Frames skipped because the worker was still busy (newest-wins). */
  dropped = 0;

  onResult: ((result: FrameResult) => void) | null = null;
  onError: ((message: string) => void) | null = null;

  constructor() {
    this.worker = new Worker(new URL("./worker/engine.ts", import.meta.url), {
      type: "module",
    });
    this.worker.onmessage = (ev: MessageEvent<WorkerOutMsg>) => {
      const msg = ev.data;
      switch (msg.t) {
        case "ready":
          this.busy = false;
          this.initResolve?.();
          this.initResolve = null;
          this.initReject = null;
          break;
        case "result": {
          this.busy = false;
          if (this.onResult && msg.snapshot) {
            this.onResult({
              snapshot: decodeSnapshot(msg.snapshot),
              canonical: msg.canonical ? new Uint8Array(msg.canonical) : null,
              recordings: (msg.recordings ?? []).map((b) => new Uint8Array(b)),
              engineMs: msg.engineMs ?? 0,
            });
          }
          break;
        }
        case "replayLoaded":
          this.replayResolve?.({
            frameCount: msg.frameCount ?? 0,
            durationMs: msg.durationMs ?? 0,
          });
          this.replayResolve = null;
          break;
        case "error": {
          this.busy = false;
          const message = msg.message ?? "unknown engine error";
          if (this.initReject) {
            this.initReject(new Error(message));
            this.initResolve = null;
            this.initReject = null;
          } else {
            this.onError?.(message);
          }
          break;
        }
      }
    };
  }

  /** Load the wasm engine inside the worker. */
  init(configJson: string): Promise<void> {
    return new Promise((resolve, reject) => {
      this.initResolve = resolve;
      this.initReject = reject;
      this.worker.postMessage({ t: "init", config: configJson });
    });
  }

  /**
   * Capture the video element's current frame and send it for processing.
   * Returns false (and counts a drop) when the worker is still busy.
   */
  sendVideoFrame(video: HTMLVideoElement, ts: number): boolean {
    if (this.busy) {
      this.dropped++;
      return false;
    }
    this.busy = true;
    createImageBitmap(video).then(
      (bitmap) => this.worker.postMessage({ t: "frame", bitmap, ts }, [bitmap]),
      (err) => {
        this.busy = false;
        this.onError?.(`createImageBitmap failed: ${err}`);
      },
    );
    return true;
  }

  setConfig(configJson: string): void {
    this.busy = true; // resolved by the worker's "ready"
    this.worker.postMessage({ t: "setConfig", config: configJson });
  }

  resetLock(): void {
    this.worker.postMessage({ t: "resetLock" });
  }

  markDiscontinuity(): void {
    this.worker.postMessage({ t: "discontinuity" });
  }

  setRecording(enabled: boolean): void {
    this.worker.postMessage({ t: "setRecording", enabled });
  }

  /** Load an .ngf / .ngf.gz replay inside the worker. */
  loadReplay(bytes: ArrayBuffer): Promise<{ frameCount: number; durationMs: number }> {
    return new Promise((resolve) => {
      this.replayResolve = resolve;
      this.worker.postMessage({ t: "replayLoad", bytes }, [bytes]);
    });
  }

  /** Request one replay frame; arrives via onResult. */
  requestReplayFrame(index: number): boolean {
    if (this.busy) return false;
    this.busy = true;
    this.worker.postMessage({ t: "replayFrame", index });
    return true;
  }
}

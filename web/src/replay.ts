// NGF replay driver: replaces the <video> element as the frame clock.
//
// The wasm Replay (inside the engine worker) re-derives full output frames
// from the recording; this driver owns the playback position, paces frame
// requests via requestAnimationFrame, and exposes a TransportTarget so the
// shared transport bar controls it.

import type { EngineClient } from "./engineClient";
import type { TransportTarget } from "./transport";

export class ReplayDriver {
  private client: EngineClient;
  private frameCount: number;
  private durationS: number;
  private index = 0;
  private paused = false;
  private rate = 1;
  private loop = false;
  private clockAnchor = performance.now();
  private anchorIndex = 0;
  private raf = 0;
  private lastRequested = -1;

  constructor(client: EngineClient, frameCount: number, durationMs: number) {
    this.client = client;
    this.frameCount = Math.max(1, frameCount);
    this.durationS = Math.max(0.001, durationMs / 1000);
  }

  /** Average recorded frame interval (the recording is ~uniformly paced). */
  private frameDt(): number {
    return this.durationS / this.frameCount;
  }

  private indexToSeconds(index: number): number {
    return (index / this.frameCount) * this.durationS;
  }

  start(): void {
    this.rebase(this.index);
    const tick = () => {
      this.raf = requestAnimationFrame(tick);
      if (!this.paused) {
        const elapsed = ((performance.now() - this.clockAnchor) / 1000) * this.rate;
        let target = this.anchorIndex + Math.floor(elapsed / this.frameDt());
        if (target >= this.frameCount) {
          if (this.loop) {
            this.rebase(0);
            target = 0;
          } else {
            target = this.frameCount - 1;
            this.paused = true;
          }
        }
        this.index = target;
      }
      if (this.index !== this.lastRequested && this.client.requestReplayFrame(this.index)) {
        this.lastRequested = this.index;
      }
    };
    this.raf = requestAnimationFrame(tick);
  }

  stop(): void {
    cancelAnimationFrame(this.raf);
  }

  /** Re-anchor the wall clock at a given frame index. */
  private rebase(index: number): void {
    this.index = Math.min(Math.max(0, index), this.frameCount - 1);
    this.anchorIndex = this.index;
    this.clockAnchor = performance.now();
    this.lastRequested = -1; // force a refresh even while paused
  }

  transportTarget(): TransportTarget {
    return {
      duration: () => this.durationS,
      position: () => this.indexToSeconds(this.index),
      seek: (s) => this.rebase(Math.round((s / this.durationS) * this.frameCount)),
      setPaused: (p) => {
        this.paused = p;
        if (!p) this.rebase(this.index);
      },
      isPaused: () => this.paused,
      setRate: (r) => {
        this.rate = r;
        this.rebase(this.index);
      },
      step: (dir) => {
        this.paused = true;
        this.rebase(this.index + dir);
      },
      setLoop: (l) => (this.loop = l),
    };
  }
}

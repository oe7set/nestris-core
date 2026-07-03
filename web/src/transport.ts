// Transport bar: play/pause, frame stepping, speed, a custom seek bar with
// buffered ranges and a hover time bubble, and a loop toggle.
//
// The bar drives an abstract target so the same controls work for video
// files (HTMLVideoElement) and NGF replays (ReplayDriver).

export interface TransportTarget {
  /** Total duration in seconds. */
  duration(): number;
  /** Current position in seconds. */
  position(): number;
  seek(seconds: number): void;
  setPaused(paused: boolean): void;
  isPaused(): boolean;
  setRate(rate: number): void;
  /** Step one frame forward (+1) or back (-1); implies pausing. */
  step(direction: 1 | -1): void;
  setLoop(loop: boolean): void;
  /** Buffered ranges in seconds (video only). */
  buffered?(): [number, number][];
}

function fmt(t: number): string {
  const m = Math.floor(t / 60);
  const s = (t % 60).toFixed(1).padStart(4, "0");
  return `${m}:${s}`;
}

export class TransportBar {
  private root = document.getElementById("transport")!;
  private playBtn = document.getElementById("tr-play") as HTMLButtonElement;
  private backBtn = document.getElementById("tr-back") as HTMLButtonElement;
  private stepBtn = document.getElementById("tr-step") as HTMLButtonElement;
  private speedSel = document.getElementById("tr-speed") as HTMLSelectElement;
  private loopBtn = document.getElementById("tr-loop") as HTMLButtonElement;
  private bar = document.getElementById("seek-bar")!;
  private played = document.getElementById("seek-played")!;
  private bufferedEl = document.getElementById("seek-buffered")!;
  private bubble = document.getElementById("seek-bubble")!;
  private timeEl = document.getElementById("transport-time")!;
  private target: TransportTarget | null = null;
  private loop = false;
  private scrubbing = false;

  constructor() {
    this.playBtn.addEventListener("click", () => this.togglePause());
    this.backBtn.addEventListener("click", () => this.target?.step(-1));
    this.stepBtn.addEventListener("click", () => this.target?.step(1));
    this.speedSel.addEventListener("change", () => {
      this.target?.setRate(Number(this.speedSel.value));
    });
    this.loopBtn.addEventListener("click", () => {
      this.loop = !this.loop;
      this.loopBtn.classList.toggle("active", this.loop);
      this.target?.setLoop(this.loop);
    });

    const timeAt = (ev: MouseEvent): number => {
      const rect = this.bar.getBoundingClientRect();
      const frac = Math.min(1, Math.max(0, (ev.clientX - rect.left) / rect.width));
      return frac * (this.target?.duration() ?? 0);
    };
    this.bar.addEventListener("mousemove", (ev) => {
      if (!this.target) return;
      const rect = this.bar.getBoundingClientRect();
      this.bubble.style.display = "block";
      this.bubble.style.left = `${ev.clientX - rect.left}px`;
      this.bubble.textContent = fmt(timeAt(ev));
      if (this.scrubbing) this.target.seek(timeAt(ev));
    });
    this.bar.addEventListener("mouseleave", () => {
      this.bubble.style.display = "none";
      this.scrubbing = false;
    });
    this.bar.addEventListener("mousedown", (ev) => {
      this.scrubbing = true;
      this.target?.seek(timeAt(ev));
    });
    window.addEventListener("mouseup", () => (this.scrubbing = false));
  }

  attach(target: TransportTarget): void {
    this.target = target;
    this.root.style.display = "flex";
    target.setLoop(this.loop);
    target.setRate(Number(this.speedSel.value));
  }

  detach(): void {
    this.target = null;
    this.root.style.display = "none";
  }

  togglePause(): void {
    const t = this.target;
    if (!t) return;
    t.setPaused(!t.isPaused());
  }

  stepFrame(direction: 1 | -1): void {
    this.target?.step(direction);
  }

  cycleSpeed(up: boolean): void {
    const idx = this.speedSel.selectedIndex + (up ? 1 : -1);
    if (idx >= 0 && idx < this.speedSel.options.length) {
      this.speedSel.selectedIndex = idx;
      this.speedSel.dispatchEvent(new Event("change"));
    }
  }

  /** Refresh the bar; call once per rendered frame. */
  update(): void {
    const t = this.target;
    if (!t) return;
    const duration = Math.max(t.duration(), 0.001);
    const pos = t.position();
    this.played.style.width = `${(100 * pos) / duration}%`;
    this.timeEl.textContent = `${fmt(pos)} / ${fmt(duration)}`;
    this.playBtn.textContent = t.isPaused() ? "▶" : "⏸";
    const ranges = t.buffered?.() ?? [];
    const end = ranges.length ? ranges[ranges.length - 1][1] : 0;
    this.bufferedEl.style.width = `${(100 * end) / duration}%`;
  }
}

/** Transport target for a `<video>` element (file playback). */
export function videoTarget(
  video: HTMLVideoElement,
  fpsEstimate: () => number,
): TransportTarget {
  return {
    duration: () => (Number.isFinite(video.duration) ? video.duration : 0),
    position: () => video.currentTime,
    seek: (s) => {
      video.currentTime = Math.min(Math.max(0, s), video.duration || s);
    },
    setPaused: (p) => {
      if (p) video.pause();
      else void video.play();
    },
    isPaused: () => video.paused,
    setRate: (r) => (video.playbackRate = r),
    step: (dir) => {
      video.pause();
      const dt = 1 / Math.max(1, fpsEstimate());
      video.currentTime = Math.max(0, video.currentTime + dir * dt);
    },
    setLoop: (l) => (video.loop = l),
    buffered: () => {
      const out: [number, number][] = [];
      for (let i = 0; i < video.buffered.length; i++) {
        out.push([video.buffered.start(i), video.buffered.end(i)]);
      }
      return out;
    },
  };
}

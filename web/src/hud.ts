// Performance HUD: worker-side engine time percentiles and dropped frames.

export class PerfHud {
  private el = document.getElementById("hud")!;
  private samples: number[] = [];

  push(engineMs: number): void {
    this.samples.push(engineMs);
    if (this.samples.length > 240) {
      this.samples.splice(0, this.samples.length - 240);
    }
  }

  render(dropped: number): void {
    if (this.samples.length < 10) {
      this.el.textContent = "";
      return;
    }
    const sorted = [...this.samples].sort((a, b) => a - b);
    const pick = (q: number) => sorted[Math.floor((sorted.length - 1) * q)];
    this.el.textContent =
      `engine ${pick(0.5).toFixed(1)}ms p95 ${pick(0.95).toFixed(1)}ms` +
      (dropped > 0 ? ` · ${dropped} dropped` : "");
  }
}

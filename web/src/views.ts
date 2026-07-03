// Canvas + DOM views: source overlay, canonical preview, tracked playfield,
// dashboard tiles, events panel.

import { cellColor, css, PIECE_COLORS } from "./palette";
import type { OutputFrame } from "./types";

export class Views {
  private rawCanvas = document.getElementById("raw-canvas") as HTMLCanvasElement;
  private rawCtx = this.rawCanvas.getContext("2d")!;
  private canonCanvas = document.getElementById("canon-canvas") as HTMLCanvasElement;
  private canonCtx = this.canonCanvas.getContext("2d")!;
  private pfCanvas = document.getElementById("playfield-canvas") as HTMLCanvasElement;
  private pfCtx = this.pfCanvas.getContext("2d")!;
  private eventsList = document.getElementById("events") as HTMLUListElement;
  private lockLabel = document.getElementById("lock-state")!;
  private fpsLabel = document.getElementById("fps")!;
  private banner = document.getElementById("state-banner")!;
  private dropHint = document.getElementById("drop-hint")!;
  private canonImage = new ImageData(256, 240);
  private eventCount = 0;

  drawSource(video: HTMLVideoElement, quad: Float64Array): void {
    const w = video.videoWidth;
    const h = video.videoHeight;
    if (this.rawCanvas.width !== w || this.rawCanvas.height !== h) {
      this.rawCanvas.width = w;
      this.rawCanvas.height = h;
    }
    this.dropHint.style.display = "none";
    this.rawCtx.drawImage(video, 0, 0, w, h);
    if (quad.length === 8) {
      this.rawCtx.strokeStyle = "#3cbcfc";
      this.rawCtx.lineWidth = Math.max(2, w / 400);
      this.rawCtx.beginPath();
      this.rawCtx.moveTo(quad[0], quad[1]);
      for (let i = 2; i < 8; i += 2) this.rawCtx.lineTo(quad[i], quad[i + 1]);
      this.rawCtx.closePath();
      this.rawCtx.stroke();
    }
  }

  drawCanonical(rgba: Uint8Array): void {
    if (rgba.length !== 256 * 240 * 4) {
      this.canonCtx.fillStyle = "#000";
      this.canonCtx.fillRect(0, 0, 256, 240);
      return;
    }
    this.canonImage.data.set(rgba);
    this.canonCtx.putImageData(this.canonImage, 0, 0);
  }

  drawPlayfield(frame: OutputFrame): void {
    const ctx = this.pfCtx;
    const cw = this.pfCanvas.width / 10;
    const ch = this.pfCanvas.height / 20;
    ctx.fillStyle = "#000";
    ctx.fillRect(0, 0, this.pfCanvas.width, this.pfCanvas.height);
    const grid = frame.fields.playfield;
    const level = frame.fields.level;
    const pieceCells = new Set(
      (frame.fields.current_piece_cells ?? []).map(([r, c]) => r * 10 + c),
    );
    if (grid) {
      for (let r = 0; r < 20; r++) {
        for (let c = 0; c < 10; c++) {
          const id = grid[r][c];
          if (id === 0 || pieceCells.has(r * 10 + c)) continue;
          ctx.fillStyle = css(cellColor(level, id));
          ctx.fillRect(c * cw + 1, r * ch + 1, cw - 2, ch - 2);
        }
      }
    }
    // Falling piece in its guideline color, on top.
    const piece = frame.fields.current_piece;
    if (piece && PIECE_COLORS[piece] && frame.fields.current_piece_cells) {
      ctx.fillStyle = css(PIECE_COLORS[piece]);
      for (const [r, c] of frame.fields.current_piece_cells) {
        ctx.fillRect(c * cw + 1, r * ch + 1, cw - 2, ch - 2);
      }
    }
  }

  updateDashboard(frame: OutputFrame): void {
    const set = (id: string, v: string) => {
      document.getElementById(id)!.textContent = v;
    };
    const num = (v: number | null, digits = 0) =>
      v === null ? "—" : v.toFixed(digits);
    set("v-score", frame.fields.score?.toLocaleString() ?? "—");
    set("v-lines", num(frame.fields.lines));
    set("v-level", num(frame.fields.level));
    set("v-next", frame.fields.next_piece ?? "—");
    set("v-pieces", String(frame.stats.pieces));
    set(
      "v-trate",
      frame.stats.tetris_rate === null
        ? "—"
        : `${(frame.stats.tetris_rate * 100).toFixed(0)}%`,
    );
    set("v-pps", num(frame.stats.pps, 2));
    set("v-burn", String(frame.stats.burn));
    set("v-drought", String(frame.stats.drought));
    this.banner.textContent = frame.game_state.replace("_", " ");
    this.banner.className = "";
    this.banner.id = "state-banner";
    this.banner.classList.add(frame.game_state);
  }

  updateLock(state: string, fps: number): void {
    this.lockLabel.textContent = state;
    this.lockLabel.className = state === "LOCKED" ? "locked" : state === "DRIFT" ? "drift" : "";
    this.lockLabel.id = "lock-state";
    this.fpsLabel.textContent = `${fps.toFixed(0)} fps`;
  }

  pushEvents(frame: OutputFrame): void {
    for (const ev of frame.events) {
      const li = document.createElement("li");
      const mm = Math.floor(ev.ts / 60);
      const ss = (ev.ts % 60).toFixed(0).padStart(2, "0");
      const gold = ev.reason === "clear_tetris";
      li.className = gold ? "gold" : ev.severity;
      li.textContent = `${mm}:${ss} ${ev.field} ${ev.reason}${
        ev.new !== null && ev.new !== undefined ? ` → ${ev.new}` : ""
      }`;
      this.eventsList.prepend(li);
      if (++this.eventCount > 200) {
        this.eventsList.lastElementChild?.remove();
        this.eventCount--;
      }
    }
  }
}

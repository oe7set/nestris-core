// The NestrisChamps-style statistics section: stat tiles, LINES/POINTS
// breakdowns, TRT trend and HEIGHT & STATE canvas charts, per-piece
// distribution bars, and persistent (localStorage) high-score tables.

import type { ExtendedStats, OutputFrame } from "./types";

const PB_KEY = "nestris-pbs";
const PIECES = ["T", "J", "Z", "O", "S", "L", "I"];
const FLAG_TETRIS_READY = 1;
const FLAG_DOUBLE_WELL = 2;
const FLAG_CLEAN_SLOPE = 4;
const FLAG_IN_DROUGHT = 8;

interface PbRecord {
  date: string; // YYYY-MM-DD local
  score: number;
  lines: number;
  level: number | null;
  trt: number | null;
}

function loadPbs(): PbRecord[] {
  try {
    return JSON.parse(localStorage.getItem(PB_KEY) ?? "[]") as PbRecord[];
  } catch {
    return [];
  }
}

function today(): string {
  const d = new Date();
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(
    d.getDate(),
  ).padStart(2, "0")}`;
}

export class StatsView {
  private tiles = document.getElementById("stats-tiles")!;
  private linesTable = document.getElementById("st-lines") as HTMLTableElement;
  private linesTitle = document.getElementById("st-lines-title")!;
  private pointsTable = document.getElementById("st-points") as HTMLTableElement;
  private pointsTitle = document.getElementById("st-points-title")!;
  private piecesEl = document.getElementById("st-pieces")!;
  private piecesTitle = document.getElementById("st-pieces-title")!;
  private trtCanvas = document.getElementById("st-trt") as HTMLCanvasElement;
  private heightCanvas = document.getElementById("st-height") as HTMLCanvasElement;
  private pbToday = document.getElementById("st-pb-today") as HTMLTableElement;
  private pbOverall = document.getElementById("st-pb-overall") as HTMLTableElement;
  private pbs: PbRecord[] = loadPbs();
  private prevState: string | null = null;
  private lastInGame: OutputFrame | null = null;

  constructor() {
    this.renderPbs();
  }

  update(frame: OutputFrame): void {
    this.trackGameEnd(frame);
    const ext = frame.stats_ext;
    if (!ext) return;
    this.renderTiles(frame, ext);
    this.renderLines(frame);
    this.renderPoints(frame, ext);
    this.renderPieces(ext);
    this.renderTrt(ext);
    this.renderHeight(ext);
  }

  /** Record a finished game into the PB store on in-game -> game-over. */
  private trackGameEnd(frame: OutputFrame): void {
    if (frame.game_state === "in_game") {
      this.lastInGame = frame;
    }
    const wasPlaying = this.prevState === "in_game" || this.prevState === "paused";
    if (wasPlaying && frame.game_state === "game_over" && this.lastInGame) {
      const last = this.lastInGame;
      this.lastInGame = null;
      if (last.fields.score !== null) {
        this.pbs.push({
          date: today(),
          score: last.fields.score,
          lines: last.fields.lines ?? 0,
          level: last.fields.level,
          trt: last.stats.tetris_rate,
        });
        if (this.pbs.length > 2000) this.pbs.splice(0, this.pbs.length - 2000);
        localStorage.setItem(PB_KEY, JSON.stringify(this.pbs));
        this.renderPbs();
      }
    }
    this.prevState = frame.game_state;
  }

  private renderTiles(frame: OutputFrame, ext: ExtendedStats): void {
    const tiles: [string, string, string?][] = [
      ["Score", frame.fields.score?.toLocaleString() ?? "—"],
      ["Pace", ext.pace_score?.toLocaleString() ?? "—", "var(--accent)"],
      ["Lines", String(frame.fields.lines ?? "—")],
      ["Level", String(frame.fields.level ?? "—")],
      ["Eff", ext.efficiency === null ? "—" : ext.efficiency.toFixed(0), "var(--accent)"],
      ["Brn", String(frame.stats.burn)],
      [
        "TRT",
        frame.stats.tetris_rate === null
          ? "—"
          : `${(frame.stats.tetris_rate * 100).toFixed(0)}%`,
        "var(--gold)",
      ],
      [
        "I-drt",
        `${ext.i_drought.current}/${ext.i_drought.last}/${ext.i_drought.max}`,
        ext.i_drought.current >= 13 ? "var(--bad)" : undefined,
      ],
    ];
    this.tiles.innerHTML = tiles
      .map(
        ([label, value, color]) =>
          `<div class="tile"><div class="label">${label}</div>` +
          `<div class="value"${color ? ` style="color:${color}"` : ""}>${value}</div></div>`,
      )
      .join("");
  }

  private renderLines(frame: OutputFrame): void {
    const c = frame.stats.clears;
    const rows: [string, number, number][] = [
      ["Singles", c.single, c.single],
      ["Doubles", c.double, c.double * 2],
      ["Triples", c.triple, c.triple * 3],
      ["Tetris", c.tetris, c.tetris * 4],
    ];
    const total = rows.reduce((acc, [, , l]) => acc + l, 0);
    this.linesTitle.textContent = `Lines ${String(total).padStart(3, "0")}`;
    this.linesTable.innerHTML = rows
      .map(
        ([label, count, contributed]) =>
          `<tr><td>${label}</td><td>${String(count).padStart(3, "0")}</td>` +
          `<td>${total ? Math.round((100 * contributed) / total) : 0}%</td></tr>`,
      )
      .join("");
  }

  private renderPoints(frame: OutputFrame, ext: ExtendedStats): void {
    const total = Math.max(1, frame.fields.score ?? 0);
    const rows: [string, number][] = [
      ["Drops", ext.points.drops],
      ["Singles", ext.points.singles],
      ["Doubles", ext.points.doubles],
      ["Triples", ext.points.triples],
      ["Tetris", ext.points.tetrises],
    ];
    this.pointsTitle.textContent = `Points ${frame.fields.score ?? 0}`;
    this.pointsTable.innerHTML = rows
      .map(
        ([label, points]) =>
          `<tr><td>${label}</td><td>${points.toLocaleString()}</td>` +
          `<td>${Math.round((100 * points) / total)}%</td></tr>`,
      )
      .join("");
  }

  private renderPieces(ext: ExtendedStats): void {
    const { counts, drought, deviation } = ext.piece_dist;
    const total = counts.reduce((a, b) => a + b, 0);
    const max = Math.max(1, ...counts);
    this.piecesTitle.textContent = `Pieces ${String(total).padStart(3, "0")} — dev ${(
      deviation * 100
    ).toFixed(1)}%`;
    this.piecesEl.innerHTML = PIECES.map((piece, i) => {
      const width = (100 * counts[i]) / max;
      return (
        `<div class="piece-row${piece === "I" ? " i-piece" : ""}">` +
        `<span>${piece} ${String(counts[i]).padStart(3, "0")}</span>` +
        `<div class="bar"><div style="width:${width}%"></div></div>` +
        `<span>drt ${String(drought[i]).padStart(2, "0")}</span></div>`
      );
    }).join("");
  }

  private chart(canvas: HTMLCanvasElement): CanvasRenderingContext2D {
    const ctx = canvas.getContext("2d")!;
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    ctx.strokeStyle = "#2a333d";
    ctx.lineWidth = 1;
    for (const frac of [0.25, 0.5, 0.75]) {
      ctx.beginPath();
      ctx.moveTo(0, canvas.height * frac);
      ctx.lineTo(canvas.width, canvas.height * frac);
      ctx.stroke();
    }
    return ctx;
  }

  private renderTrt(ext: ExtendedStats): void {
    const ctx = this.chart(this.trtCanvas);
    const samples = ext.trt_trend;
    if (samples.length < 2) return;
    const w = this.trtCanvas.width;
    const h = this.trtCanvas.height;
    const xMax = Math.max(1, samples[samples.length - 1][0]);
    ctx.strokeStyle = "#ffd700";
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    samples.forEach(([lines, rate], i) => {
      const x = (w * lines) / xMax;
      const y = h - h * Math.min(1, Math.max(0, rate));
      if (i === 0) ctx.moveTo(x, y);
      else ctx.lineTo(x, y);
    });
    ctx.stroke();
  }

  private renderHeight(ext: ExtendedStats): void {
    const ctx = this.chart(this.heightCanvas);
    const samples = ext.height_timeline;
    if (samples.length < 2) return;
    const w = this.heightCanvas.width;
    const h = this.heightCanvas.height;
    const t0 = samples[0][0];
    const t1 = Math.max(t0 + 1, samples[samples.length - 1][0]);
    const xOf = (ts: number) => (w * (ts - t0)) / (t1 - t0);

    // Flag strips at the bottom: ready (gold), drought (red), clean (blue).
    const stripH = 5;
    const stripTop = h - 3 * stripH;
    let prevX = 0;
    for (const [ts, , flags] of samples) {
      const x = xOf(ts);
      const strips: [number, number, string][] = [
        [0, FLAG_TETRIS_READY, "#ffd700"],
        [1, FLAG_IN_DROUGHT, "#fc7460"],
        [2, FLAG_DOUBLE_WELL | FLAG_CLEAN_SLOPE, "#3cbcfc"],
      ];
      for (const [row, mask, color] of strips) {
        if (flags & mask) {
          ctx.fillStyle = color;
          ctx.fillRect(prevX, stripTop + row * stripH, x - prevX, stripH - 1);
        }
      }
      prevX = x;
    }

    // Height curve above the strips.
    const curveH = stripTop - 2;
    ctx.strokeStyle = "#e6e9ee";
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    samples.forEach(([ts, height], i) => {
      const x = xOf(ts);
      const y = curveH - curveH * Math.min(1, height / 20);
      if (i === 0) ctx.moveTo(x, y);
      else ctx.lineTo(x, y);
    });
    ctx.stroke();
  }

  private renderPbs(): void {
    const render = (table: HTMLTableElement, rows: PbRecord[]) => {
      const header =
        "<tr><td style='color:var(--dim)'>Score</td><td style='color:var(--dim)'>Lines</td>" +
        "<td style='color:var(--dim)'>Lvl</td><td style='color:var(--dim)'>TRT</td></tr>";
      table.innerHTML =
        header +
        (rows.length
          ? rows
              .map(
                (r) =>
                  `<tr><td>${r.score.toLocaleString()}</td><td>${r.lines}</td>` +
                  `<td>${r.level ?? "—"}</td>` +
                  `<td>${r.trt === null ? "—" : `${Math.round(r.trt * 100)}%`}</td></tr>`,
              )
              .join("")
          : "<tr><td colspan='4' style='color:var(--dim)'>no finished games yet</td></tr>");
    };
    const sorted = [...this.pbs].sort((a, b) => b.score - a.score);
    render(this.pbOverall, sorted.slice(0, 5));
    render(
      this.pbToday,
      sorted.filter((r) => r.date === today()).slice(0, 5),
    );
  }
}

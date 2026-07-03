// Entry point: capture / replay -> engine worker -> views.
//
// The full CV pipeline (canvas readback, recognition, recording) runs in a
// Web Worker (`worker/engine.ts`); this thread captures ImageBitmaps (or
// paces replay frames), decodes compact binary snapshots, and paints the
// UI — so the page stays responsive regardless of frame cost.
//
// UI handlers are attached BEFORE the wasm engine loads, so a failed load
// (missing pkg build, blocked .wasm) surfaces as a visible error instead of
// dead buttons.

import { CaptureSource } from "./capture";
import { EngineClient } from "./engineClient";
import { PerfHud } from "./hud";
import { ReplayDriver } from "./replay";
import { initSettingsUi, loadConfig } from "./settings";
import { StatsView } from "./statsview";
import { showToast } from "./toast";
import { TransportBar, videoTarget } from "./transport";
import { Views } from "./views";

function showHint(text: string, isError = false): void {
  const hint = document.getElementById("drop-hint")!;
  hint.style.display = "block";
  hint.textContent = text;
  hint.style.color = isError ? "#f83800" : "";
}

/** Save a finished .ngf.gz recording via a browser download. */
function downloadRecording(bytes: Uint8Array): void {
  const stamp = new Date()
    .toISOString()
    .replace(/[-:]/g, "")
    .replace("T", "-")
    .slice(0, 15);
  const blob = new Blob([bytes as BlobPart], { type: "application/gzip" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = `nestris_${stamp}.ngf.gz`;
  a.click();
  URL.revokeObjectURL(url);
}

function isReplayFile(name: string): boolean {
  const lower = name.toLowerCase();
  return lower.endsWith(".ngf") || lower.endsWith(".ngf.gz") || lower.endsWith(".ngf.part");
}

async function boot(): Promise<void> {
  const views = new Views();
  const statsView = new StatsView();
  const transport = new TransportBar();
  const hud = new PerfHud();
  const source = new CaptureSource();
  const client = new EngineClient();
  const recIndicator = document.getElementById("rec-indicator")!;
  const recordBtn = document.getElementById("btn-record") as HTMLButtonElement;

  let engineReady = false;
  let mode: "video" | "replay" | null = null;
  let replayDriver: ReplayDriver | null = null;
  let recordingEnabled = true;

  let frameCount = 0;
  let fps = 0;
  let fpsWindowStart = performance.now();
  let running = false;

  client.onResult = (result) => {
    const { frame } = result.snapshot;

    for (const rec of result.recordings) {
      downloadRecording(rec);
      showToast("success", "Game recording saved (.ngf.gz)");
    }
    recIndicator.style.display = result.snapshot.recording ? "inline" : "none";

    if (mode === "video") {
      views.drawSource(source.video, result.snapshot.lockQuad);
    }
    views.drawCanonical(result.canonical ?? new Uint8Array(0));
    views.drawPlayfield(frame);
    views.updateDashboard(frame);
    views.pushEvents(frame);
    statsView.update(frame);
    transport.update();

    frameCount++;
    hud.push(result.engineMs);
    const now = performance.now();
    if (now - fpsWindowStart >= 1000) {
      fps = (frameCount * 1000) / (now - fpsWindowStart);
      frameCount = 0;
      fpsWindowStart = now;
      hud.render(client.dropped);
    }
    views.updateLock(result.snapshot.lockState, fps);
  };
  client.onError = (message) => {
    showHint(`Engine error: ${message}`, true);
    showToast("error", message);
  };

  const processFrame = (): void => {
    if (!engineReady || mode !== "video" || !source.ready) return;
    const ts = source.video.currentTime || performance.now() / 1000;
    client.sendVideoFrame(source.video, ts);
  };

  const loop = (): void => {
    processFrame();
    scheduleNext();
  };
  const scheduleNext = (): void => {
    if (!running) return;
    const video = source.video as HTMLVideoElement & {
      requestVideoFrameCallback?: (cb: () => void) => void;
    };
    if (video.requestVideoFrameCallback && !video.paused) {
      video.requestVideoFrameCallback(loop);
    } else {
      requestAnimationFrame(loop);
    }
  };

  const stopReplay = (): void => {
    replayDriver?.stop();
    replayDriver = null;
  };

  const startVideo = (isFile: boolean): void => {
    if (!engineReady) {
      showHint("Engine not loaded yet — see the console for the error.", true);
      return;
    }
    stopReplay();
    mode = "video";
    client.markDiscontinuity();
    if (isFile) {
      transport.attach(videoTarget(source.video, () => Math.max(1, fps)));
    } else {
      transport.detach(); // live sources have no timeline
    }
    if (!running) {
      running = true;
      scheduleNext();
    }
  };

  const openReplay = async (file: File): Promise<void> => {
    if (!engineReady) {
      showHint("Engine not loaded yet — see the console for the error.", true);
      return;
    }
    try {
      const bytes = await file.arrayBuffer();
      stopReplay();
      const info = await client.loadReplay(bytes);
      mode = "replay";
      running = false; // the driver paces frames, not the video loop
      showHint(`Replay: ${file.name} — ${info.frameCount} frames`);
      replayDriver = new ReplayDriver(client, info.frameCount, info.durationMs);
      transport.attach(replayDriver.transportTarget());
      replayDriver.start();
      showToast("info", `Replay loaded: ${file.name}`);
    } catch (err) {
      showToast("error", `Could not load replay: ${err}`);
    }
  };

  const openSource = async (open: () => Promise<void>, isFile: boolean): Promise<void> => {
    try {
      await open();
      startVideo(isFile);
    } catch (err) {
      showHint(`Could not open source: ${err}`, true);
    }
  };

  const openFile = (file: File): void => {
    if (isReplayFile(file.name)) {
      void openReplay(file);
    } else {
      void openSource(() => source.openFile(file), true);
    }
  };

  const pickFile = (accept: string): void => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = accept;
    input.onchange = () => {
      const file = input.files?.[0];
      if (file) openFile(file);
    };
    input.click();
  };

  document.getElementById("btn-file")!.addEventListener("click", () => {
    pickFile("video/*,.ngf,.gz,.part");
  });
  document.getElementById("btn-replay")!.addEventListener("click", () => {
    pickFile(".ngf,.gz,.part");
  });
  document.getElementById("btn-camera")!.addEventListener("click", () => {
    void openSource(() => source.openCamera(), false);
  });
  document.getElementById("btn-screen")!.addEventListener("click", () => {
    void openSource(() => source.openScreen(), false);
  });
  document.getElementById("btn-reset")!.addEventListener("click", () => {
    client.resetLock();
  });
  recordBtn.addEventListener("click", () => {
    recordingEnabled = !recordingEnabled;
    client.setRecording(recordingEnabled);
    recordBtn.classList.toggle("active", recordingEnabled);
    showToast("info", recordingEnabled ? "Game recording on" : "Game recording off");
  });

  // Drag & drop with a full-window overlay.
  const overlay = document.getElementById("drop-overlay")!;
  let dragDepth = 0;
  document.body.addEventListener("dragenter", (ev) => {
    ev.preventDefault();
    dragDepth++;
    overlay.style.display = "flex";
  });
  document.body.addEventListener("dragleave", () => {
    if (--dragDepth <= 0) {
      dragDepth = 0;
      overlay.style.display = "none";
    }
  });
  document.body.addEventListener("dragover", (ev) => ev.preventDefault());
  document.body.addEventListener("drop", (ev) => {
    ev.preventDefault();
    dragDepth = 0;
    overlay.style.display = "none";
    const file = ev.dataTransfer?.files?.[0];
    if (file) openFile(file);
  });

  // Keyboard shortcuts (mirroring the desktop app).
  document.addEventListener("keydown", (ev) => {
    const target = ev.target as HTMLElement;
    if (target.tagName === "INPUT" || target.tagName === "SELECT") return;
    switch (ev.key) {
      case " ":
        ev.preventDefault();
        transport.togglePause();
        break;
      case ",":
        transport.stepFrame(-1);
        break;
      case ".":
        transport.stepFrame(1);
        break;
      case "ArrowUp":
        transport.cycleSpeed(true);
        break;
      case "ArrowDown":
        transport.cycleSpeed(false);
        break;
      case "r":
      case "R":
        client.resetLock();
        break;
      case "o":
      case "O":
        pickFile("video/*,.ngf,.gz,.part");
        break;
    }
  });

  initSettingsUi((configJson) => {
    client.setConfig(configJson);
    showToast("info", "Engine settings applied");
  });

  // Load the engine last: the UI above stays responsive either way.
  try {
    await client.init(JSON.stringify(loadConfig()));
    engineReady = true;
    showHint(
      "Engine ready. Open a video file, camera, screen capture, or .ngf replay — or drop one here.",
    );
  } catch (err) {
    showHint(
      `Failed to load the wasm engine: ${err}. ` +
        "Run `tools/build-wasm.ps1` (or wasm-pack build crates/nestris-wasm --target web --release) and reload.",
      true,
    );
  }
}

void boot();

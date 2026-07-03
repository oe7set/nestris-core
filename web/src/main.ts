// Entry point: capture -> engine worker -> views.
//
// The full CV pipeline (canvas readback, recognition, recording) runs in a
// Web Worker (`worker/engine.ts`); this thread only captures ImageBitmaps,
// decodes compact binary snapshots, and paints the UI — so the page stays
// responsive regardless of frame cost.
//
// UI handlers are attached BEFORE the wasm engine loads, so a failed load
// (missing pkg build, blocked .wasm) surfaces as a visible error instead of
// dead buttons.

import { CaptureSource } from "./capture";
import { EngineClient } from "./engineClient";
import { initSettingsUi, loadConfig } from "./settings";
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

async function boot(): Promise<void> {
  const views = new Views();
  const source = new CaptureSource();
  const client = new EngineClient();
  let engineReady = false;

  let frameCount = 0;
  let fps = 0;
  let fpsWindowStart = performance.now();
  let running = false;

  client.onResult = (result) => {
    const { frame } = result.snapshot;

    for (const rec of result.recordings) {
      downloadRecording(rec);
    }

    views.drawSource(source.video, result.snapshot.lockQuad);
    views.drawCanonical(result.canonical ?? new Uint8Array(0));
    views.drawPlayfield(frame);
    views.updateDashboard(frame);
    views.pushEvents(frame);

    frameCount++;
    const now = performance.now();
    if (now - fpsWindowStart >= 1000) {
      fps = (frameCount * 1000) / (now - fpsWindowStart);
      frameCount = 0;
      fpsWindowStart = now;
    }
    views.updateLock(result.snapshot.lockState, fps);
  };
  client.onError = (message) => showHint(`Engine error: ${message}`, true);

  const processFrame = (): void => {
    if (!engineReady || !source.ready) return;
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
  const start = (): void => {
    if (!engineReady) {
      showHint("Engine not loaded yet — see the console for the error.", true);
      return;
    }
    client.markDiscontinuity();
    if (!running) {
      running = true;
      scheduleNext();
    }
  };

  const openSource = async (open: () => Promise<void>): Promise<void> => {
    try {
      await open();
      start();
    } catch (err) {
      showHint(`Could not open source: ${err}`, true);
    }
  };

  document.getElementById("btn-file")!.addEventListener("click", () => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = "video/*";
    input.onchange = () => {
      const file = input.files?.[0];
      if (file) void openSource(() => source.openFile(file));
    };
    input.click();
  });
  document.getElementById("btn-camera")!.addEventListener("click", () => {
    void openSource(() => source.openCamera());
  });
  document.getElementById("btn-screen")!.addEventListener("click", () => {
    void openSource(() => source.openScreen());
  });
  document.getElementById("btn-reset")!.addEventListener("click", () => {
    client.resetLock();
  });
  document.body.addEventListener("dragover", (ev) => ev.preventDefault());
  document.body.addEventListener("drop", (ev) => {
    ev.preventDefault();
    const file = ev.dataTransfer?.files?.[0];
    if (file) void openSource(() => source.openFile(file));
  });

  initSettingsUi((configJson) => {
    client.setConfig(configJson);
  });

  // Load the engine last: the UI above stays responsive either way.
  try {
    await client.init(JSON.stringify(loadConfig()));
    engineReady = true;
    showHint(
      "Engine ready. Open a video file, camera, or screen capture — or drop a video here.",
    );
  } catch (err) {
    showHint(
      `Failed to load the wasm engine: ${err}. ` +
        "Run `wasm-pack build crates/nestris-wasm --target web --release` and reload.",
      true,
    );
  }
}

void boot();

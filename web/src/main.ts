// Entry point: capture → engine → views loop, plus the recalibration worker.
//
// UI handlers are attached BEFORE the wasm engine loads, so a failed load
// (missing pkg build, blocked .wasm) surfaces as a visible error instead of
// dead buttons.

import { CaptureSource } from "./capture";
import { NestrisEngine } from "./engine";
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
  let engine: NestrisEngine | null = null;
  let worker: Worker | null = null;

  // Hidden canvas used to pull RGBA out of the video element.
  const grab = document.createElement("canvas");
  const grabCtx = grab.getContext("2d", { willReadFrequently: true })!;

  let lastSolvePost = 0;
  let frameCount = 0;
  let fps = 0;
  let fpsWindowStart = performance.now();
  let running = false;

  const processFrame = (): void => {
    if (!engine || !source.ready) return;
    const w = source.video.videoWidth;
    const h = source.video.videoHeight;
    if (grab.width !== w || grab.height !== h) {
      grab.width = w;
      grab.height = h;
    }
    grabCtx.drawImage(source.video, 0, 0, w, h);
    const imageData = grabCtx.getImageData(0, 0, w, h);
    const ts = source.video.currentTime || performance.now() / 1000;
    const frame = engine.process(imageData.data, w, h, ts);

    // Recalibration worker: paced snapshots when the lock asks for them.
    // The engine shortens the interval while the tracker reports urgency.
    const now = performance.now();
    if (
      worker &&
      engine.wantsBackgroundSolve() &&
      now - lastSolvePost >= engine.solveIntervalMs()
    ) {
      lastSolvePost = now;
      const copy = imageData.data.buffer.slice(0);
      worker.postMessage({ data: copy, width: w, height: h, seed: frame.seq }, [
        copy,
      ]);
    }

    // Auto-save finished game recordings as .ngf.gz downloads.
    while (engine.hasFinishedGame()) {
      downloadRecording(engine.takeFinishedGame());
    }

    views.drawSource(source.video, engine.lockQuad());
    views.drawCanonical(engine.canonicalRgba());
    views.drawPlayfield(frame);
    views.updateDashboard(frame);
    views.pushEvents(frame);

    frameCount++;
    if (now - fpsWindowStart >= 1000) {
      fps = (frameCount * 1000) / (now - fpsWindowStart);
      frameCount = 0;
      fpsWindowStart = now;
    }
    views.updateLock(engine.lockState(), fps);
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
    if (!engine) {
      showHint("Engine not loaded yet — see the console for the error.", true);
      return;
    }
    engine.markDiscontinuity();
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
    engine?.resetLock();
  });
  document.body.addEventListener("dragover", (ev) => ev.preventDefault());
  document.body.addEventListener("drop", (ev) => {
    ev.preventDefault();
    const file = ev.dataTransfer?.files?.[0];
    if (file) void openSource(() => source.openFile(file));
  });

  initSettingsUi((configJson) => {
    engine?.setConfig(configJson);
  });

  // Load the engine last: the UI above stays responsive either way.
  try {
    const loaded = new NestrisEngine();
    await loaded.load(JSON.stringify(loadConfig()));
    engine = loaded;
    worker = new Worker(new URL("./worker/recalib.ts", import.meta.url), {
      type: "module",
    });
    worker.onmessage = (
      ev: MessageEvent<{ h: number[]; confidence: number }>,
    ) => {
      engine?.offerSolution(ev.data.h, ev.data.confidence);
    };
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

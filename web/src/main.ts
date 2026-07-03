// Entry point: capture → engine → views loop, plus the recalibration worker.

import { CaptureSource } from "./capture";
import { NestrisEngine } from "./engine";
import { Views } from "./views";

const MIN_SOLVE_INTERVAL_MS = 500;

async function boot(): Promise<void> {
  const engine = new NestrisEngine();
  await engine.load();
  const views = new Views();
  const source = new CaptureSource();
  const worker = new Worker(new URL("./worker/recalib.ts", import.meta.url), {
    type: "module",
  });
  worker.onmessage = (ev: MessageEvent<{ h: number[]; confidence: number }>) => {
    engine.offerSolution(ev.data.h, ev.data.confidence);
  };

  // Hidden canvas used to pull RGBA out of the video element.
  const grab = document.createElement("canvas");
  const grabCtx = grab.getContext("2d", { willReadFrequently: true })!;

  let lastSolvePost = 0;
  let frameCount = 0;
  let fps = 0;
  let fpsWindowStart = performance.now();
  let running = false;

  const processFrame = (): void => {
    if (!source.ready) return;
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
    const now = performance.now();
    if (engine.wantsBackgroundSolve() && now - lastSolvePost >= MIN_SOLVE_INTERVAL_MS) {
      lastSolvePost = now;
      const copy = imageData.data.buffer.slice(0);
      worker.postMessage(
        { data: copy, width: w, height: h, seed: frame.seq },
        [copy],
      );
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
    engine.markDiscontinuity();
    if (!running) {
      running = true;
      scheduleNext();
    }
  };

  document.getElementById("btn-file")!.addEventListener("click", () => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = "video/*";
    input.onchange = async () => {
      const file = input.files?.[0];
      if (file) {
        await source.openFile(file);
        start();
      }
    };
    input.click();
  });
  document.getElementById("btn-camera")!.addEventListener("click", async () => {
    await source.openCamera();
    start();
  });
  document.getElementById("btn-screen")!.addEventListener("click", async () => {
    await source.openScreen();
    start();
  });
  document.getElementById("btn-reset")!.addEventListener("click", () => {
    engine.resetLock();
  });
  document.body.addEventListener("dragover", (ev) => ev.preventDefault());
  document.body.addEventListener("drop", async (ev) => {
    ev.preventDefault();
    const file = ev.dataTransfer?.files?.[0];
    if (file) {
      await source.openFile(file);
      start();
    }
  });
}

void boot();

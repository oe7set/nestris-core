// Capture sources: video file, camera, screen — all land in one <video>.

export type SourceKind = "file" | "camera" | "screen";

export class CaptureSource {
  readonly video: HTMLVideoElement;
  private stream: MediaStream | null = null;
  private objectUrl: string | null = null;

  constructor() {
    this.video = document.createElement("video");
    this.video.muted = true;
    this.video.playsInline = true;
  }

  private release(): void {
    this.stream?.getTracks().forEach((t) => t.stop());
    this.stream = null;
    if (this.objectUrl) {
      URL.revokeObjectURL(this.objectUrl);
      this.objectUrl = null;
    }
    this.video.srcObject = null;
    this.video.src = "";
  }

  async openFile(file: File): Promise<void> {
    this.release();
    this.objectUrl = URL.createObjectURL(file);
    this.video.src = this.objectUrl;
    // Looping is a transport-bar toggle now, not a default.
    this.video.loop = false;
    await this.video.play();
  }

  async openCamera(): Promise<void> {
    this.release();
    this.stream = await navigator.mediaDevices.getUserMedia({
      video: { width: { ideal: 1280 }, height: { ideal: 720 }, frameRate: { ideal: 60 } },
      audio: false,
    });
    this.video.srcObject = this.stream;
    await this.video.play();
  }

  async openScreen(): Promise<void> {
    this.release();
    this.stream = await navigator.mediaDevices.getDisplayMedia({
      video: { frameRate: { ideal: 60 } },
      audio: false,
    });
    this.video.srcObject = this.stream;
    await this.video.play();
  }

  get ready(): boolean {
    return this.video.readyState >= 2 && this.video.videoWidth > 0;
  }
}

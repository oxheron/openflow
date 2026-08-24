export class PcmAudioCapture {
  private stream: MediaStream | null = null;
  private context: AudioContext | null = null;
  private node: AudioWorkletNode | null = null;
  private source: MediaStreamAudioSourceNode | null = null;
  private sink: GainNode | null = null;
  private onFrame: ((frame: ArrayBuffer) => void) | null = null;
  private flushComplete: (() => void) | null = null;
  private stopPromise: Promise<void> | null = null;

  async start(onFrame: (frame: ArrayBuffer) => void): Promise<void> {
    if (this.stream) throw new Error("Microphone capture is already active");
    if (!navigator.mediaDevices?.getUserMedia)
      throw new Error("Microphone capture is unavailable in this runtime");

    const stream = await navigator.mediaDevices.getUserMedia({
      audio: {
        channelCount: 1,
        echoCancellation: true,
        noiseSuppression: true,
        autoGainControl: true,
      },
    });

    try {
      const context = new AudioContext({ latencyHint: "interactive" });
      await context.audioWorklet.addModule("/pcm-capture-worklet.js");
      const source = context.createMediaStreamSource(stream);
      const node = new AudioWorkletNode(context, "openflow-pcm-capture", {
        numberOfInputs: 1,
        numberOfOutputs: 1,
        channelCount: 1,
      });
      const sink = context.createGain();
      sink.gain.value = 0;
      this.onFrame = onFrame;
      node.port.onmessage = (event: MessageEvent<unknown>) => {
        if (event.data instanceof ArrayBuffer) {
          this.onFrame?.(event.data);
        } else if (
          typeof event.data === "object" &&
          event.data !== null &&
          "type" in event.data &&
          event.data.type === "flushed"
        ) {
          this.flushComplete?.();
        }
      };
      source.connect(node).connect(sink).connect(context.destination);
      await context.resume();
      this.stream = stream;
      this.context = context;
      this.node = node;
      this.source = source;
      this.sink = sink;
    } catch (error) {
      for (const track of stream.getTracks()) track.stop();
      throw error;
    }
  }

  async stop(): Promise<void> {
    if (this.stopPromise) return this.stopPromise;
    this.stopPromise = this.stopCapture();
    try {
      await this.stopPromise;
    } finally {
      this.stopPromise = null;
    }
  }

  private async stopCapture(): Promise<void> {
    const node = this.node;
    if (node && this.context?.state !== "closed") {
      await new Promise<void>((resolve) => {
        let settled = false;
        const timer = globalThis.setTimeout(() => finish(), 250);
        const finish = () => {
          if (settled) return;
          settled = true;
          globalThis.clearTimeout(timer);
          this.flushComplete = null;
          resolve();
        };
        this.flushComplete = finish;
        node.port.postMessage({ type: "flush" });
      });
    }
    this.onFrame = null;
    this.flushComplete = null;
    if (node) node.port.onmessage = null;
    node?.disconnect();
    this.source?.disconnect();
    this.sink?.disconnect();
    for (const track of this.stream?.getTracks() ?? []) track.stop();
    await this.context?.close();
    this.stream = null;
    this.context = null;
    this.node = null;
    this.source = null;
    this.sink = null;
  }
}

export interface SpeechBoundaryOptions {
  frameDurationMs: number;
  minimumSpeechMs: number;
  trailingSilenceMs: number;
  minimumThreshold: number;
  noiseMultiplier: number;
}

const defaultBoundaryOptions: SpeechBoundaryOptions = {
  frameDurationMs: 20,
  minimumSpeechMs: 160,
  trailingSilenceMs: 900,
  minimumThreshold: 0.012,
  noiseMultiplier: 3,
};

/** Lightweight client VAD used only to request sentence commits, never to discard audio. */
export class SpeechBoundaryDetector {
  private readonly options: SpeechBoundaryOptions;
  private noiseFloor = 0.004;
  private speechMs = 0;
  private silenceMs = 0;
  private speechConfirmed = false;

  constructor(options: Partial<SpeechBoundaryOptions> = {}) {
    this.options = { ...defaultBoundaryOptions, ...options };
  }

  process(frame: ArrayBuffer): boolean {
    const samples = new Int16Array(frame);
    if (!samples.length) return false;
    let sum = 0;
    for (const sample of samples) {
      const normalized = sample / 32768;
      sum += normalized * normalized;
    }
    const rms = Math.sqrt(sum / samples.length);
    const threshold = Math.max(this.options.minimumThreshold, this.noiseFloor * this.options.noiseMultiplier);
    const voiced = rms >= threshold;

    if (voiced) {
      this.speechMs += this.options.frameDurationMs;
      this.silenceMs = 0;
      if (this.speechMs >= this.options.minimumSpeechMs) this.speechConfirmed = true;
      return false;
    }

    if (!this.speechConfirmed) {
      this.noiseFloor = this.noiseFloor * 0.96 + rms * 0.04;
      this.speechMs = Math.max(0, this.speechMs - this.options.frameDurationMs);
      return false;
    }

    this.silenceMs += this.options.frameDurationMs;
    if (this.silenceMs < this.options.trailingSilenceMs) return false;
    this.speechMs = 0;
    this.silenceMs = 0;
    this.speechConfirmed = false;
    return true;
  }
}

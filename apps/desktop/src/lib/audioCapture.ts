export interface PcmFrameMetrics {
  samples: number;
  nonZeroRatio: number;
  peak: number;
  rms: number;
}

export interface AudioCaptureDiagnostics {
  phase: "requesting" | "capturing" | "stopped";
  trackState: MediaStreamTrackState | "unavailable";
  trackMuted: boolean;
  contextState: AudioContextState | "unavailable";
  frameCount: number;
  frame: PcmFrameMetrics | null;
}

export function inspectPcmFrame(frame: ArrayBuffer): PcmFrameMetrics {
  const samples = new Int16Array(frame);
  let nonZeroSamples = 0;
  let peak = 0;
  let sumSquares = 0;
  for (const sample of samples) {
    if (sample !== 0) nonZeroSamples += 1;
    const normalized = sample / 32768;
    const magnitude = Math.abs(normalized);
    if (magnitude > peak) peak = magnitude;
    sumSquares += normalized * normalized;
  }
  return {
    samples: samples.length,
    nonZeroRatio: samples.length ? nonZeroSamples / samples.length : 0,
    peak,
    rms: samples.length ? Math.sqrt(sumSquares / samples.length) : 0,
  };
}

export class PcmAudioCapture {
  private stream: MediaStream | null = null;
  private context: AudioContext | null = null;
  private node: AudioWorkletNode | null = null;
  private source: MediaStreamAudioSourceNode | null = null;
  private sink: GainNode | null = null;
  private onFrame: ((frame: ArrayBuffer) => void) | null = null;
  private flushComplete: (() => void) | null = null;
  private stopPromise: Promise<void> | null = null;
  private preparePromise: Promise<MediaStream> | null = null;
  private onDiagnostics: ((diagnostics: AudioCaptureDiagnostics) => void) | null = null;
  private diagnostics: AudioCaptureDiagnostics = {
    phase: "stopped",
    trackState: "unavailable",
    trackMuted: false,
    contextState: "unavailable",
    frameCount: 0,
    frame: null,
  };
  private lastDiagnosticAt = 0;

  async prepare(onDiagnostics?: (diagnostics: AudioCaptureDiagnostics) => void): Promise<void> {
    if (!navigator.mediaDevices?.getUserMedia)
      throw new Error("Microphone capture is unavailable in this runtime");
    if (onDiagnostics) this.onDiagnostics = onDiagnostics;
    this.diagnostics = {
      phase: "requesting",
      trackState: this.liveTrack()?.readyState ?? "unavailable",
      trackMuted: this.liveTrack()?.muted ?? false,
      contextState: "unavailable",
      frameCount: 0,
      frame: null,
    };
    this.publishDiagnostics();

    const stream = await this.preparedStream();
    for (const track of stream.getAudioTracks()) track.enabled = false;
    const track = stream.getAudioTracks()[0];
    this.diagnostics = {
      ...this.diagnostics,
      phase: "stopped",
      trackState: track?.readyState ?? "unavailable",
      trackMuted: track?.muted ?? false,
    };
    this.publishDiagnostics();
  }

  async start(
    onFrame: (frame: ArrayBuffer) => void,
    onDiagnostics?: (diagnostics: AudioCaptureDiagnostics) => void,
  ): Promise<void> {
    if (this.context) throw new Error("Microphone capture is already active");
    if (!navigator.mediaDevices?.getUserMedia)
      throw new Error("Microphone capture is unavailable in this runtime");

    this.onDiagnostics = onDiagnostics ?? null;
    this.diagnostics = {
      phase: "requesting",
      trackState: this.liveTrack()?.readyState ?? "unavailable",
      trackMuted: this.liveTrack()?.muted ?? false,
      contextState: "unavailable",
      frameCount: 0,
      frame: null,
    };
    this.publishDiagnostics();

    const stream = await this.preparedStream();
    for (const track of stream.getAudioTracks()) track.enabled = true;

    try {
      const track = stream.getAudioTracks()[0];
      const trackRate = track?.getSettings().sampleRate;
      this.diagnostics = {
        ...this.diagnostics,
        trackState: track?.readyState ?? "unavailable",
        trackMuted: track?.muted ?? false,
      };
      this.publishDiagnostics();
      const inputSampleRate =
        typeof trackRate === "number" &&
        Number.isFinite(trackRate) &&
        trackRate >= 8_000 &&
        trackRate <= 96_000
          ? trackRate
          : 48_000;
      // An explicit context rate avoids a WebKit/macOS mismatch where the
      // output device reports a telephony rate while microphone render quanta
      // still arrive at 48 kHz, which otherwise produces audio about 3x fast.
      const context = new AudioContext({ latencyHint: "interactive", sampleRate: inputSampleRate });
      await context.audioWorklet.addModule("/pcm-capture-worklet.js");
      const source = context.createMediaStreamSource(stream);
      const node = new AudioWorkletNode(context, "openflow-pcm-capture", {
        numberOfInputs: 1,
        numberOfOutputs: 1,
        channelCount: 1,
        processorOptions: { inputSampleRate: context.sampleRate },
      });
      const sink = context.createGain();
      sink.gain.value = 0;
      this.onFrame = onFrame;
      node.port.onmessage = (event: MessageEvent<unknown>) => {
        if (event.data instanceof ArrayBuffer) {
          this.diagnostics = {
            ...this.diagnostics,
            phase: "capturing",
            trackState: track?.readyState ?? "unavailable",
            trackMuted: track?.muted ?? false,
            contextState: context.state,
            frameCount: this.diagnostics.frameCount + 1,
            frame: inspectPcmFrame(event.data),
          };
          const now = Date.now();
          if (this.diagnostics.frameCount === 1 || now - this.lastDiagnosticAt >= 500) {
            this.lastDiagnosticAt = now;
            this.publishDiagnostics();
          }
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
      this.diagnostics = {
        ...this.diagnostics,
        phase: "capturing",
        contextState: context.state,
      };
      this.publishDiagnostics();
      this.stream = stream;
      this.context = context;
      this.node = node;
      this.source = source;
      this.sink = sink;
    } catch (error) {
      for (const track of stream.getAudioTracks()) track.enabled = false;
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
    for (const track of this.stream?.getAudioTracks() ?? []) track.enabled = false;
    await this.context?.close();
    this.context = null;
    this.node = null;
    this.source = null;
    this.sink = null;
    this.diagnostics = {
      ...this.diagnostics,
      phase: "stopped",
      trackState: this.liveTrack()?.readyState ?? "unavailable",
      trackMuted: this.liveTrack()?.muted ?? false,
      contextState: "unavailable",
    };
    this.publishDiagnostics();
    this.onDiagnostics = null;
  }

  async dispose(): Promise<void> {
    await this.stop();
    for (const track of this.stream?.getTracks() ?? []) track.stop();
    this.stream = null;
    this.preparePromise = null;
  }

  private liveTrack(): MediaStreamTrack | undefined {
    return this.stream?.getAudioTracks().find((track) => track.readyState === "live");
  }

  private async preparedStream(): Promise<MediaStream> {
    if (this.liveTrack() && this.stream) return this.stream;
    if (!this.preparePromise) {
      this.preparePromise = navigator.mediaDevices
        .getUserMedia({
          audio: {
            channelCount: 1,
            sampleRate: { ideal: 48_000 },
            echoCancellation: true,
            noiseSuppression: true,
            autoGainControl: true,
          },
        })
        .then((stream) => {
          this.stream = stream;
          return stream;
        })
        .finally(() => {
          this.preparePromise = null;
        });
    }
    return this.preparePromise;
  }

  private publishDiagnostics(): void {
    this.onDiagnostics?.({
      ...this.diagnostics,
      frame: this.diagnostics.frame ? { ...this.diagnostics.frame } : null,
    });
  }
}

export interface SpeechBoundaryOptions {
  sampleRateHz: number;
  minimumSpeechMs: number;
  trailingSilenceMs: number;
  minimumThreshold: number;
  noiseMultiplier: number;
}

const defaultBoundaryOptions: SpeechBoundaryOptions = {
  sampleRateHz: 16_000,
  minimumSpeechMs: 160,
  trailingSilenceMs: 700,
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
    const frameDurationMs = (samples.length * 1000) / this.options.sampleRateHz;
    let sum = 0;
    for (const sample of samples) {
      const normalized = sample / 32768;
      sum += normalized * normalized;
    }
    const rms = Math.sqrt(sum / samples.length);
    const threshold = Math.max(this.options.minimumThreshold, this.noiseFloor * this.options.noiseMultiplier);
    const voiced = rms >= threshold;

    if (voiced) {
      this.speechMs += frameDurationMs;
      this.silenceMs = 0;
      if (this.speechMs >= this.options.minimumSpeechMs) this.speechConfirmed = true;
      return false;
    }

    if (!this.speechConfirmed) {
      this.noiseFloor = this.noiseFloor * 0.96 + rms * 0.04;
      this.speechMs = Math.max(0, this.speechMs - frameDurationMs);
      return false;
    }

    this.silenceMs += frameDurationMs;
    if (this.silenceMs < this.options.trailingSilenceMs) return false;
    this.speechMs = 0;
    this.silenceMs = 0;
    this.speechConfirmed = false;
    return true;
  }
}

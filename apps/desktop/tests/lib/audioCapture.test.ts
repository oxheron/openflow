import { afterEach, describe, expect, it, vi } from "vitest";
import { inspectPcmFrame, PcmAudioCapture, SpeechBoundaryDetector } from "../../src/lib/audioCapture";

afterEach(() => {
  vi.unstubAllGlobals();
});

function pcm(amplitude: number, samples = 320): ArrayBuffer {
  const frame = new Int16Array(samples);
  frame.fill(amplitude);
  return frame.buffer;
}

describe("SpeechBoundaryDetector", () => {
  it("commits after speech followed by sustained silence", () => {
    const detector = new SpeechBoundaryDetector();
    for (let index = 0; index < 12; index += 1) expect(detector.process(pcm(5000))).toBe(false);
    for (let index = 0; index < 44; index += 1) expect(detector.process(pcm(0))).toBe(false);
    expect(detector.process(pcm(0))).toBe(true);
  });

  it("does not commit ambient silence or short impulses", () => {
    const detector = new SpeechBoundaryDetector();
    for (let index = 0; index < 100; index += 1) expect(detector.process(pcm(40))).toBe(false);
    detector.process(pcm(6000));
    for (let index = 0; index < 60; index += 1) expect(detector.process(pcm(0))).toBe(false);
  });

  it("derives elapsed speech and silence from batched PCM sample counts", () => {
    const detector = new SpeechBoundaryDetector();
    expect(detector.process(pcm(5000, 1600))).toBe(false);
    expect(detector.process(pcm(5000, 1600))).toBe(false);
    for (let index = 0; index < 8; index += 1) {
      expect(detector.process(pcm(0, 1600))).toBe(false);
    }
    expect(detector.process(pcm(0, 1600))).toBe(true);
  });
});

describe("inspectPcmFrame", () => {
  it("identifies an all-zero client audio frame", () => {
    expect(inspectPcmFrame(pcm(0))).toEqual({
      samples: 320,
      nonZeroRatio: 0,
      peak: 0,
      rms: 0,
    });
  });

  it("reports amplitude and non-zero samples", () => {
    const metrics = inspectPcmFrame(pcm(16_384));

    expect(metrics.samples).toBe(320);
    expect(metrics.nonZeroRatio).toBe(1);
    expect(metrics.peak).toBe(0.5);
    expect(metrics.rms).toBe(0.5);
  });
});

describe("PcmAudioCapture.prepare", () => {
  it("keeps one disabled live track ready for background reuse", async () => {
    const stop = vi.fn();
    const track = {
      enabled: true,
      muted: false,
      readyState: "live",
      getSettings: () => ({ sampleRate: 48_000 }),
      stop,
    } as unknown as MediaStreamTrack;
    const stream = {
      getAudioTracks: () => [track],
      getTracks: () => [track],
    } as unknown as MediaStream;
    const getUserMedia = vi.fn(async () => stream);
    vi.stubGlobal("navigator", { mediaDevices: { getUserMedia } });
    const capture = new PcmAudioCapture();

    await capture.prepare();
    await capture.prepare();

    expect(getUserMedia).toHaveBeenCalledTimes(1);
    expect(track.enabled).toBe(false);
    expect(stop).not.toHaveBeenCalled();

    await capture.dispose();
    expect(stop).toHaveBeenCalledTimes(1);
  });
});

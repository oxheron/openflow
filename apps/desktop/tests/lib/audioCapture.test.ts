import { describe, expect, it } from "vitest";
import { SpeechBoundaryDetector } from "../../src/lib/audioCapture";

function pcm(amplitude: number): ArrayBuffer {
  const frame = new Int16Array(320);
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
});

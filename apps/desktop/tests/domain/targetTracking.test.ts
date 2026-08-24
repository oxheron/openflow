import { describe, expect, it } from "vitest";
import type { CorrectionPatch } from "../../src/domain/protocol";
import { TargetTracker } from "../../src/domain/targetTracking";

describe("TargetTracker", () => {
  it("emits only new stable suffixes", () => {
    const tracker = new TargetTracker();
    expect(tracker.acceptStablePrefix("a", 1, "Hello")).toEqual({ expectedPrefix: "", text: "Hello" });
    expect(tracker.acceptStablePrefix("a", 2, "Hello world")).toEqual({
      expectedPrefix: "Hello",
      text: " world",
    });
    expect(tracker.acceptStablePrefix("a", 3, "Hello there")).toBeNull();
    expect(tracker.text).toBe("Hello world");
  });

  it("owns a separator between speech segments without including it in corrections", () => {
    const tracker = new TargetTracker();
    expect(tracker.acceptStablePrefix("0", 1, "hello")).toEqual({
      expectedPrefix: "",
      text: "hello",
    });
    expect(tracker.acceptStablePrefix("1", 1, "world")).toEqual({
      expectedPrefix: "hello",
      text: " world",
    });
    expect(tracker.text).toBe("hello world");

    expect(
      tracker.planCorrection({
        type: "correction_patch",
        sessionId: "session",
        segmentId: "1",
        baseRevision: 1,
        revision: 2,
        sequence: 3,
        rawTextSha256: "digest",
        replacement: "World!",
      }),
    ).toEqual({
      expectedText: "hello world",
      startGrapheme: 6,
      endGrapheme: 11,
      replacement: "World!",
    });
  });

  it("does not duplicate whitespace already supplied by recognition", () => {
    const tracker = new TargetTracker();
    tracker.acceptStablePrefix("0", 1, "hello ");
    expect(tracker.acceptStablePrefix("1", 1, "world")).toMatchObject({ text: "world" });

    const leading = new TargetTracker();
    leading.acceptStablePrefix("0", 1, "hello");
    expect(leading.acceptStablePrefix("1", 1, " world")).toMatchObject({ text: " world" });
  });

  it("replaces an unstable prefix when final ASR diverges", () => {
    const tracker = new TargetTracker();
    tracker.acceptStablePrefix("0", 1, "I scream");
    expect(tracker.acceptFinal("0", 2, "ice cream")).toEqual({
      kind: "replace",
      patch: {
        expectedText: "I scream",
        startGrapheme: 0,
        endGrapheme: 8,
        replacement: "ice cream",
      },
    });
    expect(tracker.text).toBe("ice cream");

    const cleanup: CorrectionPatch = {
      type: "correction_patch",
      sessionId: "s",
      segmentId: "0",
      revision: 3,
      baseRevision: 2,
      sequence: 4,
      rawTextSha256: "hash",
      replacement: "Ice cream.",
    };
    expect(tracker.planCorrection(cleanup)).toMatchObject({
      expectedText: "ice cream",
      replacement: "Ice cream.",
    });
  });

  it("maps segment-local corrections to the verified full range", () => {
    const tracker = new TargetTracker();
    tracker.acceptStablePrefix("a", 2, "Hello ");
    tracker.acceptStablePrefix("b", 4, "wrld");
    const event: CorrectionPatch = {
      type: "correction_patch",
      sessionId: "s",
      segmentId: "b",
      revision: 5,
      baseRevision: 4,
      sequence: 7,
      rawTextSha256: "test-hash",
      replacement: "world",
    };
    expect(tracker.planCorrection(event)).toEqual({
      expectedText: "Hello wrld",
      startGrapheme: 6,
      endGrapheme: 10,
      replacement: "world",
    });
    expect(tracker.commitCorrection(event)).toBe(true);
    expect(tracker.text).toBe("Hello world");
    expect(tracker.planCorrection(event)).toBeNull();
  });
});

import { describe, expect, it } from "vitest";
import { dictationReducer, initialDictationState, renderedTranscript } from "../../src/domain/session";

describe("dictationReducer", () => {
  it("moves through arm, listen, finalize and idle", () => {
    let state = dictationReducer(initialDictationState, { type: "arm", requestId: "r1", now: 10 });
    state = dictationReducer(state, { type: "started", sessionId: "s1" });
    expect(state.phase).toBe("listening");
    state = dictationReducer(state, { type: "stop" });
    expect(state.phase).toBe("finalizing");
    state = dictationReducer(state, { type: "stopped" });
    expect(state.phase).toBe("idle");
  });

  it("ignores stale events and applies a revision-matched correction", () => {
    let state = dictationReducer(initialDictationState, { type: "arm", requestId: "r1", now: 10 });
    state = dictationReducer(state, { type: "started", sessionId: "s1" });
    state = dictationReducer(state, {
      type: "partial",
      event: {
        type: "partial_transcript",
        sessionId: "s1",
        segmentId: "seg1",
        revision: 2,
        sequence: 3,
        text: "Open flo",
        stableText: "Open",
      },
    });
    state = dictationReducer(state, {
      type: "partial",
      event: {
        type: "partial_transcript",
        sessionId: "s1",
        segmentId: "seg1",
        revision: 1,
        sequence: 2,
        text: "stale",
        stableText: "stale",
      },
    });
    expect(renderedTranscript(state)).toBe("Open flo");
    state = dictationReducer(state, {
      type: "correction",
      event: {
        type: "correction_patch",
        sessionId: "s1",
        segmentId: "seg1",
        revision: 3,
        baseRevision: 2,
        sequence: 4,
        rawTextSha256: "test-hash",
        replacement: "Open Flow",
      },
    });
    expect(renderedTranscript(state)).toBe("Open Flow");
  });

  it("ignores another session", () => {
    let state = dictationReducer(initialDictationState, { type: "arm", requestId: "r1", now: 10 });
    state = dictationReducer(state, { type: "started", sessionId: "s1" });
    const next = dictationReducer(state, {
      type: "final",
      event: {
        type: "segment_final",
        sessionId: "other",
        segmentId: "seg",
        revision: 1,
        sequence: 1,
        text: "bad",
        formattedText: "bad",
      },
    });
    expect(next).toBe(state);
  });

  it("preserves a failure when the server acknowledges cleanup", () => {
    let state = dictationReducer(initialDictationState, { type: "arm", requestId: "r1", now: 10 });
    state = dictationReducer(state, { type: "started", sessionId: "s1" });
    state = dictationReducer(state, { type: "fail", message: "model failed" });
    state = dictationReducer(state, { type: "stopped" });
    expect(state.phase).toBe("error");
    expect(state.error).toBe("model failed");
  });
});

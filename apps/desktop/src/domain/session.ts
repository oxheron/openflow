import type { CorrectionPatch, PartialTranscript, SegmentFinal } from "./protocol";

export type SessionPhase = "idle" | "arming" | "listening" | "finalizing" | "error";

export interface TranscriptSegment {
  id: string;
  revision: number;
  sequence: number;
  text: string;
  stableText: string;
  final: boolean;
}

export interface DictationState {
  phase: SessionPhase;
  sessionId: string | null;
  requestId: string | null;
  segments: TranscriptSegment[];
  error: string | null;
  startedAt: number | null;
}

export type DictationAction =
  | { type: "arm"; requestId: string; now: number }
  | { type: "started"; sessionId: string }
  | { type: "partial"; event: PartialTranscript }
  | { type: "final"; event: SegmentFinal }
  | { type: "correction"; event: CorrectionPatch }
  | { type: "stop" }
  | { type: "stopped" }
  | { type: "fail"; message: string }
  | { type: "reset" };

export const initialDictationState: DictationState = {
  phase: "idle",
  sessionId: null,
  requestId: null,
  segments: [],
  error: null,
  startedAt: null,
};

function acceptsEvent(state: DictationState, sessionId: string): boolean {
  return state.sessionId === sessionId && state.phase !== "idle" && state.phase !== "error";
}

function upsertSegment(
  segments: TranscriptSegment[],
  id: string,
  revision: number,
  sequence: number,
  update: (previous?: TranscriptSegment) => TranscriptSegment,
): TranscriptSegment[] {
  const index = segments.findIndex((segment) => segment.id === id);
  const previous = index >= 0 ? segments[index] : undefined;
  if (previous && (sequence < previous.sequence || revision < previous.revision)) return segments;
  const next = update(previous);
  if (index < 0) return [...segments, next];
  return segments.map((segment, segmentIndex) => (segmentIndex === index ? next : segment));
}

export function dictationReducer(state: DictationState, action: DictationAction): DictationState {
  switch (action.type) {
    case "arm":
      if (state.phase !== "idle" && state.phase !== "error") return state;
      return {
        ...initialDictationState,
        phase: "arming",
        requestId: action.requestId,
        startedAt: action.now,
      };
    case "started":
      if (state.phase !== "arming" && state.phase !== "finalizing") return state;
      return {
        ...state,
        phase: state.phase === "finalizing" ? "finalizing" : "listening",
        sessionId: action.sessionId,
      };
    case "partial": {
      if (!acceptsEvent(state, action.event.sessionId)) return state;
      const { event } = action;
      return {
        ...state,
        segments: upsertSegment(
          state.segments,
          event.segmentId,
          event.revision,
          event.sequence,
          (previous) => ({
            id: event.segmentId,
            revision: event.revision,
            sequence: event.sequence,
            text: event.text,
            stableText: event.stableText,
            final: previous?.final ?? false,
          }),
        ),
      };
    }
    case "final": {
      if (!acceptsEvent(state, action.event.sessionId)) return state;
      const { event } = action;
      return {
        ...state,
        segments: upsertSegment(state.segments, event.segmentId, event.revision, event.sequence, () => ({
          id: event.segmentId,
          revision: event.revision,
          sequence: event.sequence,
          text: event.text,
          stableText: event.text,
          final: true,
        })),
      };
    }
    case "correction": {
      if (!acceptsEvent(state, action.event.sessionId)) return state;
      const { event } = action;
      const previous = state.segments.find((segment) => segment.id === event.segmentId);
      if (!previous || previous.revision !== event.baseRevision || event.sequence < previous.sequence)
        return state;
      return {
        ...state,
        segments: state.segments.map((segment) =>
          segment.id === event.segmentId
            ? {
                ...segment,
                revision: event.revision,
                sequence: event.sequence,
                text: event.replacement,
                stableText: event.replacement,
                final: true,
              }
            : segment,
        ),
      };
    }
    case "stop":
      return state.phase === "listening" || state.phase === "arming"
        ? { ...state, phase: "finalizing" }
        : state;
    case "stopped":
      if (state.phase === "error") return state;
      return { ...state, phase: "idle", sessionId: null, requestId: null, startedAt: null };
    case "fail":
      return { ...state, phase: "error", error: action.message, sessionId: null, requestId: null };
    case "reset":
      return initialDictationState;
  }
}

export function renderedTranscript(state: DictationState): string {
  return state.segments
    .map((segment) => segment.text.trim())
    .filter(Boolean)
    .join(" ");
}

export function stableTranscript(state: DictationState): string {
  return state.segments
    .map((segment) => segment.stableText.trim())
    .filter(Boolean)
    .join(" ");
}

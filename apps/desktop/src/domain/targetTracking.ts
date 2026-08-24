import { graphemes } from "./patches";
import type { CorrectionPatch } from "./protocol";

interface TrackedSegment {
  id: string;
  revision: number;
  separator: string;
  text: string;
}

export interface StableAppend {
  expectedPrefix: string;
  text: string;
}

export interface TrackedCorrection {
  expectedText: string;
  startGrapheme: number;
  endGrapheme: number;
  replacement: string;
}

export type FinalMutation =
  { kind: "append"; append: StableAppend } | { kind: "replace"; patch: TrackedCorrection };

export class TargetTracker {
  private segments: TrackedSegment[] = [];

  get text(): string {
    return this.segments.map((segment) => segment.separator + segment.text).join("");
  }

  acceptStablePrefix(segmentId: string, revision: number, stableText: string): StableAppend | null {
    let index = this.segments.findIndex((segment) => segment.id === segmentId);
    if (index < 0) {
      index = this.segments.length;
      this.segments.push({ id: segmentId, revision, separator: "", text: "" });
    }
    if (index !== this.segments.length - 1) return null;

    const segment = this.segments[index];
    if (revision < segment.revision || !stableText.startsWith(segment.text)) return null;
    const suffix = stableText.slice(segment.text.length);
    if (!suffix) {
      segment.revision = revision;
      return null;
    }

    const expectedPrefix = this.text;
    if (!segment.text && expectedPrefix && !/\s$/u.test(expectedPrefix) && !/^\s/u.test(stableText)) {
      segment.separator = " ";
    }
    segment.text = stableText;
    segment.revision = revision;
    return { expectedPrefix, text: segment.separator + suffix };
  }

  /**
   * Converges a possibly speculative stable prefix to the final ASR text. A
   * divergent final is replaced only inside this segment's owned range; the
   * exact full target value is still verified by the native adapter.
   */
  acceptFinal(segmentId: string, revision: number, finalText: string): FinalMutation | null {
    const index = this.segments.findIndex((segment) => segment.id === segmentId);
    if (index < 0) {
      const append = this.acceptStablePrefix(segmentId, revision, finalText);
      return append ? { kind: "append", append } : null;
    }
    if (index !== this.segments.length - 1) return null;
    const segment = this.segments[index];
    if (revision < segment.revision) return null;
    if (finalText.startsWith(segment.text)) {
      const append = this.acceptStablePrefix(segmentId, revision, finalText);
      return append ? { kind: "append", append } : null;
    }

    const expectedText = this.text;
    const offset = this.segments
      .slice(0, index)
      .reduce((total, item) => total + graphemes(item.separator + item.text).length, 0);
    const startGrapheme = offset + graphemes(segment.separator).length;
    const endGrapheme = startGrapheme + graphemes(segment.text).length;
    segment.text = finalText;
    segment.revision = revision;
    return {
      kind: "replace",
      patch: { expectedText, startGrapheme, endGrapheme, replacement: finalText },
    };
  }

  planCorrection(event: CorrectionPatch): TrackedCorrection | null {
    const index = this.segments.findIndex((segment) => segment.id === event.segmentId);
    if (index < 0) return null;
    const segment = this.segments[index];
    if (segment.revision !== event.baseRevision) return null;

    const segmentLength = graphemes(segment.text).length;
    const offset = this.segments
      .slice(0, index)
      .reduce((total, item) => total + graphemes(item.separator + item.text).length, 0);
    const separatorLength = graphemes(segment.separator).length;
    return {
      expectedText: this.text,
      startGrapheme: offset + separatorLength,
      endGrapheme: offset + separatorLength + segmentLength,
      replacement: event.replacement,
    };
  }

  commitCorrection(event: CorrectionPatch): boolean {
    const segment = this.segments.find((item) => item.id === event.segmentId);
    if (!segment || segment.revision !== event.baseRevision) return false;
    try {
      segment.text = event.replacement;
      segment.revision = event.revision;
      return true;
    } catch {
      return false;
    }
  }
}

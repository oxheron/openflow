export interface GraphemePatch {
  startGrapheme: number;
  endGrapheme: number;
  replacement: string;
}

const segmenter =
  typeof Intl.Segmenter === "function" ? new Intl.Segmenter(undefined, { granularity: "grapheme" }) : null;

export function graphemes(value: string): string[] {
  if (!segmenter) return Array.from(value);
  return Array.from(segmenter.segment(value), (part) => part.segment);
}

export function applyGraphemePatch(value: string, patch: GraphemePatch): string {
  const parts = graphemes(value);
  if (
    !Number.isInteger(patch.startGrapheme) ||
    !Number.isInteger(patch.endGrapheme) ||
    patch.startGrapheme < 0 ||
    patch.endGrapheme < patch.startGrapheme ||
    patch.endGrapheme > parts.length
  ) {
    throw new RangeError("Patch range is outside the current transcript");
  }
  return [...parts.slice(0, patch.startGrapheme), patch.replacement, ...parts.slice(patch.endGrapheme)].join(
    "",
  );
}

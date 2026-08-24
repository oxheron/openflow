import { describe, expect, it } from "vitest";
import { applyGraphemePatch, graphemes } from "../../src/domain/patches";

describe("applyGraphemePatch", () => {
  it("replaces a basic range", () => {
    expect(
      applyGraphemePatch("hello wrld", { startGrapheme: 6, endGrapheme: 10, replacement: "world" }),
    ).toBe("hello world");
  });

  it("does not split emoji grapheme clusters", () => {
    const value = "Hi 👨‍👩‍👧‍👦!";
    expect(graphemes(value)).toHaveLength(5);
    expect(applyGraphemePatch(value, { startGrapheme: 3, endGrapheme: 4, replacement: "🙂" })).toBe("Hi 🙂!");
  });

  it("rejects invalid and stale ranges", () => {
    expect(() => applyGraphemePatch("short", { startGrapheme: 2, endGrapheme: 9, replacement: "x" })).toThrow(
      RangeError,
    );
  });
});

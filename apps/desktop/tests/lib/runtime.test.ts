import { describe, expect, it } from "vitest";
import { randomVerificationCode } from "../../src/lib/runtime";

describe("randomVerificationCode", () => {
  it("creates a zero-padded six-digit verification code", () => {
    for (let index = 0; index < 32; index += 1) {
      expect(randomVerificationCode()).toMatch(/^\d{6}$/);
    }
  });
});

export function isTauriRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export function randomId(prefix: string): string {
  const id = globalThis.crypto?.randomUUID?.() ?? `${Date.now()}-${Math.random().toString(16).slice(2)}`;
  return `${prefix}-${id}`;
}

export function randomUuid(): string {
  if (!globalThis.crypto?.randomUUID)
    throw new Error("This runtime cannot create a secure session identifier");
  return globalThis.crypto.randomUUID();
}

export function randomVerificationCode(): string {
  if (!globalThis.crypto?.getRandomValues)
    throw new Error("This runtime cannot create a secure verification code");
  const values = new Uint32Array(1);
  // Rejection sampling avoids favoring the low end of the six-digit range.
  const unbiasedLimit = Math.floor(2 ** 32 / 1_000_000) * 1_000_000;
  do {
    globalThis.crypto.getRandomValues(values);
  } while (values[0] >= unbiasedLimit);
  return String(values[0] % 1_000_000).padStart(6, "0");
}

export async function sha256Hex(value: string): Promise<string> {
  const digest = await globalThis.crypto.subtle.digest("SHA-256", new TextEncoder().encode(value));
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
}

export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const power = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  const value = bytes / 1024 ** power;
  return `${value >= 10 || power === 0 ? value.toFixed(0) : value.toFixed(1)} ${units[power]}`;
}

export function friendlyError(error: unknown, fallback = "Something went wrong"): string {
  return error instanceof Error && error.message ? error.message : fallback;
}

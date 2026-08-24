import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { defaultSettings, readSettings, storeSettings } from "../../src/domain/settings";

describe("desktop settings", () => {
  const values = new Map<string, string>();

  beforeEach(() => {
    values.clear();
    vi.stubGlobal("localStorage", {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => values.set(key, value),
    });
  });

  afterEach(() => vi.unstubAllGlobals());

  it("never writes a server credential into webview storage", () => {
    storeSettings({ ...defaultSettings, authToken: "device_token_1234567890", language: "fr" });
    const stored = values.get("openflow.settings.v1") ?? "";
    expect(stored).not.toContain("device_token_1234567890");
    expect(JSON.parse(stored)).toMatchObject({ language: "fr" });
  });

  it("reads a legacy credential once for keyring migration", () => {
    values.set(
      "openflow.settings.v1",
      JSON.stringify({ authToken: "legacy_device_token_1234", serverUrl: "https://host.example" }),
    );
    expect(readSettings()).toMatchObject({
      authToken: "legacy_device_token_1234",
      serverUrl: "https://host.example",
    });
  });
});

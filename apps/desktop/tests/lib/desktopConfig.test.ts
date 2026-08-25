import { describe, expect, it } from "vitest";
import config from "../../src-tauri/tauri.conf.json";

describe("desktop webview configuration", () => {
  it("keeps the microphone capture webview running in the background", () => {
    const mainWindow = config.app.windows.find(({ label }) => label === "main");

    expect(mainWindow?.backgroundThrottling).toBe("disabled");
  });
});

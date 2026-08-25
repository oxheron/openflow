import type { ShortcutEvent, ShortcutHandler } from "@tauri-apps/plugin-global-shortcut";
import { describe, expect, it, vi } from "vitest";
import {
  HotkeyRegistrationCoordinator,
  type HotkeyRegistrationState,
} from "../../src/lib/hotkeyRegistration";

function shortcutEvent(shortcut: string, state: ShortcutEvent["state"]): ShortcutEvent {
  return { shortcut, state, id: 1 };
}

function createHarness() {
  const handlers = new Map<string, ShortcutHandler>();
  const registerPlugin = vi.fn(async (accelerator: string, handler: ShortcutHandler) => {
    handlers.set(accelerator, handler);
  });
  const isPluginRegistered = vi.fn(async () => true);
  const unregisterPlugin = vi.fn(async (accelerator: string) => {
    handlers.delete(accelerator);
  });
  const registerWayland = vi.fn(async () => false);
  const unregisterWayland = vi.fn(async () => undefined);
  const coordinator = new HotkeyRegistrationCoordinator({
    registerPlugin,
    isPluginRegistered,
    unregisterPlugin,
    registerWayland,
    unregisterWayland,
  });

  return {
    coordinator,
    handlers,
    registerPlugin,
    isPluginRegistered,
    unregisterPlugin,
    registerWayland,
    unregisterWayland,
  };
}

describe("HotkeyRegistrationCoordinator", () => {
  it("verifies registration and dispatches only pressed events", async () => {
    const harness = createHarness();
    const onPressed = vi.fn();
    const states: HotkeyRegistrationState[] = [];

    const dispose = harness.coordinator.activate("CommandOrControl+Shift+Space", onPressed, (state) =>
      states.push(state),
    );
    await harness.coordinator.whenIdle();

    expect(harness.registerPlugin).toHaveBeenCalledTimes(1);
    expect(harness.isPluginRegistered).toHaveBeenCalledWith("CommandOrControl+Shift+Space");
    expect(states.map(({ status }) => status)).toEqual(["registering", "active"]);

    const handler = harness.handlers.get("CommandOrControl+Shift+Space");
    handler?.(shortcutEvent("CommandOrControl+Shift+Space", "Released"));
    handler?.(shortcutEvent("CommandOrControl+Shift+Space", "Pressed"));
    expect(onPressed).toHaveBeenCalledTimes(1);

    dispose();
    await harness.coordinator.whenIdle();
    expect(harness.unregisterPlugin).toHaveBeenCalledTimes(1);
  });

  it("coalesces Strict Mode cleanup and registers only the live request", async () => {
    const harness = createHarness();
    const staleToggle = vi.fn();
    const liveToggle = vi.fn();

    const disposeStale = harness.coordinator.activate("CommandOrControl+Shift+Space", staleToggle, vi.fn());
    disposeStale();
    const disposeLive = harness.coordinator.activate("CommandOrControl+Shift+Space", liveToggle, vi.fn());
    await harness.coordinator.whenIdle();

    expect(harness.registerPlugin).toHaveBeenCalledTimes(1);
    expect(harness.unregisterPlugin).not.toHaveBeenCalled();
    harness.handlers.get("CommandOrControl+Shift+Space")?.(
      shortcutEvent("CommandOrControl+Shift+Space", "Pressed"),
    );
    expect(staleToggle).not.toHaveBeenCalled();
    expect(liveToggle).toHaveBeenCalledTimes(1);

    disposeLive();
    await harness.coordinator.whenIdle();
    expect(harness.unregisterPlugin).toHaveBeenCalledTimes(1);
  });

  it("unregisters the old accelerator exactly once before activating its replacement", async () => {
    const harness = createHarness();
    const disposeOld = harness.coordinator.activate("CommandOrControl+Shift+Space", vi.fn(), vi.fn());
    await harness.coordinator.whenIdle();

    disposeOld();
    const disposeNew = harness.coordinator.activate("CommandOrControl+Alt+Space", vi.fn(), vi.fn());
    await harness.coordinator.whenIdle();

    expect(harness.unregisterPlugin).toHaveBeenCalledTimes(1);
    expect(harness.unregisterPlugin).toHaveBeenCalledWith("CommandOrControl+Shift+Space");
    expect(harness.registerPlugin).toHaveBeenCalledTimes(2);
    expect(harness.registerPlugin.mock.invocationCallOrder[0]).toBeLessThan(
      harness.unregisterPlugin.mock.invocationCallOrder[0],
    );
    expect(harness.unregisterPlugin.mock.invocationCallOrder[0]).toBeLessThan(
      harness.registerPlugin.mock.invocationCallOrder[1],
    );

    disposeNew();
    await harness.coordinator.whenIdle();
    expect(harness.unregisterPlugin).toHaveBeenCalledTimes(2);
  });

  it("surfaces the native error and advises changing an occupied shortcut", async () => {
    const harness = createHarness();
    harness.registerPlugin.mockRejectedValueOnce("RegisterEventHotKey failed for Space");
    const states: HotkeyRegistrationState[] = [];

    harness.coordinator.activate("CommandOrControl+Shift+Space", vi.fn(), (state) => states.push(state));
    await harness.coordinator.whenIdle();

    expect(states.at(-1)).toEqual({
      status: "failed",
      error:
        'RegisterEventHotKey failed for Space The shortcut "CommandOrControl+Shift+Space" may already be used by macOS or another application. Choose another shortcut and try again.',
    });
  });

  it("fails and cleans up when isRegistered does not confirm registration", async () => {
    const harness = createHarness();
    harness.isPluginRegistered.mockResolvedValueOnce(false);
    const states: HotkeyRegistrationState[] = [];

    harness.coordinator.activate("CommandOrControl+Shift+Space", vi.fn(), (state) => states.push(state));
    await harness.coordinator.whenIdle();

    expect(harness.unregisterPlugin).toHaveBeenCalledTimes(1);
    expect(states.at(-1)?.status).toBe("failed");
    expect(states.at(-1)?.error).toContain("isRegistered() returned false");
  });
});

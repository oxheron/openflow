import { useEffect, useRef, useState } from "react";
import { register, unregister } from "@tauri-apps/plugin-global-shortcut";
import { registerWaylandHotkey, unregisterWaylandHotkey } from "../lib/bridge";
import { isTauriRuntime } from "../lib/runtime";

let nextRegistrationId = 0;

export function useHotkey(accelerator: string, onToggle: () => void): string | null {
  const [error, setError] = useState<string | null>(null);
  const onToggleRef = useRef(onToggle);

  useEffect(() => {
    onToggleRef.current = onToggle;
  }, [onToggle]);

  useEffect(() => {
    if (!isTauriRuntime()) {
      const listener = (event: KeyboardEvent) => {
        if (event.code === "Space" && event.shiftKey && (event.metaKey || event.ctrlKey)) {
          event.preventDefault();
          onToggleRef.current();
        }
      };
      window.addEventListener("keydown", listener);
      return () => window.removeEventListener("keydown", listener);
    }

    nextRegistrationId += 1;
    const registrationId = `hotkey-${nextRegistrationId}`;
    let active = true;
    let pluginRegistered = false;

    void (async () => {
      try {
        // Wayland deliberately uses the desktop portal so registration is mediated by
        // the compositor and its user-facing permission dialog. macOS and X11 return
        // false here and keep using Tauri's native global-shortcut implementation.
        const portalRegistered = await registerWaylandHotkey(accelerator, registrationId);
        if (!active) {
          if (portalRegistered) await unregisterWaylandHotkey(registrationId);
          return;
        }
        if (portalRegistered) {
          setError(null);
          return;
        }

        await register(accelerator, (event) => {
          if (active && event.state === "Pressed") onToggleRef.current();
        });
        pluginRegistered = true;
        if (!active) {
          await unregister(accelerator);
          return;
        }
        setError(null);
      } catch (reason: unknown) {
        if (active) {
          setError(reason instanceof Error ? reason.message : "Hotkey registration failed");
        }
      }
    })();

    return () => {
      active = false;
      // This also cancels an in-flight Wayland portal permission request. It is a
      // no-op on macOS and X11.
      void unregisterWaylandHotkey(registrationId).catch(() => undefined);
      if (pluginRegistered) void unregister(accelerator).catch(() => undefined);
    };
  }, [accelerator]);

  return error;
}

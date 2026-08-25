import { useEffect, useRef, useState } from "react";
import { isRegistered, register, unregister } from "@tauri-apps/plugin-global-shortcut";
import { registerWaylandHotkey, unregisterWaylandHotkey } from "../lib/bridge";
import { HotkeyRegistrationCoordinator, type HotkeyRegistrationState } from "../lib/hotkeyRegistration";
import { isTauriRuntime } from "../lib/runtime";

const coordinator = new HotkeyRegistrationCoordinator({
  registerPlugin: register,
  isPluginRegistered: isRegistered,
  unregisterPlugin: unregister,
  registerWayland: registerWaylandHotkey,
  unregisterWayland: unregisterWaylandHotkey,
});

export function useHotkey(accelerator: string, onToggle: () => void): HotkeyRegistrationState {
  const [state, setState] = useState<HotkeyRegistrationState>({ status: "registering", error: null });
  const onToggleRef = useRef(onToggle);
  onToggleRef.current = onToggle;

  useEffect(() => {
    if (!isTauriRuntime()) {
      const listener = (event: KeyboardEvent) => {
        if (event.code === "Space" && event.shiftKey && (event.metaKey || event.ctrlKey)) {
          event.preventDefault();
          onToggleRef.current();
        }
      };
      window.addEventListener("keydown", listener);
      setState({ status: "active", error: null });
      return () => window.removeEventListener("keydown", listener);
    }

    // Wayland is mediated by the desktop portal. macOS and X11 return false
    // there and continue through Tauri's native global-shortcut plugin.
    return coordinator.activate(accelerator, () => onToggleRef.current(), setState);
  }, [accelerator]);

  return state;
}

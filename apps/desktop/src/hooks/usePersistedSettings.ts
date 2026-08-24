import { useCallback, useState } from "react";
import { defaultSettings, readSettings, storeSettings, type DesktopSettings } from "../domain/settings";

export function usePersistedSettings() {
  const [settings, setSettingsState] = useState<DesktopSettings>(readSettings);
  const setSettings = useCallback(
    (next: DesktopSettings | ((current: DesktopSettings) => DesktopSettings)) => {
      setSettingsState((current) => {
        const value = typeof next === "function" ? next(current) : next;
        storeSettings(value);
        return value;
      });
    },
    [],
  );
  const resetSettings = useCallback(() => setSettings({ ...defaultSettings }), [setSettings]);
  return { settings, setSettings, resetSettings };
}

export type ConnectionMode = "local" | "remote";

export interface DesktopSettings {
  connectionMode: ConnectionMode;
  serverUrl: string;
  authToken: string;
  hotkey: string;
  asrModelId: string;
  cleanupModelId: string;
  language: string;
  showOverlay: boolean;
  copyFallback: boolean;
}

export const defaultSettings: DesktopSettings = {
  connectionMode: "local",
  serverUrl: "http://127.0.0.1:8765",
  authToken: "",
  hotkey: "CommandOrControl+Shift+Space",
  asrModelId: "",
  cleanupModelId: "",
  language: "auto",
  showOverlay: true,
  copyFallback: true,
};

export function readSettings(): DesktopSettings {
  try {
    const stored = localStorage.getItem("openflow.settings.v1");
    if (!stored) return defaultSettings;
    const decoded = JSON.parse(stored) as Partial<DesktopSettings>;
    // Read a legacy token once so App can migrate it to the OS credential
    // store. `storeSettings` immediately removes it from webview storage.
    return { ...defaultSettings, ...decoded };
  } catch {
    return defaultSettings;
  }
}

export function storeSettings(settings: DesktopSettings): void {
  const { authToken: _credentialLivesInTheOsKeyring, ...nonSecretSettings } = settings;
  localStorage.setItem("openflow.settings.v1", JSON.stringify(nonSecretSettings));
}

import { useCallback, useEffect, useRef, useState } from "react";
import { emitTo, listen } from "@tauri-apps/api/event";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { CheckCircle2 } from "lucide-react";
import { ConnectionPanel } from "./components/ConnectionPanel";
import { DictationPanel } from "./components/DictationPanel";
import { LiveOverlay } from "./components/LiveOverlay";
import { ModelsPanel } from "./components/ModelsPanel";
import { PreferencesPanel } from "./components/PreferencesPanel";
import { Sidebar, type Page } from "./components/Sidebar";
import { useHotkey } from "./hooks/useHotkey";
import { useOpenFlow } from "./hooks/useOpenFlow";
import { usePersistedSettings } from "./hooks/usePersistedSettings";
import {
  copyTranscript,
  deleteServerCredential,
  ensureLocalServer,
  loadServerCredential,
  storeServerCredential,
} from "./lib/bridge";
import { isTauriRuntime } from "./lib/runtime";

export default function App() {
  const [page, setPage] = useState<Page>("dictation");
  const [notice, setNotice] = useState<string | null>(null);
  const [credentialReadyFor, setCredentialReadyFor] = useState<string | null>(
    isTauriRuntime() ? null : "browser",
  );
  const { settings, setSettings, resetSettings } = usePersistedSettings();
  const controller = useOpenFlow(settings);
  const toggle = useCallback(() => {
    void controller.toggle().catch(() => undefined);
  }, [controller.toggle]);
  const hotkeyRegistration = useHotkey(settings.hotkey, toggle);
  const attemptedConnection = useRef<string | null>(null);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    const endpoint = settings.serverUrl;
    let active = true;
    setCredentialReadyFor(null);
    void (async () => {
      // A token left by an older build is migrated before storeSettings removes
      // it from local storage. Otherwise load the endpoint-scoped OS secret.
      const token = settings.authToken || (await loadServerCredential(endpoint)) || "";
      if (settings.authToken) await storeServerCredential(endpoint, settings.authToken);
      if (!active) return;
      setSettings((current) =>
        current.serverUrl === endpoint && current.authToken !== token
          ? { ...current, authToken: token }
          : current,
      );
      setCredentialReadyFor(endpoint);
    })().catch((error: unknown) => {
      if (!active) return;
      setCredentialReadyFor(endpoint);
      setNotice(error instanceof Error ? error.message : "Could not access the OS credential store");
    });
    return () => {
      active = false;
    };
    // Loading is endpoint-driven. Adding authToken would restart this effect
    // while it is migrating the value it just loaded.
  }, [setSettings, settings.serverUrl]);

  const commitCredential = useCallback(async (endpoint: string, token: string) => {
    if (!isTauriRuntime()) return;
    if (token.trim()) await storeServerCredential(endpoint, token.trim());
    else await deleteServerCredential(endpoint);
  }, []);

  useEffect(() => {
    if (isTauriRuntime() && credentialReadyFor !== settings.serverUrl) return;
    const key = `${settings.serverUrl}\n${settings.authToken}`;
    if (attemptedConnection.current === key) return;
    attemptedConnection.current = key;
    void (async () => {
      if (
        isTauriRuntime() &&
        settings.connectionMode === "local" &&
        settings.serverUrl === "http://127.0.0.1:8765"
      ) {
        try {
          const local = await ensureLocalServer();
          if (local.adminToken) {
            try {
              await storeServerCredential(settings.serverUrl, local.adminToken);
            } catch (error) {
              setNotice(
                error instanceof Error
                  ? `${error.message}; the credential is available only until OpenFlow exits`
                  : "The local credential could not be stored securely",
              );
            }
            setSettings((current) => ({ ...current, authToken: local.adminToken ?? "" }));
            return;
          }
        } catch {
          // Connection still runs so an independently managed service can be
          // used when the optional sibling binary is not installed.
        }
      }
      await controller.connect();
    })().catch(() => undefined);
  }, [
    controller.connect,
    credentialReadyFor,
    setSettings,
    settings.authToken,
    settings.connectionMode,
    settings.serverUrl,
  ]);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    let cleanup: (() => void) | undefined;
    void listen("openflow://toggle-requested", toggle).then((unlisten) => {
      cleanup = unlisten;
    });
    return () => cleanup?.();
  }, [toggle]);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    const visible =
      settings.showOverlay && controller.session.phase !== "idle" && controller.session.phase !== "error";
    void (async () => {
      const overlay = await WebviewWindow.getByLabel("overlay");
      if (!overlay) return;
      if (!visible) {
        await overlay.hide();
        return;
      }
      await overlay.show();
      await emitTo("overlay", "openflow://overlay-state", {
        phase: controller.session.phase,
        transcript: controller.transcript,
        hotkey: settings.hotkey,
        audioDiagnostics: controller.audioDiagnostics,
      });
    })().catch(() => undefined);
  }, [
    controller.audioDiagnostics,
    controller.session.phase,
    controller.transcript,
    settings.hotkey,
    settings.showOverlay,
  ]);

  useEffect(() => {
    if (!notice) return;
    const timer = window.setTimeout(() => setNotice(null), 3200);
    return () => window.clearTimeout(timer);
  }, [notice]);

  useEffect(() => {
    if (
      (controller.session.phase !== "idle" && controller.session.phase !== "error") ||
      !settings.copyFallback ||
      !controller.transcript ||
      controller.deliveryPolicy === "direct"
    )
      return;
    void copyTranscript(controller.transcript)
      .then(() => setNotice("Transcript copied to clipboard"))
      .catch(() => undefined);
  }, [controller.deliveryPolicy, controller.session.phase, controller.transcript, settings.copyFallback]);

  return (
    <div className="app-shell">
      <Sidebar page={page} onPageChange={setPage} connection={controller.connection} />
      <main>
        {page === "dictation" && (
          <DictationPanel
            controller={controller}
            settings={settings}
            onNavigate={setPage}
            onNotice={setNotice}
          />
        )}
        {page === "models" && (
          <ModelsPanel
            controller={controller}
            settings={settings}
            onSettingsChange={setSettings}
            onNotice={setNotice}
          />
        )}
        {page === "connection" && (
          <ConnectionPanel
            controller={controller}
            settings={settings}
            onSettingsChange={setSettings}
            onNotice={setNotice}
            onCredentialCommit={commitCredential}
          />
        )}
        {page === "preferences" && (
          <PreferencesPanel
            settings={settings}
            platform={controller.platform}
            hotkeyRegistration={hotkeyRegistration}
            onSettingsChange={setSettings}
            onReset={resetSettings}
          />
        )}
      </main>
      {!isTauriRuntime() && settings.showOverlay && (
        <LiveOverlay
          session={controller.session}
          transcript={controller.transcript}
          hotkey={settings.hotkey}
          onStop={() => void controller.stop()}
          onCopy={() => void copyTranscript(controller.transcript).then(() => setNotice("Transcript copied"))}
        />
      )}
      {notice && (
        <div className="toast">
          <CheckCircle2 size={17} />
          {notice}
        </div>
      )}
    </div>
  );
}

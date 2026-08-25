import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { LoaderCircle, Mic } from "lucide-react";
import type { SessionPhase } from "./domain/session";
import type { AudioCaptureDiagnostics } from "./lib/audioCapture";

export interface OverlayState {
  phase: SessionPhase;
  transcript: string;
  hotkey: string;
  audioDiagnostics: AudioCaptureDiagnostics | null;
}

function audioSummary(diagnostics: AudioCaptureDiagnostics | null): string {
  if (!diagnostics) return "Preparing client audio…";
  if (diagnostics.phase === "requesting") {
    return `Mic ${diagnostics.trackState}${diagnostics.trackMuted ? " (muted)" : ""} · audio ${diagnostics.contextState}`;
  }
  if (!diagnostics.frameCount || !diagnostics.frame) {
    return `Mic ${diagnostics.trackState}${diagnostics.trackMuted ? " (muted)" : ""} · audio ${diagnostics.contextState} · no PCM frames`;
  }
  const nonZero = Math.round(diagnostics.frame.nonZeroRatio * 100);
  return `${diagnostics.frameCount} PCM frames · peak ${diagnostics.frame.peak.toFixed(3)} · RMS ${diagnostics.frame.rms.toFixed(3)} · ${nonZero}% non-zero`;
}

export function OverlayApp() {
  const [state, setState] = useState<OverlayState | null>(null);

  useEffect(() => {
    document.documentElement.classList.add("overlay-document");
    void getCurrentWindow()
      .setIgnoreCursorEvents(true)
      .catch(() => undefined);
    let unlisten: (() => void) | undefined;
    void listen<OverlayState>("openflow://overlay-state", (event) => setState(event.payload)).then((next) => {
      unlisten = next;
    });
    return () => {
      unlisten?.();
      document.documentElement.classList.remove("overlay-document");
    };
  }, []);

  if (!state) return null;
  const active = state.phase === "listening" || state.phase === "arming";
  return (
    <div className="live-overlay overlay-window" role="status" aria-live="polite">
      <div className={`overlay-pulse ${active ? "active" : ""}`}>
        {active ? <Mic size={17} /> : <LoaderCircle size={17} />}
      </div>
      <div className="overlay-copy">
        <span>{active ? "Listening" : "Finishing"}</span>
        <p>{state.transcript || audioSummary(state.audioDiagnostics)}</p>
      </div>
      <kbd>{state.hotkey.replaceAll("CommandOrControl", "⌘ / Ctrl")}</kbd>
    </div>
  );
}

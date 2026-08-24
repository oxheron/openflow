import { Copy, LoaderCircle, Mic, Square } from "lucide-react";
import type { DictationState } from "../domain/session";

interface LiveOverlayProps {
  session: DictationState;
  transcript: string;
  hotkey: string;
  onStop: () => void;
  onCopy: () => void;
}

export function LiveOverlay({ session, transcript, hotkey, onStop, onCopy }: LiveOverlayProps) {
  if (session.phase === "idle" || session.phase === "error") return null;
  const active = session.phase === "listening" || session.phase === "arming";
  return (
    <div className="live-overlay" role="status" aria-live="polite">
      <div className={`overlay-pulse ${active ? "active" : ""}`}>
        {active ? <Mic size={17} /> : <LoaderCircle size={17} />}
      </div>
      <div className="overlay-copy">
        <span>{active ? "Listening" : "Finishing"}</span>
        <p>{transcript || "Start speaking…"}</p>
      </div>
      {transcript && (
        <button type="button" title="Copy transcript" onClick={onCopy}>
          <Copy size={16} />
        </button>
      )}
      <button type="button" title={`Stop (${hotkey})`} onClick={onStop}>
        <Square size={15} fill="currentColor" />
      </button>
    </div>
  );
}

import { AlertTriangle, Check, Copy, Mic, Radio, Server, Square } from "lucide-react";
import type { OpenFlowController } from "../hooks/useOpenFlow";
import type { DesktopSettings } from "../domain/settings";
import { copyTranscript } from "../lib/bridge";
import { formatBytes } from "../lib/runtime";

interface DictationPanelProps {
  controller: OpenFlowController;
  settings: DesktopSettings;
  onNavigate: (page: "models" | "connection") => void;
  onNotice: (message: string) => void;
}

export function DictationPanel({ controller, settings, onNavigate, onNotice }: DictationPanelProps) {
  const isRecording = controller.session.phase === "listening" || controller.session.phase === "arming";
  const asrModel = controller.models.find((model) => model.id === settings.asrModelId);
  const cleanupModel = controller.models.find((model) => model.id === settings.cleanupModelId);
  const modelName = asrModel?.displayName ?? "No ASR model selected";
  const modelReady =
    (asrModel?.state === "cached" || asrModel?.state === "active") &&
    (!settings.cleanupModelId || cleanupModel?.state === "cached" || cleanupModel?.state === "active");
  const targetFallback =
    controller.deliveryPolicy === "overlay_clipboard" ||
    (!controller.deliveryPolicy && controller.platform && controller.platform.policy !== "direct");

  const toggle = async () => {
    try {
      await controller.toggle();
    } catch (error) {
      onNotice(error instanceof Error ? error.message : "Could not toggle dictation");
    }
  };

  return (
    <section className="page-content dictation-page">
      <div className="page-heading">
        <div>
          <span className="eyebrow">READY WHEN YOU ARE</span>
          <h1>
            Speak naturally.
            <br />
            <em>Keep your flow.</em>
          </h1>
        </div>
        <span className={`server-badge ${controller.connection}`}>
          <span className="status-dot" />
          {controller.connection}
        </span>
      </div>

      <div className={`record-card ${isRecording ? "recording" : ""}`}>
        <div className="ambient-ring ring-one" />
        <div className="ambient-ring ring-two" />
        <button
          type="button"
          className="record-button"
          disabled={
            controller.connection !== "connected" || controller.session.phase === "finalizing" || !modelReady
          }
          onClick={() => void toggle()}
          aria-label={isRecording ? "Stop dictation" : "Start dictation"}
        >
          {isRecording ? <Square size={25} fill="currentColor" /> : <Mic size={30} />}
        </button>
        <div className="record-copy">
          <h2>
            {isRecording
              ? "Listening…"
              : controller.session.phase === "finalizing"
                ? "Finishing sentence…"
                : !settings.asrModelId && controller.connection === "connected"
                  ? "Choose a speech model"
                  : !modelReady && controller.connection === "connected"
                    ? "Preparing selected models…"
                    : "Start dictating"}
          </h2>
          <p>
            {isRecording ? (
              "Your audio is streaming to your selected server"
            ) : !settings.asrModelId && controller.connection === "connected" ? (
              "Open the model library to review size and license before downloading"
            ) : !modelReady && controller.connection === "connected" ? (
              "Downloads are verified and cached by the inference server"
            ) : (
              <>
                Press <kbd>{settings.hotkey.replaceAll("CommandOrControl", "⌘ / Ctrl")}</kbd> anywhere
              </>
            )}
          </p>
        </div>
        <div className="waveform" aria-hidden="true">
          {Array.from({ length: 22 }, (_, index) => (
            <i
              key={index}
              style={
                {
                  "--bar": `${24 + ((index * 17) % 64)}%`,
                  "--delay": `${index * -58}ms`,
                } as React.CSSProperties
              }
            />
          ))}
        </div>
      </div>

      {controller.session.error && (
        <div className="inline-alert danger">
          <AlertTriangle size={17} />
          <div>
            <strong>Dictation stopped safely</strong>
            <span>{controller.session.error}</span>
          </div>
        </div>
      )}
      {targetFallback && (
        <div className="inline-alert">
          <Copy size={17} />
          <div>
            <strong>Overlay + clipboard mode</strong>
            <span>{controller.deliveryReason ?? controller.platform?.reason}</span>
          </div>
        </div>
      )}

      {controller.transcript && (
        <div className="transcript-card">
          <div className="card-title-row">
            <div>
              <span className="eyebrow">CURRENT SESSION</span>
              <h3>Transcript</h3>
            </div>
            <button
              className="ghost-button"
              type="button"
              onClick={() =>
                void copyTranscript(controller.transcript).then(() => onNotice("Transcript copied"))
              }
            >
              <Copy size={15} /> Copy
            </button>
          </div>
          <p>{controller.transcript}</p>
        </div>
      )}

      <div className="stats-grid">
        <button className="stat-card" type="button" onClick={() => onNavigate("models")}>
          <div className="stat-icon mint">
            <Radio size={19} />
          </div>
          <div>
            <span>Speech model</span>
            <strong>{modelName}</strong>
          </div>
          <Check size={16} className="stat-check" />
        </button>
        <button className="stat-card" type="button" onClick={() => onNavigate("connection")}>
          <div className="stat-icon amber">
            <Server size={19} />
          </div>
          <div>
            <span>Inference</span>
            <strong>{controller.capabilities?.hardware.deviceName ?? "Not connected"}</strong>
            <small>
              {controller.capabilities
                ? `${controller.capabilities.hardware.backend.toUpperCase()} · ${formatBytes(controller.capabilities.hardware.availableMemoryBytes)} reported`
                : "Configure your server"}
            </small>
          </div>
        </button>
      </div>

      <div className="privacy-note">
        <span className="privacy-lock">◆</span>
        <p>
          <strong>Private by design.</strong> Audio is streamed only to your configured OpenFlow server and is
          not saved by this client.
        </p>
      </div>
    </section>
  );
}

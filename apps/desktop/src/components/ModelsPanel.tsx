import { Check, Cpu, Download, HardDrive, RefreshCw, Square, Trash2 } from "lucide-react";
import type { ModelSpec } from "../domain/protocol";
import type { DesktopSettings } from "../domain/settings";
import type { OpenFlowController } from "../hooks/useOpenFlow";
import { formatBytes } from "../lib/runtime";

interface ModelsPanelProps {
  controller: OpenFlowController;
  settings: DesktopSettings;
  onSettingsChange: (settings: DesktopSettings) => void;
  onNotice: (message: string) => void;
}

function ModelCard({
  model,
  selected,
  onSelect,
  onAction,
}: {
  model: ModelSpec;
  selected: boolean;
  onSelect: () => void;
  onAction: (kind: "download" | "cancel" | "activate" | "delete") => Promise<void>;
}) {
  const action =
    model.state === "available" || model.state === "error"
      ? "download"
      : model.state === "downloading" || model.state === "verifying"
        ? "cancel"
        : model.state === "cached"
          ? "activate"
          : null;
  return (
    <article className={`model-card ${selected ? "selected" : ""}`} onClick={onSelect}>
      <div className="model-card-top">
        <div className={`model-kind ${model.kind}`}>
          <Cpu size={18} />
        </div>
        <div className="model-name">
          <div>
            <h3>{model.displayName}</h3>
            {model.recommended && <span className="recommended">Recommended</span>}
          </div>
          <span>
            {model.family} · {model.parameterLabel} · {model.quantization}
          </span>
        </div>
        {selected && (
          <div className="selected-check">
            <Check size={14} />
          </div>
        )}
      </div>
      <p>{model.description}</p>
      <div className="model-meta">
        <span>
          <HardDrive size={14} />
          {formatBytes(model.downloadBytes)}
        </span>
        <span>{formatBytes(model.estimatedMemoryBytes)} memory</span>
        <span>{model.license}</span>
      </div>
      {model.state === "downloading" && (
        <div className="progress">
          <i style={{ width: `${Math.round((model.progress ?? 0) * 100)}%` }} />
        </div>
      )}
      <div className="model-footer">
        <span className={`model-state ${model.state}`}>
          {model.state === "active" ? (
            <>
              <Check size={13} /> Active
            </>
          ) : (
            model.state
          )}
        </span>
        {action && (
          <button
            className="small-button"
            type="button"
            onClick={(event) => {
              event.stopPropagation();
              void onAction(action);
            }}
          >
            {action === "download" ? (
              <Download size={14} />
            ) : action === "cancel" ? (
              <Square size={13} />
            ) : (
              <Check size={14} />
            )}
            {action}
          </button>
        )}
        {(model.state === "cached" || model.state === "error") && (
          <button
            className="icon-button danger-text"
            type="button"
            title="Delete model"
            onClick={(event) => {
              event.stopPropagation();
              void onAction("delete");
            }}
          >
            <Trash2 size={15} />
          </button>
        )}
      </div>
    </article>
  );
}

export function ModelsPanel({ controller, settings, onSettingsChange, onNotice }: ModelsPanelProps) {
  const perform = async (model: ModelSpec, action: "download" | "cancel" | "activate" | "delete") => {
    try {
      if (action === "download") await controller.downloadModel(model.id);
      if (action === "cancel") await controller.cancelModelDownload(model.id);
      if (action === "activate") {
        await controller.activateModel(model.id);
        onSettingsChange({
          ...settings,
          [model.kind === "asr" ? "asrModelId" : "cleanupModelId"]: model.id,
        });
      }
      if (action === "delete") {
        await controller.deleteModel(model.id);
        onSettingsChange({
          ...settings,
          ...(model.kind === "asr" && settings.asrModelId === model.id ? { asrModelId: "" } : {}),
          ...(model.kind === "cleanup" && settings.cleanupModelId === model.id ? { cleanupModelId: "" } : {}),
        });
      }
      onNotice(
        action === "download"
          ? `${model.displayName}: download started`
          : `${model.displayName}: ${action} complete`,
      );
    } catch (error) {
      onNotice(error instanceof Error ? error.message : `Could not ${action} model`);
    }
  };
  const groups = (["asr", "cleanup"] as const).map((kind) => ({
    kind,
    models: controller.models.filter((model) => model.kind === kind),
  }));

  const disableCleanup = async () => {
    const active = controller.models.find((model) => model.kind === "cleanup" && model.state === "active");
    try {
      if (active) await controller.deactivateModel(active.id);
      onSettingsChange({ ...settings, cleanupModelId: "" });
      onNotice("Text cleanup disabled; its model has been unloaded.");
    } catch (error) {
      onNotice(error instanceof Error ? error.message : "Could not disable text cleanup");
    }
  };

  const select = (model: ModelSpec) => {
    const otherId = model.kind === "asr" ? settings.cleanupModelId : settings.asrModelId;
    const other = controller.models.find((candidate) => candidate.id === otherId);
    const requiredMemory = model.estimatedMemoryBytes + (other?.estimatedMemoryBytes ?? 0);
    const availableMemory = controller.capabilities?.hardware.availableMemoryBytes ?? 0;
    if (availableMemory > 0 && requiredMemory > Math.floor(availableMemory * 0.8)) {
      onNotice(
        `${model.displayName} and the selected ${model.kind === "asr" ? "cleanup" : "speech"} model exceed the server's safe 80% memory budget.`,
      );
      return;
    }
    onSettingsChange({
      ...settings,
      [model.kind === "asr" ? "asrModelId" : "cleanupModelId"]: model.id,
    });
    if (model.state === "available" || model.state === "error") {
      void perform(model, "download");
    } else if (model.state === "cached") {
      void perform(model, "activate");
    }
  };

  return (
    <section className="page-content">
      <div className="page-heading compact">
        <div>
          <span className="eyebrow">ON-DEVICE INTELLIGENCE</span>
          <h1>Model library</h1>
          <p>Choose the best fit for the hardware running your inference server.</p>
        </div>
        <button
          className="ghost-button"
          type="button"
          onClick={() => void controller.refreshModels()}
          disabled={controller.connection !== "connected"}
        >
          <RefreshCw size={15} /> Refresh
        </button>
      </div>
      {controller.connection !== "connected" ? (
        <div className="empty-state">
          <div className="empty-icon">
            <Cpu size={28} />
          </div>
          <h2>Connect to manage models</h2>
          <p>
            The server owns downloads, checksums, cache storage, and activation. No model files are stored by
            the desktop client.
          </p>
        </div>
      ) : (
        groups.map(({ kind, models }) => (
          <div className="model-section" key={kind}>
            <div className="section-heading">
              <div>
                <h2>{kind === "asr" ? "Speech recognition" : "Text cleanup"}</h2>
                <p>
                  {kind === "asr"
                    ? "Whisper models turn audio into stable raw text."
                    : "A small local LLM corrects low-confidence words and formatting."}
                </p>
              </div>
              {kind === "cleanup" && settings.cleanupModelId ? (
                <button className="ghost-button" type="button" onClick={() => void disableCleanup()}>
                  Disable cleanup
                </button>
              ) : (
                <span>{models.length} available</span>
              )}
            </div>
            <div className="model-grid">
              {models.map((model) => {
                const selected =
                  kind === "asr" ? settings.asrModelId === model.id : settings.cleanupModelId === model.id;
                return (
                  <ModelCard
                    key={model.id}
                    model={model}
                    selected={selected}
                    onSelect={() => select(model)}
                    onAction={(action) => perform(model, action)}
                  />
                );
              })}
            </div>
          </div>
        ))
      )}
    </section>
  );
}

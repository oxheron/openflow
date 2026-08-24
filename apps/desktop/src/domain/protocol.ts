export type ConnectionState = "disconnected" | "connecting" | "connected" | "error";
export type BackendKind = "metal" | "cuda" | "rocm" | "vulkan" | "cpu";
export type ModelKind = "asr" | "cleanup";
export type ModelState =
  "available" | "downloading" | "cancelling" | "verifying" | "cached" | "active" | "error";

export interface HardwareProfile {
  platform: string;
  architecture: string;
  logicalCpus: number;
  deviceName: string;
  backend: BackendKind;
  totalMemoryBytes: number;
  availableMemoryBytes: number;
}

export interface ServerCapabilities {
  protocolVersion: number;
  serverVersion: string;
  hardware: HardwareProfile;
  maxAudioBytesPerSession: number;
  maxConcurrentSessions: 1;
  activeSessions: number;
  supportsModelManagement: true;
}

export interface ModelSpec {
  id: string;
  displayName: string;
  kind: ModelKind;
  family: string;
  license: string;
  parameterLabel: string;
  quantization: string;
  downloadBytes: number;
  estimatedMemoryBytes: number;
  description: string;
  recommended?: boolean;
  state: ModelState;
  progress?: number;
  error?: string;
}

export interface SessionConfig {
  sessionId: string;
  asrModelId: string | null;
  cleanupModelId: string | null;
  language: string | null;
  sampleRateHz: 16000;
  channels: 1;
  audioEncoding: "pcm_s16_le";
  glossary: string[];
  options: Record<string, unknown>;
}

export interface PartialTranscript {
  type: "partial_transcript";
  sessionId: string;
  segmentId: string;
  revision: number;
  sequence: number;
  text: string;
  stableText: string;
}

export interface SegmentFinal {
  type: "segment_final";
  sessionId: string;
  segmentId: string;
  revision: number;
  sequence: number;
  text: string;
  formattedText: string;
}

export interface CorrectionPatch {
  type: "correction_patch";
  sessionId: string;
  segmentId: string;
  revision: number;
  sequence: number;
  baseRevision: number;
  rawTextSha256: string;
  replacement: string;
}

export interface ServerError {
  type: "error";
  code: string;
  message: string;
  retryable: boolean;
}

export interface SessionStarted {
  type: "session_started";
  sessionId: string;
}
export interface SessionStopped {
  type: "session_stopped";
  sessionId: string;
}
export interface ServerReady {
  type: "ready";
  protocolVersion: number;
}
export interface ServerPong {
  type: "pong";
  nonce: number;
}

export type ServerEvent =
  | PartialTranscript
  | SegmentFinal
  | CorrectionPatch
  | ServerError
  | SessionStarted
  | SessionStopped
  | ServerReady
  | ServerPong;

export type ClientControlMessage =
  | { type: "start"; payload: SessionConfig }
  | { type: "commit" }
  | { type: "stop" }
  | { type: "ping"; payload: { nonce: number } };

interface WireHardwareProfile {
  os: string;
  architecture: string;
  logical_cpus: number;
  system_memory_bytes: number | null;
  accelerator_memory_bytes: number | null;
  backends: BackendKind[];
}

export interface WireCapabilities {
  protocol_version: number;
  server_version: string;
  hardware: WireHardwareProfile;
  max_audio_bytes_per_session: number;
  active_session: boolean;
}

interface WireModelInfo {
  id: string;
  display_name: string;
  kind: "speech_to_text" | "text_cleanup";
  family: string;
  license: string;
  size_bytes: number;
  estimated_memory_bytes: number;
  quantization: string;
  metadata?: Record<string, string>;
  state: "not_downloaded" | "verifying" | "ready" | "downloading" | "cancelling" | "failed";
  downloaded_bytes?: number;
  total_bytes?: number;
  message?: string;
  active: boolean;
}

export function decodeCapabilities(wire: WireCapabilities): ServerCapabilities {
  const backend = wire.hardware.backends.find((candidate) => candidate !== "cpu") ?? "cpu";
  const memory = wire.hardware.accelerator_memory_bytes ?? wire.hardware.system_memory_bytes ?? 0;
  return {
    protocolVersion: wire.protocol_version,
    serverVersion: wire.server_version,
    hardware: {
      platform: wire.hardware.os,
      architecture: wire.hardware.architecture,
      logicalCpus: wire.hardware.logical_cpus,
      deviceName:
        backend === "cpu"
          ? `${wire.hardware.logical_cpus} CPU threads`
          : `${backend.toUpperCase()} accelerator`,
      backend,
      totalMemoryBytes: memory,
      availableMemoryBytes: memory,
    },
    maxAudioBytesPerSession: wire.max_audio_bytes_per_session,
    maxConcurrentSessions: 1,
    activeSessions: wire.active_session ? 1 : 0,
    supportsModelManagement: true,
  };
}

export function decodeModels(wire: WireModelInfo[]): ModelSpec[] {
  return wire.map((model) => {
    const state = model.active
      ? "active"
      : model.state === "not_downloaded"
        ? "available"
        : model.state === "ready"
          ? "cached"
          : model.state === "failed"
            ? "error"
            : model.state;
    return {
      id: model.id,
      displayName: model.display_name,
      kind: model.kind === "speech_to_text" ? "asr" : "cleanup",
      family: model.family,
      license: model.license,
      parameterLabel: model.metadata?.parameters ?? model.metadata?.tier ?? "",
      quantization: model.quantization,
      downloadBytes: model.size_bytes,
      estimatedMemoryBytes: model.estimated_memory_bytes,
      description: model.metadata?.description ?? `${model.family} model for private, local inference.`,
      recommended: model.metadata?.recommended === "true",
      state,
      progress: model.total_bytes ? (model.downloaded_bytes ?? 0) / model.total_bytes : undefined,
      error: model.message,
    };
  });
}

function objectValue(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

function stringValue(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}
function numberValue(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

/** Returns null for an invalid byte offset instead of typing an ambiguous prefix. */
export function stableUtf8Prefix(text: string, byteLength: number): string | null {
  const bytes = new TextEncoder().encode(text);
  if (!Number.isInteger(byteLength) || byteLength < 0 || byteLength > bytes.length) return null;
  const decoder = new TextDecoder("utf-8", { fatal: true });
  try {
    return decoder.decode(bytes.slice(0, byteLength));
  } catch {
    return null;
  }
}

export function decodeServerEvent(value: unknown): ServerEvent | null {
  const envelope = objectValue(value);
  const type = stringValue(envelope?.type);
  const payload = objectValue(envelope?.payload);
  if (!type) return null;
  if (type === "ready" && payload) {
    const protocolVersion = numberValue(payload.protocol_version);
    return protocolVersion === null ? null : { type: "ready", protocolVersion };
  }
  if ((type === "session_started" || type === "session_stopped") && payload) {
    const sessionId = stringValue(payload.session_id);
    return sessionId ? { type, sessionId } : null;
  }
  if (type === "partial" && payload) {
    const sessionId = stringValue(payload.session_id);
    const segmentId = numberValue(payload.segment_id);
    const revision = numberValue(payload.revision);
    const sequence = numberValue(payload.sequence);
    const text = stringValue(payload.text);
    const stableBytes = numberValue(payload.stable_prefix_bytes);
    const stableText = text !== null && stableBytes !== null ? stableUtf8Prefix(text, stableBytes) : null;
    return sessionId &&
      segmentId !== null &&
      revision !== null &&
      sequence !== null &&
      text !== null &&
      stableText !== null
      ? {
          type: "partial_transcript",
          sessionId,
          segmentId: String(segmentId),
          revision,
          sequence,
          text,
          stableText,
        }
      : null;
  }
  if (type === "final" && payload) {
    const sessionId = stringValue(payload.session_id);
    const segmentId = numberValue(payload.segment_id);
    const revision = numberValue(payload.revision);
    const sequence = numberValue(payload.sequence);
    const text = stringValue(payload.raw_text);
    const formattedText = stringValue(payload.formatted_text);
    return sessionId &&
      segmentId !== null &&
      revision !== null &&
      sequence !== null &&
      text !== null &&
      formattedText !== null
      ? {
          type: "segment_final",
          sessionId,
          segmentId: String(segmentId),
          revision,
          sequence,
          text,
          formattedText,
        }
      : null;
  }
  if (type === "correction" && payload) {
    const sessionId = stringValue(payload.session_id);
    const segmentId = numberValue(payload.segment_id);
    const revision = numberValue(payload.revision);
    const baseRevision = numberValue(payload.base_revision);
    const sequence = numberValue(payload.sequence);
    const rawTextSha256 = stringValue(payload.raw_text_sha256);
    const replacement = stringValue(payload.replacement);
    return sessionId &&
      segmentId !== null &&
      revision !== null &&
      baseRevision !== null &&
      sequence !== null &&
      rawTextSha256 &&
      replacement !== null
      ? {
          type: "correction_patch",
          sessionId,
          segmentId: String(segmentId),
          revision,
          baseRevision,
          sequence,
          rawTextSha256,
          replacement,
        }
      : null;
  }
  if (type === "pong" && payload) {
    const nonce = numberValue(payload.nonce);
    return nonce === null ? null : { type: "pong", nonce };
  }
  if (type === "error" && payload) {
    const code = stringValue(payload.code);
    const message = stringValue(payload.message);
    return code && message ? { type: "error", code, message, retryable: payload.retryable === true } : null;
  }
  return null;
}

export function encodeClientMessage(message: ClientControlMessage): string {
  if (message.type !== "start") return JSON.stringify(message);
  const config = message.payload;
  return JSON.stringify({
    type: "start",
    payload: {
      session_id: config.sessionId,
      audio_encoding: config.audioEncoding,
      sample_rate_hz: config.sampleRateHz,
      channels: config.channels,
      language: config.language,
      asr_model_id: config.asrModelId,
      cleanup_model_id: config.cleanupModelId,
      glossary: config.glossary,
      options: config.options,
    },
  });
}

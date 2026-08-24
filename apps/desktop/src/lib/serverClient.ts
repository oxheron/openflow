import type {
  ClientControlMessage,
  ModelSpec,
  ServerCapabilities,
  ServerEvent,
  WireCapabilities,
} from "../domain/protocol";
import { decodeCapabilities, decodeModels, decodeServerEvent, encodeClientMessage } from "../domain/protocol";
import { fetch as tauriFetch } from "@tauri-apps/plugin-http";
import { isTauriRuntime } from "./runtime";

export interface ServerClientOptions {
  baseUrl: string;
  authToken?: string;
  onEvent: (event: ServerEvent) => void | Promise<void>;
  onDisconnect: (reason: string) => void;
}

export interface PairedDevice {
  deviceId: string;
  deviceToken: string;
}

export class OrderedEventDispatcher<T> {
  private tail: Promise<void> = Promise.resolve();

  constructor(
    private readonly handler: (event: T) => void | Promise<void>,
    private readonly onError: () => void,
  ) {}

  dispatch(event: T): void {
    this.tail = this.tail.then(() => this.handler(event)).catch(() => this.onError());
  }
}

export function canQueueAudio(bufferedBytes: number, frameBytes: number, maximumBytes: number): boolean {
  return (
    Number.isSafeInteger(bufferedBytes) &&
    Number.isSafeInteger(frameBytes) &&
    bufferedBytes >= 0 &&
    frameBytes >= 0 &&
    bufferedBytes + frameBytes <= maximumBytes
  );
}

export function assertProtocolVersion(version: number): void {
  if (!Number.isSafeInteger(version) || version !== 1) {
    throw new Error(`Incompatible OpenFlow protocol ${version}; expected 1`);
  }
}

export function decodeServerFrame(data: unknown): ServerEvent {
  if (typeof data !== "string") throw new Error("The inference server sent a non-text protocol frame");
  let decoded: unknown;
  try {
    decoded = JSON.parse(data) as unknown;
  } catch {
    throw new Error("The inference server sent malformed JSON");
  }
  const event = decodeServerEvent(decoded);
  if (!event) throw new Error("The inference server sent an invalid OpenFlow event");
  return event;
}

export function normalizedBaseUrl(value: string): URL {
  const url = new URL(value);
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    throw new Error("Server URL must use http:// or https://");
  }
  if (url.username || url.password) {
    throw new Error("Server URLs must not contain embedded credentials");
  }
  if (url.search || url.hash) {
    throw new Error("Server URLs must not contain a query or fragment");
  }
  const loopback = url.hostname === "localhost" || url.hostname === "127.0.0.1" || url.hostname === "[::1]";
  if (url.protocol === "http:" && !loopback) {
    throw new Error("Remote OpenFlow servers require HTTPS");
  }
  url.pathname = url.pathname.replace(/\/$/, "");
  return url;
}

function websocketUrl(base: URL): URL {
  const url = new URL(base);
  url.protocol = base.protocol === "https:" ? "wss:" : "ws:";
  url.pathname = `${base.pathname}/v1/dictation`.replace(/\/+/g, "/");
  return url;
}

function tokenProtocol(token: string): string {
  if (!/^[A-Za-z0-9_-]{16,512}$/.test(token)) {
    throw new Error("Device token must be a URL-safe token containing only letters, numbers, _ or -");
  }
  return `openflow.bearer.${token}`;
}

export class OpenFlowServerClient {
  private static readonly requestTimeoutMs = 30_000;
  private static readonly handshakeTimeoutMs = 10_000;
  private static readonly maxBufferedAudioBytes = 1024 * 1024;
  private readonly baseUrl: URL;
  private readonly token: string;
  private readonly onEvent: (event: ServerEvent) => void | Promise<void>;
  private readonly onDisconnect: (reason: string) => void;
  private socket: WebSocket | null = null;
  private intentionalClose = false;
  private readonly eventDispatcher: OrderedEventDispatcher<ServerEvent>;

  constructor(options: ServerClientOptions) {
    this.baseUrl = normalizedBaseUrl(options.baseUrl);
    this.token = options.authToken?.trim() ?? "";
    this.onEvent = options.onEvent;
    this.onDisconnect = options.onDisconnect;
    this.eventDispatcher = new OrderedEventDispatcher(this.onEvent, () => {
      this.close();
      this.onDisconnect("Could not safely process an inference server event");
    });
  }

  private headers(): HeadersInit {
    return this.token ? { Authorization: `Bearer ${this.token}` } : {};
  }

  private async request<T>(
    path: string,
    init?: RequestInit,
    timeoutMs = OpenFlowServerClient.requestTimeoutMs,
  ): Promise<T> {
    const url = new URL(`${this.baseUrl.pathname}${path}`.replace(/\/+/g, "/"), this.baseUrl);
    // Native HTTP avoids webview CORS while Tauri's URL scope still limits the
    // renderer to loopback HTTP and TLS-protected remote endpoints.
    const fetchRequest = isTauriRuntime() ? tauriFetch : globalThis.fetch;
    const timeout = new AbortController();
    const timer = globalThis.setTimeout(() => timeout.abort("OpenFlow server request timed out"), timeoutMs);
    let response: Response;
    try {
      response = await fetchRequest(url, {
        ...init,
        signal: timeout.signal,
        headers: { "Content-Type": "application/json", ...this.headers(), ...init?.headers },
      });
    } finally {
      globalThis.clearTimeout(timer);
    }
    if (!response.ok) {
      let detail = `${response.status} ${response.statusText}`;
      try {
        const body = (await response.json()) as { message?: string };
        if (body.message) detail = body.message;
      } catch {
        /* Preserve the HTTP error. */
      }
      throw new Error(detail);
    }
    if (response.status === 204) return undefined as T;
    return response.json() as Promise<T>;
  }

  async capabilities(): Promise<ServerCapabilities> {
    const capabilities = decodeCapabilities(await this.request<WireCapabilities>("/v1/capabilities"));
    assertProtocolVersion(capabilities.protocolVersion);
    return capabilities;
  }

  async models(): Promise<ModelSpec[]> {
    return decodeModels(await this.request<Parameters<typeof decodeModels>[0]>("/v1/models"));
  }

  async downloadModel(modelId: string): Promise<void> {
    await this.request("/v1/models/download", {
      method: "POST",
      body: JSON.stringify({ model_id: modelId }),
    });
  }

  async cancelModelDownload(modelId: string): Promise<void> {
    await this.request("/v1/models/cancel", {
      method: "POST",
      body: JSON.stringify({ model_id: modelId }),
    });
  }

  async activateModel(modelId: string): Promise<void> {
    await this.request(
      "/v1/models/activate",
      {
        method: "POST",
        body: JSON.stringify({ model_id: modelId }),
      },
      6 * 60 * 1000,
    );
  }

  async deactivateModel(modelId: string): Promise<void> {
    await this.request("/v1/models/deactivate", {
      method: "POST",
      body: JSON.stringify({ model_id: modelId }),
    });
  }

  async deleteModel(modelId: string): Promise<void> {
    await this.request(`/v1/models/${encodeURIComponent(modelId)}`, { method: "DELETE" });
  }

  async pair(pairingCode: string, deviceName: string): Promise<PairedDevice> {
    const response = await this.request<{ device_id: string; device_token: string }>("/v1/pair", {
      method: "POST",
      body: JSON.stringify({ pairing_code: pairingCode.trim(), device_name: deviceName.trim() }),
    });
    return { deviceId: response.device_id, deviceToken: response.device_token };
  }

  async pairInteractively(deviceName: string, verificationCode: string): Promise<PairedDevice> {
    const response = await this.request<{ device_id: string; device_token: string }>(
      "/v1/pair/interactive",
      {
        method: "POST",
        body: JSON.stringify({
          device_name: deviceName.trim(),
          verification_code: verificationCode,
        }),
      },
      10 * 60 * 1000,
    );
    return { deviceId: response.device_id, deviceToken: response.device_token };
  }

  connectStream(): Promise<void> {
    if (this.socket?.readyState === WebSocket.OPEN) return Promise.resolve();
    this.intentionalClose = false;
    return new Promise((resolve, reject) => {
      const protocols = this.token ? ["openflow.v1", tokenProtocol(this.token)] : ["openflow.v1"];
      const socket = new WebSocket(websocketUrl(this.baseUrl), protocols);
      socket.binaryType = "arraybuffer";
      this.socket = socket;
      let ready = false;
      let settled = false;
      const timer = globalThis.setTimeout(() => {
        if (settled) return;
        settled = true;
        this.intentionalClose = true;
        socket.close(1002, "protocol handshake timed out");
        reject(new Error("The inference server did not complete the OpenFlow handshake"));
      }, OpenFlowServerClient.handshakeTimeoutMs);
      const failProtocol = (message: string) => {
        if (settled) return;
        settled = true;
        globalThis.clearTimeout(timer);
        this.intentionalClose = true;
        socket.close(1002, "incompatible protocol");
        reject(new Error(message));
      };
      const terminateConnectedProtocol = (message: string) => {
        if (!ready || this.intentionalClose) return;
        this.intentionalClose = true;
        socket.close(1002, "invalid protocol frame");
        this.onDisconnect(message);
      };
      socket.addEventListener("open", () => {
        if (socket.protocol !== "openflow.v1") {
          failProtocol("The server did not negotiate the OpenFlow WebSocket protocol");
        }
      });
      socket.addEventListener("error", () => {
        if (!ready) failProtocol("Could not open the dictation stream");
      });
      socket.addEventListener("message", (message) => {
        let event: ServerEvent;
        try {
          event = decodeServerFrame(message.data);
          if (event.type === "ready") {
            try {
              assertProtocolVersion(event.protocolVersion);
            } catch (error) {
              failProtocol(error instanceof Error ? error.message : "Incompatible OpenFlow protocol");
              return;
            }
            if (ready) {
              terminateConnectedProtocol("The inference server repeated its ready handshake");
              return;
            }
            ready = true;
            if (!settled) {
              settled = true;
              globalThis.clearTimeout(timer);
              resolve();
            }
          } else if (!ready) {
            failProtocol("The server sent dictation data before the OpenFlow ready message");
            return;
          }
          // Preserve wire order even when an event (notably correction hash
          // verification) performs asynchronous work.
          this.eventDispatcher.dispatch(event);
        } catch (error) {
          const reason = error instanceof Error ? error.message : "The inference server sent invalid data";
          if (ready) terminateConnectedProtocol(reason);
          else failProtocol(reason);
        }
      });
      socket.addEventListener("close", (event) => {
        globalThis.clearTimeout(timer);
        this.socket = null;
        if (!ready && !settled) {
          settled = true;
          reject(new Error(event.reason || `Connection closed (${event.code})`));
        }
        if (!this.intentionalClose) this.onDisconnect(event.reason || `Connection closed (${event.code})`);
      });
    });
  }

  sendControl(message: ClientControlMessage): void {
    if (this.socket?.readyState !== WebSocket.OPEN) throw new Error("Dictation stream is not connected");
    this.socket.send(encodeClientMessage(message));
  }

  sendAudio(frame: ArrayBuffer): boolean {
    if (this.socket?.readyState !== WebSocket.OPEN) return false;
    if (
      !canQueueAudio(this.socket.bufferedAmount, frame.byteLength, OpenFlowServerClient.maxBufferedAudioBytes)
    ) {
      return false;
    }
    this.socket.send(frame);
    return true;
  }

  close(): void {
    this.intentionalClose = true;
    this.socket?.close(1000, "client closed");
    this.socket = null;
  }
}

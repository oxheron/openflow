import type { ShortcutEvent } from "@tauri-apps/plugin-global-shortcut";

export type HotkeyRegistrationStatus = "registering" | "active" | "failed";

export interface HotkeyRegistrationState {
  status: HotkeyRegistrationStatus;
  error: string | null;
}

interface HotkeyRegistrationDependencies {
  registerPlugin: (accelerator: string, handler: (event: ShortcutEvent) => void) => Promise<void>;
  isPluginRegistered: (accelerator: string) => Promise<boolean>;
  unregisterPlugin: (accelerator: string) => Promise<void>;
  registerWayland: (accelerator: string, registrationId: string) => Promise<boolean>;
  unregisterWayland: (registrationId: string) => Promise<void>;
}

interface RegistrationRequest {
  id: number;
  accelerator: string;
  onPressed: () => void;
  onStateChange: (state: HotkeyRegistrationState) => void;
  registrationId: string;
}

interface ActiveRegistration {
  requestId: number;
  accelerator: string;
  registrationId: string;
  backend: "plugin" | "wayland";
}

function errorDetail(reason: unknown): string {
  if (reason instanceof Error && reason.message) return reason.message;
  if (typeof reason === "string" && reason) return reason;
  try {
    const serialized = JSON.stringify(reason);
    if (serialized) return serialized;
  } catch {
    // Fall through to a stable message for non-serializable values.
  }
  return "Unknown global-shortcut registration error";
}

function failedState(accelerator: string, reason: unknown): HotkeyRegistrationState {
  const detail = errorDetail(reason);
  return {
    status: "failed",
    error: `${detail} The shortcut "${accelerator}" may already be used by macOS or another application. Choose another shortcut and try again.`,
  };
}

/**
 * Serializes native shortcut ownership so delayed cleanup can never unregister a
 * newer registration of the same accelerator. Synchronous effect teardown/setup
 * is coalesced before native work begins, including React Strict Mode's dev pass.
 */
export class HotkeyRegistrationCoordinator {
  private nextRequestId = 0;
  private desired: RegistrationRequest | null = null;
  private active: ActiveRegistration | null = null;
  private failedRequestId: number | null = null;
  private scheduled = false;
  private running = false;
  private idleWaiters: Array<() => void> = [];

  constructor(private readonly dependencies: HotkeyRegistrationDependencies) {}

  activate(
    accelerator: string,
    onPressed: () => void,
    onStateChange: (state: HotkeyRegistrationState) => void,
  ): () => void {
    const id = ++this.nextRequestId;
    const request: RegistrationRequest = {
      id,
      accelerator,
      onPressed,
      onStateChange,
      registrationId: `hotkey-${id}`,
    };
    this.desired = request;
    this.failedRequestId = null;
    onStateChange({ status: "registering", error: null });
    this.scheduleReconcile();

    return () => {
      if (this.desired?.id !== id) return;
      this.desired = null;
      this.failedRequestId = null;
      this.scheduleReconcile();
    };
  }

  async whenIdle(): Promise<void> {
    if (!this.scheduled && !this.running) return;
    await new Promise<void>((resolve) => this.idleWaiters.push(resolve));
  }

  private scheduleReconcile(): void {
    if (this.scheduled || this.running) return;
    this.scheduled = true;
    queueMicrotask(() => {
      this.scheduled = false;
      void this.reconcile();
    });
  }

  private async reconcile(): Promise<void> {
    if (this.running) return;
    this.running = true;
    try {
      while (true) {
        const desired = this.desired;

        if (this.active && this.active.requestId !== desired?.id) {
          const stale = this.active;
          this.active = null;
          await this.unregister(stale);
          continue;
        }

        if (!desired || this.active?.requestId === desired.id || this.failedRequestId === desired.id) return;

        await this.register(desired);
      }
    } finally {
      this.running = false;
      const actualId = this.active?.requestId ?? null;
      const desiredId = this.desired?.id ?? null;
      if (actualId !== desiredId && this.failedRequestId !== desiredId) this.scheduleReconcile();
      else this.resolveIdleWaiters();
    }
  }

  private async register(request: RegistrationRequest): Promise<void> {
    try {
      const portalRegistered = await this.dependencies.registerWayland(
        request.accelerator,
        request.registrationId,
      );
      if (portalRegistered) {
        if (this.desired?.id !== request.id) {
          await this.dependencies.unregisterWayland(request.registrationId);
          return;
        }
        this.active = {
          requestId: request.id,
          accelerator: request.accelerator,
          registrationId: request.registrationId,
          backend: "wayland",
        };
        request.onStateChange({ status: "active", error: null });
        return;
      }

      let pluginRegistered = false;
      try {
        await this.dependencies.registerPlugin(request.accelerator, (event) => {
          if (event.state === "Pressed" && this.desired?.id === request.id) request.onPressed();
        });
        pluginRegistered = true;

        const confirmed = await this.dependencies.isPluginRegistered(request.accelerator);
        if (!confirmed) {
          throw new Error(
            `Global shortcut registration was not confirmed: isRegistered() returned false for "${request.accelerator}".`,
          );
        }

        if (this.desired?.id !== request.id) {
          await this.dependencies.unregisterPlugin(request.accelerator);
          return;
        }

        this.active = {
          requestId: request.id,
          accelerator: request.accelerator,
          registrationId: request.registrationId,
          backend: "plugin",
        };
        request.onStateChange({ status: "active", error: null });
      } catch (reason) {
        if (pluginRegistered)
          await this.dependencies.unregisterPlugin(request.accelerator).catch(() => undefined);
        throw reason;
      }
    } catch (reason) {
      if (this.desired?.id !== request.id) return;
      this.failedRequestId = request.id;
      request.onStateChange(failedState(request.accelerator, reason));
    }
  }

  private async unregister(registration: ActiveRegistration): Promise<void> {
    if (registration.backend === "wayland") {
      await this.dependencies.unregisterWayland(registration.registrationId).catch(() => undefined);
      return;
    }
    await this.dependencies.unregisterPlugin(registration.accelerator).catch(() => undefined);
  }

  private resolveIdleWaiters(): void {
    const waiters = this.idleWaiters.splice(0);
    for (const resolve of waiters) resolve();
  }
}

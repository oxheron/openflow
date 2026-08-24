import { invoke } from "@tauri-apps/api/core";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { isTauriRuntime } from "./runtime";

export type TargetPolicy = "direct" | "overlay_clipboard" | "blocked";

export interface PlatformCapabilities {
  platform: "macos" | "linux" | "unsupported";
  sessionType: "native" | "x11" | "wayland" | "unknown";
  directInsertionAvailable: boolean;
  targetVerificationAvailable: boolean;
  policy: TargetPolicy;
  reason: string;
}

export interface TargetLease {
  leaseId: number;
  policy: TargetPolicy;
  initialRevision: number;
  reason: string;
}

export interface LocalServerLaunch {
  available: boolean;
  started: boolean;
  adminToken: string | null;
}

export interface NativePatchRequest {
  leaseId: number;
  baseRevision: number;
  expectedText: string;
  startGrapheme: number;
  endGrapheme: number;
  replacement: string;
}

const browserCapabilities: PlatformCapabilities = {
  platform: "unsupported",
  sessionType: "unknown",
  directInsertionAvailable: false,
  targetVerificationAvailable: false,
  policy: "overlay_clipboard",
  reason: "Browser preview cannot access the focused application. Results stay in the overlay.",
};

export async function getPlatformCapabilities(): Promise<PlatformCapabilities> {
  if (!isTauriRuntime()) return browserCapabilities;
  return invoke<PlatformCapabilities>("get_platform_capabilities");
}

export async function ensureLocalServer(): Promise<LocalServerLaunch> {
  if (!isTauriRuntime()) return { available: false, started: false, adminToken: null };
  return invoke<LocalServerLaunch>("ensure_local_server");
}

export async function loadServerCredential(endpoint: string): Promise<string | null> {
  if (!isTauriRuntime()) return null;
  return invoke<string | null>("load_server_credential", { endpoint });
}

export async function storeServerCredential(endpoint: string, token: string): Promise<void> {
  if (isTauriRuntime()) await invoke("store_server_credential", { endpoint, token });
}

export async function deleteServerCredential(endpoint: string): Promise<void> {
  if (isTauriRuntime()) await invoke("delete_server_credential", { endpoint });
}

export async function registerWaylandHotkey(accelerator: string, registrationId: string): Promise<boolean> {
  if (!isTauriRuntime()) return false;
  return invoke<boolean>("register_wayland_hotkey", { accelerator, registrationId });
}

export async function unregisterWaylandHotkey(registrationId: string): Promise<void> {
  if (isTauriRuntime()) await invoke("unregister_wayland_hotkey", { registrationId });
}

export async function isOpenFlowWindowFocused(): Promise<boolean> {
  if (!isTauriRuntime()) return false;
  return invoke<boolean>("is_openflow_window_focused");
}

export async function captureTarget(): Promise<TargetLease> {
  if (!isTauriRuntime())
    return {
      leaseId: 0,
      policy: "overlay_clipboard",
      initialRevision: 0,
      reason: browserCapabilities.reason,
    };
  return invoke<TargetLease>("capture_target");
}

export async function insertStableText(
  leaseId: number,
  baseRevision: number,
  expectedPrefix: string,
  text: string,
): Promise<number> {
  if (!isTauriRuntime()) throw new Error(browserCapabilities.reason);
  return invoke<number>("insert_stable_text", { request: { leaseId, baseRevision, expectedPrefix, text } });
}

export async function applyNativePatch(request: NativePatchRequest): Promise<number> {
  if (!isTauriRuntime()) throw new Error(browserCapabilities.reason);
  return invoke<number>("apply_target_patch", { request });
}

export async function releaseTarget(leaseId: number): Promise<void> {
  if (isTauriRuntime()) await invoke("release_target", { leaseId });
}

export async function copyTranscript(text: string): Promise<void> {
  if (isTauriRuntime()) {
    await writeText(text);
    return;
  }
  await navigator.clipboard.writeText(text);
}

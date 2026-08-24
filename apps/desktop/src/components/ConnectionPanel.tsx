import {
  AlertTriangle,
  CheckCircle2,
  Link,
  Network,
  PlugZap,
  Server,
  ShieldCheck,
  Unplug,
} from "lucide-react";
import { useState } from "react";
import type { DesktopSettings } from "../domain/settings";
import type { OpenFlowController } from "../hooks/useOpenFlow";
import { formatBytes, randomVerificationCode } from "../lib/runtime";

interface ConnectionPanelProps {
  controller: OpenFlowController;
  settings: DesktopSettings;
  onSettingsChange: (settings: DesktopSettings) => void;
  onNotice: (message: string) => void;
  onCredentialCommit: (endpoint: string, token: string) => Promise<void>;
}

export function ConnectionPanel({
  controller,
  settings,
  onSettingsChange,
  onNotice,
  onCredentialCommit,
}: ConnectionPanelProps) {
  const [pairingCode, setPairingCode] = useState("");
  const [deviceName, setDeviceName] = useState("OpenFlow desktop");
  const [pairing, setPairing] = useState(false);
  const [approvalCode, setApprovalCode] = useState<string | null>(null);
  const connect = async () => {
    try {
      await onCredentialCommit(settings.serverUrl, settings.authToken);
      await controller.connect();
      onNotice("Inference server connected");
    } catch (error) {
      onNotice(error instanceof Error ? error.message : "Could not connect to the inference server");
    }
  };
  const pair = async () => {
    setPairing(true);
    try {
      const token = await controller.pair(pairingCode, deviceName);
      await onCredentialCommit(settings.serverUrl, token);
      onSettingsChange({ ...settings, authToken: token });
      setPairingCode("");
      onNotice("Device paired; connecting with its revocable credential");
    } catch (error) {
      onNotice(error instanceof Error ? error.message : "Could not pair this device");
    } finally {
      setPairing(false);
    }
  };
  const requestApproval = async () => {
    setPairing(true);
    try {
      const verificationCode = randomVerificationCode();
      setApprovalCode(verificationCode);
      const token = await controller.pairInteractively(deviceName, verificationCode);
      await onCredentialCommit(settings.serverUrl, token);
      onSettingsChange({ ...settings, authToken: token });
      onNotice("Device approved and paired; its credential will survive restarts");
    } catch (error) {
      onNotice(error instanceof Error ? error.message : "The server did not approve this device");
    } finally {
      setPairing(false);
      setApprovalCode(null);
    }
  };
  return (
    <section className="page-content narrow-page">
      <div className="page-heading compact">
        <div>
          <span className="eyebrow">INFERENCE SERVER</span>
          <h1>Connection</h1>
          <p>Run models here or securely connect to a more powerful computer.</p>
        </div>
      </div>

      <div className="segmented" role="radiogroup" aria-label="Connection mode">
        <button
          className={settings.connectionMode === "local" ? "active" : ""}
          type="button"
          disabled={pairing}
          onClick={() =>
            onSettingsChange({
              ...settings,
              connectionMode: "local",
              serverUrl: "http://127.0.0.1:8765",
              authToken: "",
            })
          }
        >
          <Server size={17} />
          <span>
            <strong>This computer</strong>
            <small>Default</small>
          </span>
        </button>
        <button
          className={settings.connectionMode === "remote" ? "active" : ""}
          type="button"
          disabled={pairing}
          onClick={() => onSettingsChange({ ...settings, connectionMode: "remote", authToken: "" })}
        >
          <Network size={17} />
          <span>
            <strong>Remote server</strong>
            <small>Advanced</small>
          </span>
        </button>
      </div>

      <div className="settings-card">
        <div className="settings-card-title">
          <div className="stat-icon mint">
            <Link size={18} />
          </div>
          <div>
            <h2>{settings.connectionMode === "local" ? "Local service" : "Remote endpoint"}</h2>
            <p>
              {settings.connectionMode === "local"
                ? "OpenFlow starts the bundled server on loopback, or connects to your existing service."
                : "Use a Tailscale HTTPS name or a TLS-protected server."}
            </p>
          </div>
        </div>
        <label className="field">
          <span>Server URL</span>
          <input
            value={settings.serverUrl}
            disabled={pairing}
            spellCheck={false}
            placeholder="https://openflow.tailnet-name.ts.net"
            onChange={(event) =>
              onSettingsChange({ ...settings, serverUrl: event.target.value, authToken: "" })
            }
          />
        </label>
        {settings.connectionMode === "remote" && (
          <div className="pairing-fields">
            <label className="field">
              <span>Device name</span>
              <input
                value={deviceName}
                disabled={pairing}
                onChange={(event) => setDeviceName(event.target.value)}
              />
            </label>
            {approvalCode && (
              <div className="pairing-verification" role="status" aria-live="polite">
                <span>Confirm this code in the server terminal</span>
                <strong>{approvalCode}</strong>
                <small>Only approve if both screens show the same code and device name.</small>
              </div>
            )}
            <button
              className="primary-button"
              type="button"
              disabled={pairing || !deviceName.trim()}
              onClick={() => void requestApproval()}
            >
              <ShieldCheck size={16} /> {approvalCode ? "Waiting for server…" : "Request server approval"}
            </button>
            <div className="pairing-separator" role="separator">
              <span>or use an administrator-created code</span>
            </div>
            <label className="field">
              <span>One-time pairing code</span>
              <input
                value={pairingCode}
                disabled={pairing}
                spellCheck={false}
                autoComplete="off"
                placeholder="Generated by the server administrator"
                onChange={(event) => setPairingCode(event.target.value)}
              />
            </label>
            <button
              className="secondary-button"
              type="button"
              disabled={pairing || !pairingCode.trim() || !deviceName.trim()}
              onClick={() => void pair()}
            >
              <ShieldCheck size={16} /> {pairing && !approvalCode ? "Pairing…" : "Pair with code"}
            </button>
          </div>
        )}
        <label className="field">
          <span>
            Device or admin token <small>required</small>
          </span>
          <input
            type="password"
            value={settings.authToken}
            placeholder="Paste the local bootstrap or paired-device token"
            onChange={(event) => onSettingsChange({ ...settings, authToken: event.target.value })}
          />
        </label>
        {settings.connectionMode === "remote" && !settings.serverUrl.startsWith("https://") && (
          <div className="inline-alert danger">
            <AlertTriangle size={16} />
            <div>
              <strong>TLS required</strong>
              <span>
                Non-loopback remote connections should use HTTPS/WSS. The server is expected to reject
                plaintext binds.
              </span>
            </div>
          </div>
        )}
        <div className="button-row">
          {controller.connection === "connected" ? (
            <button className="secondary-button" type="button" onClick={controller.disconnect}>
              <Unplug size={16} /> Disconnect
            </button>
          ) : (
            <button
              className="primary-button"
              type="button"
              disabled={controller.connection === "connecting"}
              onClick={() => void connect()}
            >
              <PlugZap size={16} />
              {controller.connection === "connecting" ? "Connecting…" : "Test & connect"}
            </button>
          )}
        </div>
      </div>

      {controller.connectionError && (
        <div className="inline-alert danger">
          <AlertTriangle size={17} />
          <div>
            <strong>Could not connect</strong>
            <span>{controller.connectionError}</span>
          </div>
        </div>
      )}
      {controller.capabilities && (
        <div className="hardware-card">
          <div className="hardware-status">
            <CheckCircle2 size={19} />
            <div>
              <strong>Server is ready</strong>
              <span>
                Protocol v{controller.capabilities.protocolVersion} · Server{" "}
                {controller.capabilities.serverVersion}
              </span>
            </div>
          </div>
          <div className="hardware-grid">
            <div>
              <span>Accelerator</span>
              <strong>{controller.capabilities.hardware.deviceName}</strong>
            </div>
            <div>
              <span>Backend</span>
              <strong>{controller.capabilities.hardware.backend.toUpperCase()}</strong>
            </div>
            <div>
              <span>Reported memory</span>
              <strong>{formatBytes(controller.capabilities.hardware.availableMemoryBytes)}</strong>
            </div>
            <div>
              <span>Sessions</span>
              <strong>
                {controller.capabilities.activeSessions} / {controller.capabilities.maxConcurrentSessions}
              </strong>
            </div>
          </div>
        </div>
      )}
      <div className="security-card">
        <ShieldCheck size={20} />
        <div>
          <strong>The server never controls your keyboard.</strong>
          <p>
            Only audio and transcript events cross the connection. Target verification, clipboard access, and
            text insertion stay on this device.
          </p>
        </div>
      </div>
    </section>
  );
}

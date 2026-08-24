import { Clipboard, Command, Eye, Info, Keyboard, Languages, RotateCcw, ShieldAlert } from "lucide-react";
import type { DesktopSettings } from "../domain/settings";
import type { PlatformCapabilities } from "../lib/bridge";

interface PreferencesPanelProps {
  settings: DesktopSettings;
  platform: PlatformCapabilities | null;
  hotkeyError: string | null;
  onSettingsChange: (settings: DesktopSettings) => void;
  onReset: () => void;
}

function Toggle({
  checked,
  label,
  description,
  onChange,
}: {
  checked: boolean;
  label: string;
  description: string;
  onChange: (value: boolean) => void;
}) {
  return (
    <label className="toggle-row">
      <div>
        <strong>{label}</strong>
        <span>{description}</span>
      </div>
      <input type="checkbox" checked={checked} onChange={(event) => onChange(event.target.checked)} />
      <i />
    </label>
  );
}

export function PreferencesPanel({
  settings,
  platform,
  hotkeyError,
  onSettingsChange,
  onReset,
}: PreferencesPanelProps) {
  return (
    <section className="page-content narrow-page">
      <div className="page-heading compact">
        <div>
          <span className="eyebrow">DESKTOP CLIENT</span>
          <h1>Preferences</h1>
          <p>Control capture, feedback, and safe fallback behavior.</p>
        </div>
        <button className="ghost-button" type="button" onClick={onReset}>
          <RotateCcw size={15} /> Reset defaults
        </button>
      </div>
      <div className="settings-card">
        <div className="settings-card-title">
          <div className="stat-icon amber">
            <Keyboard size={18} />
          </div>
          <div>
            <h2>Global shortcut</h2>
            <p>The same shortcut starts and stops dictation.</p>
          </div>
        </div>
        <label className="field">
          <span>Accelerator</span>
          <div className="input-with-icon">
            <Command size={16} />
            <input
              value={settings.hotkey}
              spellCheck={false}
              onChange={(event) => onSettingsChange({ ...settings, hotkey: event.target.value })}
            />
          </div>
          <small>Use Tauri accelerator syntax, for example CommandOrControl+Shift+Space.</small>
        </label>
        {hotkeyError && (
          <div className="inline-alert danger">
            <ShieldAlert size={16} />
            <div>
              <strong>Shortcut unavailable</strong>
              <span>{hotkeyError}</span>
            </div>
          </div>
        )}
      </div>
      <div className="settings-card">
        <div className="settings-card-title">
          <div className="stat-icon mint">
            <Languages size={18} />
          </div>
          <div>
            <h2>Recognition</h2>
            <p>Auto-detection is best for multilingual dictation.</p>
          </div>
        </div>
        <label className="field">
          <span>Spoken language</span>
          <select
            value={settings.language}
            onChange={(event) => onSettingsChange({ ...settings, language: event.target.value })}
          >
            <option value="auto">Auto detect</option>
            <option value="en">English</option>
            <option value="es">Spanish</option>
            <option value="fr">French</option>
            <option value="de">German</option>
            <option value="zh">Mandarin Chinese</option>
            <option value="ja">Japanese</option>
          </select>
        </label>
      </div>
      <div className="settings-card toggle-card">
        <Toggle
          checked={settings.showOverlay}
          label="Live transcript overlay"
          description="Show stable text while speaking."
          onChange={(showOverlay) => onSettingsChange({ ...settings, showOverlay })}
        />
        <Toggle
          checked={settings.copyFallback}
          label="Copy unsupported targets"
          description="Place final text on the clipboard when direct insertion is unsafe."
          onChange={(copyFallback) => onSettingsChange({ ...settings, copyFallback })}
        />
      </div>
      <div className="platform-card">
        <Info size={18} />
        <div>
          <strong>
            {platform ? `${platform.platform} · ${platform.sessionType}` : "Detecting desktop capabilities…"}
          </strong>
          <p>{platform?.reason ?? "OpenFlow is checking whether the focused application can be verified."}</p>
          <span className={`policy-label ${platform?.policy ?? "blocked"}`}>
            {platform?.policy === "direct" ? (
              <>
                <Eye size={13} /> Verified direct typing
              </>
            ) : (
              <>
                <Clipboard size={13} /> Overlay + clipboard
              </>
            )}
          </span>
        </div>
      </div>
    </section>
  );
}

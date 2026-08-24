import { AudioLines, Cpu, Radio, Settings2, SlidersHorizontal } from "lucide-react";

export type Page = "dictation" | "models" | "connection" | "preferences";

interface SidebarProps {
  page: Page;
  onPageChange: (page: Page) => void;
  connection: "disconnected" | "connecting" | "connected" | "error";
}

const items: { id: Page; label: string; icon: typeof AudioLines }[] = [
  { id: "dictation", label: "Dictation", icon: AudioLines },
  { id: "models", label: "Models", icon: Cpu },
  { id: "connection", label: "Connection", icon: Radio },
  { id: "preferences", label: "Preferences", icon: SlidersHorizontal },
];

export function Sidebar({ page, onPageChange, connection }: SidebarProps) {
  return (
    <aside className="sidebar">
      <div className="brand">
        <div className="brand-mark" aria-hidden="true">
          <AudioLines size={20} />
        </div>
        <div>
          <strong>OpenFlow</strong>
          <span>Local dictation</span>
        </div>
      </div>

      <nav aria-label="Primary navigation">
        {items.map((item) => {
          const Icon = item.icon;
          return (
            <button
              className={`nav-item ${page === item.id ? "active" : ""}`}
              key={item.id}
              onClick={() => onPageChange(item.id)}
              type="button"
            >
              <Icon size={18} />
              <span>{item.label}</span>
            </button>
          );
        })}
      </nav>

      <div className="sidebar-footer">
        <div className={`connection-pill ${connection}`}>
          <span className="status-dot" />
          {connection === "connected"
            ? "Server online"
            : connection === "connecting"
              ? "Connecting"
              : "Server offline"}
        </div>
        <button className="icon-link" type="button" onClick={() => onPageChange("preferences")}>
          <Settings2 size={15} /> Open settings
        </button>
      </div>
    </aside>
  );
}

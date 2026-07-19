/**
 * SettingsPanel — floating overlay with tabbed config.
 *
 * Tabs: General (thinking/effort/mode/theme) | Models | MCP | Preferences
 */
import { useState } from "react";
import type { AppConfig, ProviderSummary, McpServer, Effort } from "../types";

interface SettingsPanelProps {
  config: AppConfig;
  providers: ProviderSummary[];
  mcpServers: McpServer[];
  thinkingEnabled: boolean;
  onToggleThinking: () => void;
  effort: Effort;
  onEffortChange: (v: Effort) => void;
  effortLevels: string[];
  theme: "dark" | "light";
  onThemeChange: (theme: "dark" | "light") => void;
  onSave?: (config: AppConfig) => void;
  onNewSession: () => void;
  onClose: () => void;
}

type SettingsTab = "general" | "models" | "mcp" | "prefs";

export default function SettingsPanel({
  config,
  providers,
  mcpServers,
  thinkingEnabled,
  onToggleThinking,
  effort,
  onEffortChange,
  effortLevels,
  theme,
  onThemeChange,
  onSave,
  onNewSession,
  onClose,
}: SettingsPanelProps) {
  const [tab, setTab] = useState<SettingsTab>("general");
  const [local, setLocal] = useState<AppConfig>(config);
  const [saved, setSaved] = useState(false);

  const update = (patch: Partial<AppConfig>) => { setLocal({ ...local, ...patch }); setSaved(false); };
  const handleSave = () => { onSave?.(local); setSaved(true); setTimeout(() => setSaved(false), 2000); };

  return (
    <>
      <div onClick={onClose} style={{ position: "fixed", inset: 0, zIndex: 90 }} aria-hidden="true" />
      <div className="dp-settings dp-settings-wide" role="dialog" aria-label="Settings">
        <div className="dp-settings-header">
          <p className="title">Settings</p>
          <button className="dp-iconbtn" onClick={onClose} aria-label="Close">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <path d="M18 6L6 18M6 6l12 12" />
            </svg>
          </button>
        </div>

        <div className="dp-settings-tabs">
          <button className={`dp-settings-tab ${tab === "general" ? "active" : ""}`} onClick={() => setTab("general")}>General</button>
          <button className={`dp-settings-tab ${tab === "models" ? "active" : ""}`} onClick={() => setTab("models")}>Models</button>
          <button className={`dp-settings-tab ${tab === "mcp" ? "active" : ""}`} onClick={() => setTab("mcp")}>MCP</button>
          <button className={`dp-settings-tab ${tab === "prefs" ? "active" : ""}`} onClick={() => setTab("prefs")}>Preferences</button>
        </div>

        <div className="dp-settings-body">
          {tab === "general" && (
            <>
              <div className="field">
                <span className="lbl">Thinking mode</span>
                <button className="toggle" data-on={thinkingEnabled} onClick={onToggleThinking} aria-label="Toggle thinking mode" />
              </div>
              <div className="field">
                <span className="lbl">Effort</span>
                <select value={effort} onChange={(e) => onEffortChange(e.target.value as Effort)}>
                  {effortLevels.map((l) => <option key={l} value={l}>{l}</option>)}
                </select>
              </div>
              <div className="field">
                <span className="lbl">Theme</span>
                <div style={{ display: "flex", gap: 4 }}>
                  {(["dark", "light"] as const).map((t) => (
                    <button key={t} className={`dp-btn${theme === t ? " primary" : ""}`} style={{ padding: "4px 10px", fontSize: 12 }} onClick={() => onThemeChange(t)}>
                      {t === "dark" ? "Dark" : "Light"}
                    </button>
                  ))}
                </div>
              </div>
            </>
          )}

          {tab === "models" && (
            <div className="dp-settings-section">
              {providers.length > 0 && (
                <div className="field">
                  <span className="lbl">Connected Providers</span>
                  {providers.map(p => (
                    <div key={p.name} style={{ display: "flex", alignItems: "center", gap: 8, padding: "4px 0", fontSize: 13 }}>
                      <span style={{ width: 8, height: 8, borderRadius: "50%", background: p.connected ? "var(--dp-success)" : "var(--dp-danger)" }} />
                      <span>{p.name}</span>
                      <span style={{ color: "var(--dp-muted)", fontSize: 12 }}>{p.model || "default"}</span>
                    </div>
                  ))}
                </div>
              )}
              <p className="dp-empty" style={{ fontSize: 12, marginTop: 8 }}>
                Configure providers via environment variables or deepnova.toml.
              </p>
            </div>
          )}

          {tab === "mcp" && (
            <div className="dp-settings-section">
              {mcpServers.length > 0 ? (
                mcpServers.map(s => (
                  <div key={s.name} style={{ padding: "8px 0", borderBottom: "1px solid var(--dp-border)" }}>
                    <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                      <span style={{ width: 8, height: 8, borderRadius: "50%", background: s.status === "connected" ? "var(--dp-success)" : s.status === "error" ? "var(--dp-danger)" : "var(--dp-muted)" }} />
                      <strong style={{ fontSize: 13 }}>{s.name}</strong>
                      <span style={{ marginLeft: "auto", fontSize: 11, color: "var(--dp-muted)" }}>{s.tool_count} tools</span>
                    </div>
                    <div style={{ fontSize: 11, color: "var(--dp-muted)", marginTop: 2 }}>{s.command} {s.args.join(" ")}</div>
                    {s.error && <div style={{ fontSize: 10, color: "var(--dp-danger)", marginTop: 2 }}>{s.error}</div>}
                  </div>
                ))
              ) : (
                <p className="dp-empty">No MCP servers configured.</p>
              )}
            </div>
          )}

          {tab === "prefs" && (
            <>
              <div className="field">
                <span className="lbl">Default Mode</span>
                <div style={{ display: "flex", gap: 4 }}>
                  {(["plan", "act", "yolo"] as const).map(m => (
                    <button key={m} className={`dp-btn${local.default_mode === m ? " primary" : ""}`} style={{ padding: "4px 10px", fontSize: 12 }} onClick={() => update({ default_mode: m })}>
                      {m === "plan" ? "Plan" : m === "act" ? "Act" : "YOLO"}
                    </button>
                  ))}
                </div>
              </div>
              <div className="field">
                <span className="lbl">Max Steps: {local.max_steps}</span>
                <input type="range" min="1" max="200" value={local.max_steps} onChange={e => update({ max_steps: parseInt(e.target.value) })} style={{ width: "100%" }} />
              </div>
              <div className="field">
                <span className="lbl">Auto Mode</span>
                <button className="toggle" data-on={local.auto_mode} onClick={() => update({ auto_mode: !local.auto_mode })} aria-label="Toggle auto mode" />
              </div>
              <button className="dp-btn primary" onClick={handleSave} style={{ marginTop: 8 }}>
                {saved ? "✓ Saved" : "Save Preferences"}
              </button>
            </>
          )}
        </div>

        <div className="footer">
          <button className="dp-btn" onClick={onNewSession}>New Session</button>
          <button className="dp-btn primary" onClick={onClose}>Done</button>
        </div>
      </div>
    </>
  );
}

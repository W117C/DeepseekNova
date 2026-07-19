/**
 * SidebarPanel — multi-tab panel (Sessions / Files / Skills / Providers / MCP).
 *
 * Uses dp-* design system classes.
 */
import { useState } from "react";
import type { SessionSummary, SkillSummary, ProviderSummary, McpServer } from "../types";

interface SidebarPanelProps {
  sessions: SessionSummary[];
  skills?: SkillSummary[];
  providers?: ProviderSummary[];
  mcpServers?: McpServer[];
  collapsed?: boolean;
  onNewSession?: () => void;
  onSelectSession?: (id: string) => void;
  running?: boolean;
  messageCount?: number;
}

type Tab = "sessions" | "files" | "skills" | "providers" | "mcp";

export default function SidebarPanel({
  sessions,
  skills = [],
  providers = [],
  mcpServers = [],
  collapsed,
  onNewSession,
  onSelectSession,
  running,
  messageCount,
}: SidebarPanelProps) {
  const [activeTab, setActiveTab] = useState<Tab>("sessions");

  if (collapsed) return null;

  return (
    <aside className="dp-sidebar">
      <div className="head">
        <button
          className="new-btn"
          onClick={onNewSession}
          disabled={running}
          title="New session"
        >
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5">
            <path d="M12 5v14M5 12h14" />
          </svg>
          <span>New chat</span>
        </button>
      </div>

      <div className="dp-tabs" role="tablist">
        <button
          role="tab"
          aria-selected={activeTab === "sessions"}
          className={`tab${activeTab === "sessions" ? " active" : ""}`}
          onClick={() => setActiveTab("sessions")}
        >
          Sessions
        </button>
        <button
          role="tab"
          aria-selected={activeTab === "files"}
          className={`tab${activeTab === "files" ? " active" : ""}`}
          onClick={() => setActiveTab("files")}
        >
          Files
        </button>
        <button
          role="tab"
          aria-selected={activeTab === "skills"}
          className={`tab${activeTab === "skills" ? " active" : ""}`}
          onClick={() => setActiveTab("skills")}
        >
          Skills
        </button>
        <button
          role="tab"
          aria-selected={activeTab === "providers"}
          className={`tab${activeTab === "providers" ? " active" : ""}`}
          onClick={() => setActiveTab("providers")}
        >
          Models
        </button>
        <button
          role="tab"
          aria-selected={activeTab === "mcp"}
          className={`tab${activeTab === "mcp" ? " active" : ""}`}
          onClick={() => setActiveTab("mcp")}
        >
          MCP
        </button>
      </div>

      <div className="dp-list">
        {activeTab === "sessions" &&
          sessions.map((s) => (
            <button
              key={s.id}
              className={`dp-list-item${s.active ? " active" : ""}`}
              onClick={() => onSelectSession?.(s.id)}
            >
              <span className="ico" aria-hidden="true">
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                  <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" />
                </svg>
              </span>
              <span className="label">{s.title}</span>
              {messageCount !== undefined && (
                <span style={{ marginLeft: "auto", fontSize: 11, color: "var(--dp-muted)" }}>
                  {messageCount}
                </span>
              )}
            </button>
          ))}

        {activeTab === "files" && (
          <p className="dp-empty">Workspace file tree will appear here.</p>
        )}

        {activeTab === "skills" &&
          (skills.length > 0 ? (
            skills.map((s) => (
              <button
                key={s.name}
                className="dp-list-item"
                title={s.description}
              >
                <span className="ico" aria-hidden="true">
                  <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                    <path d="M12 2L2 7l10 5 10-5-10-5z" />
                  </svg>
                </span>
                <span className="label">{s.name}</span>
              </button>
            ))
          ) : (
            <p className="dp-empty">No skills loaded.</p>
          ))}

        {activeTab === "providers" &&
          (providers.length > 0 ? (
            providers.map((p) => (
              <div key={p.name} className="dp-list-item" style={{ flexDirection: "column", alignItems: "flex-start", gap: 2 }}>
                <span style={{ display: "flex", alignItems: "center", gap: 8, width: "100%" }}>
                  <span aria-hidden="true" style={{
                    width: 8, height: 8, borderRadius: "50%",
                    background: p.connected ? "var(--dp-success)" : "var(--dp-danger)",
                    boxShadow: p.connected ? "0 0 4px var(--dp-success)" : undefined,
                  }} />
                  <span className="label">{p.name}</span>
                </span>
                <span style={{ fontSize: 11, color: "var(--dp-muted)", paddingLeft: 16 }}>
                  {p.model || "default"}
                </span>
              </div>
            ))
          ) : (
            <p className="dp-empty">No providers configured.</p>
          ))}

        {activeTab === "mcp" &&
          (mcpServers.length > 0 ? (
            mcpServers.map((m) => (
              <div key={m.name} className="dp-list-item" style={{ flexDirection: "column", alignItems: "flex-start", gap: 2 }}>
                <span style={{ display: "flex", alignItems: "center", gap: 8, width: "100%" }}>
                  <span aria-hidden="true" style={{
                    width: 8, height: 8, borderRadius: "50%",
                    background: m.status === "connected" ? "var(--dp-success)" : m.status === "error" ? "var(--dp-danger)" : "var(--dp-muted)",
                  }} />
                  <span className="label">{m.name}</span>
                  <span style={{ marginLeft: "auto", fontSize: 11, color: "var(--dp-muted)" }}>{m.tool_count} tools</span>
                </span>
                {m.error && <span style={{ fontSize: 10, color: "var(--dp-danger)", paddingLeft: 16 }}>{m.error}</span>}
              </div>
            ))
          ) : (
            <p className="dp-empty">No MCP servers configured.</p>
          ))}
      </div>

      <div className="foot">
        <span>DeepseekNova v0.3.0</span>
      </div>
    </aside>
  );
}

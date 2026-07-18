/**
 * Sidebar — Reasonix-style session list with new-session button and workspace info.
 */
import { useState, useEffect } from "react";
import { listSkills } from "../bridge";
import type { SkillSummary } from "../types";

interface SidebarProps {
  collapsed: boolean;
  messageCount: number;
  onNewSession: () => void;
  running: boolean;
  workspaceDir?: string;
}

export default function Sidebar({ collapsed, messageCount, onNewSession, running, workspaceDir }: SidebarProps) {
  const [skills, setSkills] = useState<SkillSummary[]>([]);

  useEffect(() => {
    listSkills().then(setSkills).catch(() => {});
  }, []);

  if (collapsed) return null;

  return (
    <aside className="sidebar">
      <div className="sidebar-header">
        <h3>Sessions</h3>
        <button
          type="button"
          className="btn btn-sm"
          disabled={running}
          onClick={onNewSession}
          title="New session (Cmd+N)"
        >
          + New
        </button>
      </div>

      <div className="sidebar-list">
        {/* Current session */}
        <div className="sidebar-item" data-active="true" title="Current session">
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" />
          </svg>
          <span className="name">Current session</span>
          <span className="count">{messageCount}</span>
        </div>

        {/* Workspace path */}
        {workspaceDir && (
          <div className="sidebar-item" title={workspaceDir} style={{ fontSize: 11, color: "var(--muted)" }}>
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z" />
            </svg>
            <span className="name">{workspaceDir}</span>
          </div>
        )}

        {/* Skills section */}
        {skills.length > 0 && (
          <>
            <div style={{ margin: "12px 12px 4px" }}>
              <h3 style={{ fontSize: 11, fontWeight: 600, color: "var(--muted)", textTransform: "uppercase", letterSpacing: "0.5px" }}>Skills</h3>
            </div>
            {skills.map((s) => (
              <div key={s.name} className="sidebar-item" title={s.description}>
                <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                  <path d="M12 2L2 7l10 5 10-5-10-5z" />
                </svg>
                <span className="name">{s.name}</span>
              </div>
            ))}
          </>
        )}
      </div>
    </aside>
  );
}

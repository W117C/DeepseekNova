/**
 * Sidebar — Reasonix .sidebar > .side-head + .side-workspace + .session-list
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
      {/* New chat button */}
      <div className="side-head">
        <button className="new-btn" onClick={onNewSession} disabled={running} title="New session (Cmd+N)">
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><path d="M12 5v14M5 12h14" /></svg>
          <span>New chat</span>
          <span className="shortcut">&#8984;N</span>
        </button>
      </div>

      {/* Workspace */}
      {workspaceDir && (
        <div className="side-workspace">
          <button className="workspace-btn" title={workspaceDir}>
            <span className="ico">
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z" /></svg>
            </span>
            <span className="name">{workspaceDir}</span>
          </button>
        </div>
      )}

      {/* Sessions list */}
      <div className="session-list">
        <button className="tree-session" data-active="true" title="Current session">
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" /></svg>
          <span className="name">Current session</span>
          <span className="count">{messageCount}</span>
        </button>

        {/* Skills section */}
        {skills.length > 0 && (
          <>
            <div style={{ padding: "8px 8px 4px", fontSize: 11, fontWeight: 600, color: "var(--muted)", textTransform: "uppercase", letterSpacing: "0.5px" }}>Skills</div>
            {skills.map((s) => (
              <button key={s.name} className="tree-session" title={s.description}>
                <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><path d="M12 2L2 7l10 5 10-5-10-5z" /></svg>
                <span className="name">{s.name}</span>
              </button>
            ))}
          </>
        )}
      </div>
    </aside>
  );
}

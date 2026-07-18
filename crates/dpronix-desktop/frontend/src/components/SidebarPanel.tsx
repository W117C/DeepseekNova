/**
 * SidebarPanel — Reasonix UI: tabs (sessions/files/skills) with list items.
 */
import { useState } from "react";
import type { SessionSummary } from "../types";

interface SidebarPanelProps {
  sessions: SessionSummary[];
  onSelectSession?: (id: string) => void;
  filesSlot?: React.ReactNode;
  skillsSlot?: React.ReactNode;
}

type Tab = "sessions" | "files" | "skills";

export default function SidebarPanel({ sessions, onSelectSession, filesSlot, skillsSlot }: SidebarPanelProps) {
  const [activeTab, setActiveTab] = useState<Tab>("sessions");

  return (
    <div className="sidebar">
      <div className="tabs" role="tablist">
        <button role="tab" aria-selected={activeTab === "sessions"} className={`tab${activeTab === "sessions" ? " active" : ""}`} onClick={() => setActiveTab("sessions")}>Sessions</button>
        <button role="tab" aria-selected={activeTab === "files"} className={`tab${activeTab === "files" ? " active" : ""}`} onClick={() => setActiveTab("files")}>Files</button>
        <button role="tab" aria-selected={activeTab === "skills"} className={`tab${activeTab === "skills" ? " active" : ""}`} onClick={() => setActiveTab("skills")}>Skills</button>
      </div>

      {activeTab === "sessions" && (
        <div className="list">
          {sessions.map((s) => (
            <button key={s.id} className={`list-item${s.active ? " active" : ""}`} onClick={() => onSelectSession?.(s.id)}>{s.title}</button>
          ))}
        </div>
      )}

      {activeTab === "files" && (
        <div className="list">
          {filesSlot ?? <p className="empty">Workspace file tree goes here</p>}
        </div>
      )}

      {activeTab === "skills" && (
        <div className="list">
          {skillsSlot ?? <p className="empty">Skills list goes here</p>}
        </div>
      )}
    </div>
  );
}

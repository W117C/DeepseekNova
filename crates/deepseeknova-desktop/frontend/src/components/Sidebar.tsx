/**
 * Sidebar.tsx — 左侧边栏
 * 会话列表（按日期分组）+ 搜索 + 技能库 + 新建会话
 */

import { useState, useMemo } from "react";
import { useStore } from "../store";
import { useI18n } from "../i18n";
import { renameSession } from "../bridge";
import type { SessionSummary } from "../types";

export default function Sidebar() {
  const { t } = useI18n();
  const collapsed = useStore((s) => s.sidebarCollapsed);
  const sessions = useStore((s) => s.sessions);
  const activeSessionId = useStore((s) => s.activeSessionId);
  const setActiveSession = useStore((s) => s.setActiveSession);
  const skills = useStore((s) => s.skills);
  const [searchQuery, setSearchQuery] = useState("");
  const [activeTab, setActiveTab] = useState<"sessions" | "skills">("sessions");

  // 按日期分组
  const grouped = useMemo(() => {
    const now = Date.now();
    const today: SessionSummary[] = [];
    const yesterday: SessionSummary[] = [];
    const earlier: SessionSummary[] = [];

    const filtered = searchQuery
      ? sessions.filter((s) => s.title.toLowerCase().includes(searchQuery.toLowerCase()))
      : sessions;

    for (const s of filtered) {
      // 简单分组：没有时间戳就用默认
      const id = parseInt(s.id) || 0;
      const age = now - id;
      if (age < 86400000) today.push(s);
      else if (age < 172800000) yesterday.push(s);
      else earlier.push(s);
    }
    return { today, yesterday, earlier };
  }, [sessions, searchQuery]);

  if (collapsed) {
    return (
      <aside className="sidebar" style={{ alignItems: "center", padding: "8px 0" }}>
        <button className="btn-icon" title={t("sidebar.newChat")}>
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <line x1="12" y1="5" x2="12" y2="19"/>
            <line x1="5" y1="12" x2="19" y2="12"/>
          </svg>
        </button>
        <button className="btn-icon" title={t("panel.files")} onClick={() => setActiveTab("sessions")}>
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/>
          </svg>
        </button>
        <button className="btn-icon" title={t("settings.skills")} onClick={() => setActiveTab("skills")}>
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <path d="M12 2L2 7l10 5 10-5-10-5z"/><path d="M2 17l10 5 10-5"/>
          </svg>
        </button>
      </aside>
    );
  }

  return (
    <aside className="sidebar">
      {/* 标签页切换 */}
      <div className="tabs">
        <div
          className={`tab ${activeTab === "sessions" ? "active" : ""}`}
          onClick={() => setActiveTab("sessions")}
        >{t("sidebar.sessions")}</div>
        <div
          className={`tab ${activeTab === "skills" ? "active" : ""}`}
          onClick={() => setActiveTab("skills")}
        >{t("sidebar.skills")}</div>
      </div>

      {activeTab === "sessions" ? (
        <>
          {/* 搜索 */}
          <div className="sidebar-search">
            <input
              className="input"
              placeholder={t("sidebar.search")}
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
            />
          </div>

          {/* 新建会话 */}
          <div style={{ padding: "0 12px 8px" }}>
            <button className="btn btn-primary" style={{ width: "100%", justifyContent: "center" }}>
              + {t("sidebar.newChat")}
            </button>
          </div>

          {/* 会话列表 */}
          <div className="sidebar-list">
            {grouped.today.length > 0 && (
              <>
                <div className="date-group">{t("sidebar.today")}</div>
                {grouped.today.map((s) => (
                  <SessionItem
                    key={s.id}
                    session={s}
                    active={s.id === activeSessionId}
                    onClick={() => setActiveSession(s.id)}
                  />
                ))}
              </>
            )}
            {grouped.yesterday.length > 0 && (
              <>
                <div className="date-group">{t("sidebar.yesterday")}</div>
                {grouped.yesterday.map((s) => (
                  <SessionItem
                    key={s.id}
                    session={s}
                    active={s.id === activeSessionId}
                    onClick={() => setActiveSession(s.id)}
                  />
                ))}
              </>
            )}
            {grouped.earlier.length > 0 && (
              <>
                <div className="date-group">{t("sidebar.earlier")}</div>
                {grouped.earlier.map((s) => (
                  <SessionItem
                    key={s.id}
                    session={s}
                    active={s.id === activeSessionId}
                    onClick={() => setActiveSession(s.id)}
                  />
                ))}
              </>
            )}
            {grouped.today.length === 0 && grouped.yesterday.length === 0 && grouped.earlier.length === 0 && (
              <div className="empty-state">
                <div className="empty-state-icon">💬</div>
                <div className="empty-state-text">{t("panel.empty")}</div>
              </div>
            )}
          </div>
        </>
      ) : (
        <>
          {/* 技能库 */}
          <div className="sidebar-list">
            {skills.length === 0 ? (
              <div className="empty-state">
                <div className="empty-state-icon">⚡</div>
                <div className="empty-state-text">暂无已加载技能<br/>在 .deepseeknova/skills/ 创建</div>
              </div>
            ) : (
              skills.map((skill) => (
                <div key={skill.name} className="skill-card-mini" style={{ margin: "4px 8px" }}>
                  <div className="skill-card-mini-name">{skill.name}</div>
                  <div className="skill-card-mini-desc">{skill.description}</div>
                  {skill.tools_allowed.length > 0 && (
                    <div style={{ display: "flex", gap: "4px", marginTop: "4px", flexWrap: "wrap" }}>
                      {skill.tools_allowed.map((t) => (
                        <span key={t} className="tag tag-cyan">{t}</span>
                      ))}
                    </div>
                  )}
                </div>
              ))
            )}
          </div>
        </>
      )}
    </aside>
  );
}

function SessionItem({
  session,
  active,
  onClick,
}: {
  session: SessionSummary;
  active: boolean;
  onClick: () => void;
}) {
  // 双击重命名（rename_session）
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(session.title);
  const setSessions = useStore((s) => s.setSessions);

  const commit = async () => {
    setEditing(false);
    const title = draft.trim();
    if (!title || title === session.title) {
      setDraft(session.title);
      return;
    }
    try {
      await renameSession(session.id, title);
      const sessions = useStore.getState().sessions;
      setSessions(sessions.map((s) => (s.id === session.id ? { ...s, title } : s)));
    } catch (err) {
      console.warn("rename_session failed:", err);
      setDraft(session.title);
    }
  };

  return (
    <div
      className={`sidebar-item ${active ? "active" : ""}`}
      onClick={onClick}
      onDoubleClick={() => { setDraft(session.title); setEditing(true); }}
    >
      {editing ? (
        <input
          className="input"
          style={{ fontSize: 12, padding: "2px 5px" }}
          value={draft}
          autoFocus
          onChange={(e) => setDraft(e.target.value)}
          onBlur={commit}
          onKeyDown={(e) => {
            if (e.key === "Enter") commit();
            if (e.key === "Escape") { setDraft(session.title); setEditing(false); }
          }}
          onClick={(e) => e.stopPropagation()}
        />
      ) : (
        <span className="sidebar-item-title" title="双击重命名">{session.title}</span>
      )}
    </div>
  );
}

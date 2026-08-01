/**
 * TitleBar.tsx — Reasonix 风格多标签顶栏
 * [侧边栏切换] [Logo] [标签1 标签2 +] ─── [⌘P] [右侧面板]
 */

import { useStore } from "../store";
import { useState } from "react";

export default function TitleBar() {
  const capabilities = useStore((s) => s.capabilities);
  const toggleSidebar = useStore((s) => s.toggleSidebar);
  const toggleDrawer = useStore((s) => s.toggleDrawer);
  const sidebarCollapsed = useStore((s) => s.sidebarCollapsed);
  const drawerOpen = useStore((s) => s.drawerOpen);
  const setShowCommandPalette = useStore((s) => s.setShowCommandPalette);
  const setShowSettings = useStore((s) => s.setShowSettings);

  const [tabs, setTabs] = useState([{ id: "1", title: "主会话" }]);
  const [activeTabId, setActiveTabId] = useState("1");

  const addTab = () => {
    const id = String(Date.now());
    setTabs([...tabs, { id, title: "新会话" }]);
    setActiveTabId(id);
  };

  const closeTab = (id: string, e: React.MouseEvent) => {
    e.stopPropagation();
    const next = tabs.filter((t) => t.id !== id);
    setTabs(next.length ? next : [{ id: "1", title: "主会话" }]);
    if (activeTabId === id && next.length) setActiveTabId(next[0].id);
  };

  return (
    <header className="app-header">
      <div className="header-left">
        <button className="btn-icon" onClick={toggleSidebar} title={sidebarCollapsed ? "展开" : "折叠"}>
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <rect x="3" y="3" width="18" height="18" rx="2"/><line x1="9" y1="3" x2="9" y2="21"/>
          </svg>
        </button>
        <span className="header-logo">DeepseekNova</span>
        {capabilities && <span className="header-badge">v{capabilities.version}</span>}
      </div>

      <div className="header-tabs">
        {tabs.map((tab) => (
          <div
            key={tab.id}
            className={`header-tab ${activeTabId === tab.id ? "active" : ""}`}
            onClick={() => setActiveTabId(tab.id)}
          >
            <span className="header-tab-dot" />
            <span className="header-tab-title">{tab.title}</span>
            {tabs.length > 1 && (
              <button className="header-tab-close" onClick={(e) => closeTab(tab.id, e)}>✕</button>
            )}
          </div>
        ))}
        <button className="header-tab-add" onClick={addTab}>+</button>
      </div>

      <div className="header-right">
        <button className="btn btn-ghost" onClick={() => setShowCommandPalette(true)} style={{ fontSize: 11, padding: "2px 8px" }} title="命令面板">
          <span className="icon-only">⌘P</span>
          <span className="text-only">命令</span>
        </button>
        <button className={`btn-icon ${drawerOpen ? "active" : ""}`} onClick={toggleDrawer} title={drawerOpen ? "关闭任务面板" : "任务面板"}>
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <rect x="3" y="3" width="18" height="18" rx="2"/><line x1="15" y1="3" x2="15" y2="21"/>
          </svg>
        </button>
        <button className="btn-icon" onClick={() => setShowSettings(true)} title="设置">
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <circle cx="12" cy="12" r="3"/>
            <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z"/>
          </svg>
        </button>
      </div>
    </header>
  );
}

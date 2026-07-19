/**
 * TitleBar.tsx — Reasonix 风格多标签顶栏
 * [侧边栏切换] [Logo] [标签1 标签2 +] ─── [⌘P] [右侧面板]
 */

import { useStore } from "../store";
import { useState } from "react";

export default function TitleBar() {
  const capabilities = useStore((s) => s.capabilities);
  const toggleSidebar = useStore((s) => s.toggleSidebar);
  const toggleRightPanel = useStore((s) => s.toggleRightPanel);
  const sidebarCollapsed = useStore((s) => s.sidebarCollapsed);
  const rightCollapsed = useStore((s) => s.rightCollapsed);
  const setShowCommandPalette = useStore((s) => s.setShowCommandPalette);

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
        <button className="btn-icon" onClick={toggleRightPanel} title={rightCollapsed ? "展开面板" : "折叠面板"}>
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <rect x="3" y="3" width="18" height="18" rx="2"/><line x1="15" y1="3" x2="15" y2="21"/>
          </svg>
        </button>
      </div>
    </header>
  );
}

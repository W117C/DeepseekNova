/**
 * TitleBar.tsx — 顶部导航栏（多标签版）
 *
 * 参考 Reasonix 的多标签设计：
 * - 每个标签 = 独立会话，可并行处理不同项目
 * - 标签之间完全隔离
 * - 重启后自动恢复
 *
 * 布局：[侧边栏切换] [Logo] [标签页1 标签页2 +] ──── [命令面板] [右侧面板切换]
 */

import { useStore } from "../store";
import { useState } from "react";

interface Tab {
  id: string;
  title: string;
  sessionId?: string;
}

export default function TitleBar() {
  const capabilities = useStore((s) => s.capabilities);
  const toggleSidebar = useStore((s) => s.toggleSidebar);
  const toggleRightPanel = useStore((s) => s.toggleRightPanel);
  const sidebarCollapsed = useStore((s) => s.sidebarCollapsed);
  const rightCollapsed = useStore((s) => s.rightCollapsed);
  const setShowCommandPalette = useStore((s) => s.setShowCommandPalette);

  // 多标签会话（模拟数据，后续接入 store）
  const [tabs, setTabs] = useState<Tab[]>([
    { id: "1", title: "主会话" },
  ]);
  const [activeTabId, setActiveTabId] = useState("1");

  const addTab = () => {
    const id = String(Date.now());
    setTabs([...tabs, { id, title: "新会话" }]);
    setActiveTabId(id);
  };

  const closeTab = (id: string, e: React.MouseEvent) => {
    e.stopPropagation();
    const newTabs = tabs.filter((t) => t.id !== id);
    if (newTabs.length === 0) {
      // 至少保留一个标签
      setTabs([{ id: "1", title: "主会话" }]);
      setActiveTabId("1");
    } else {
      setTabs(newTabs);
      if (activeTabId === id) {
        setActiveTabId(newTabs[0].id);
      }
    }
  };

  return (
    <header className="app-header">
      {/* 左侧：侧边栏切换 + Logo */}
      <div className="header-left">
        <button className="btn-icon" onClick={toggleSidebar} title={sidebarCollapsed ? "展开侧边栏" : "折叠侧边栏"}>
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <rect x="3" y="3" width="18" height="18" rx="2"/>
            <line x1="9" y1="3" x2="9" y2="21"/>
          </svg>
        </button>
        <span className="header-logo">DeepseekNova</span>
        {capabilities && <span className="header-badge">v{capabilities.version}</span>}
      </div>

      {/* 中间：多标签页 */}
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
              <button
                className="header-tab-close"
                onClick={(e) => closeTab(tab.id, e)}
                title="关闭标签"
              >
                ✕
              </button>
            )}
          </div>
        ))}
        <button className="header-tab-add" onClick={addTab} title="新建标签">
          +
        </button>
      </div>

      {/* 右侧：命令面板 + 面板控制 */}
      <div className="header-right">
        <button
          className="btn btn-ghost"
          onClick={() => setShowCommandPalette(true)}
          style={{ fontSize: "12px", padding: "4px 12px" }}
          title="命令面板 (Ctrl+P)"
        >
          <span className="icon-only">⌘P</span>
          <span className="text-only">命令面板</span>
        </button>
        <button className="btn-icon" onClick={toggleRightPanel} title={rightCollapsed ? "展开右侧面板" : "折叠右侧面板"}>
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <rect x="3" y="3" width="18" height="18" rx="2"/>
            <line x1="15" y1="3" x2="15" y2="21"/>
          </svg>
        </button>
      </div>
    </header>
  );
}

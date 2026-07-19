/**
 * TitleBar.tsx — 顶部导航栏（精简版）
 * Logo | 面板控制
 */

import { useStore } from "../store";

export default function TitleBar() {
  const capabilities = useStore((s) => s.capabilities);
  const toggleSidebar = useStore((s) => s.toggleSidebar);
  const toggleRightPanel = useStore((s) => s.toggleRightPanel);
  const sidebarCollapsed = useStore((s) => s.sidebarCollapsed);
  const rightCollapsed = useStore((s) => s.rightCollapsed);
  const setShowCommandPalette = useStore((s) => s.setShowCommandPalette);

  return (
    <header className="app-header">
      <div className="header-left">
        <button className="btn-icon" onClick={toggleSidebar} title={sidebarCollapsed ? "展开侧边栏" : "折叠侧边栏"}>
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><rect x="3" y="3" width="18" height="18" rx="2"/><line x1="9" y1="3" x2="9" y2="21"/></svg>
        </button>
        <span className="header-logo">DeepseekNova</span>
        {capabilities && <span className="header-badge">v{capabilities.version}</span>}
      </div>

      <div className="header-center">
        <button
          className="btn btn-ghost"
          onClick={() => setShowCommandPalette(true)}
          style={{ fontSize: "12px", padding: "4px 12px" }}
          title="命令面板 (Ctrl+P)"
        >
          <span className="icon-only">⌘P</span>
          <span className="text-only">命令面板</span>
        </button>
      </div>

      <div className="header-right">
        <button className="btn-icon" onClick={toggleRightPanel} title={rightCollapsed ? "展开右侧面板" : "折叠右侧面板"}>
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><rect x="3" y="3" width="18" height="18" rx="2"/><line x1="15" y1="3" x2="15" y2="21"/></svg>
        </button>
      </div>
    </header>
  );
}

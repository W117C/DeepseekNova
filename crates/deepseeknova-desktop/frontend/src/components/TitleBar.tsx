/**
 * TitleBar.tsx — 顶部导航栏
 * 项目名 | 模型选择器 | 模式切换 | Effort 切换 | 主题切换 | 面板控制
 */

import { useStore } from "../store";
import { useTheme } from "../store/theme";
import ModelSelector from "./ModelSelector";
import ModeBar from "./ModeBar";
import EffortSwitcher from "./EffortSwitcher";

export default function TitleBar() {
  const capabilities = useStore((s) => s.capabilities);
  const toggleSidebar = useStore((s) => s.toggleSidebar);
  const toggleRightPanel = useStore((s) => s.toggleRightPanel);
  const sidebarCollapsed = useStore((s) => s.sidebarCollapsed);
  const rightCollapsed = useStore((s) => s.rightCollapsed);
  const setShowCommandPalette = useStore((s) => s.setShowCommandPalette);

  const theme = useTheme((s) => s.theme);
  const setTheme = useTheme((s) => s.setTheme);
  const displayMode = useTheme((s) => s.displayMode);
  const toggleDisplayMode = useTheme((s) => s.toggleDisplayMode);

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
        <ModelSelector />
        <ModeBar />
        <EffortSwitcher />
      </div>

      <div className="header-right">
        {/* 主题选择器 */}
        <div className="theme-selector" title="主题模式">
          <button className={`theme-btn ${theme === "light" ? "active" : ""}`} onClick={() => setTheme("light")}>☀️</button>
          <button className={`theme-btn ${theme === "dark" ? "active" : ""}`} onClick={() => setTheme("dark")}>🌙</button>
          <button className={`theme-btn ${theme === "system" ? "active" : ""}`} onClick={() => setTheme("system")}>🖥️</button>
        </div>

        {/* 显示模式切换 */}
        <button className="btn-icon" onClick={toggleDisplayMode} title={displayMode === "icon" ? "切换到文字模式" : "切换到图标模式"}>
          {displayMode === "icon" ? "Aa" : "📦"}
        </button>

        <button className="btn-icon" onClick={() => setShowCommandPalette(true)} title="命令面板 (Ctrl+P)">
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><circle cx="11" cy="11" r="8"/><line x1="21" y1="21" x2="16.65" y2="16.65"/></svg>
        </button>
        <button className="btn-icon" onClick={toggleRightPanel} title={rightCollapsed ? "展开右侧面板" : "折叠右侧面板"}>
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><rect x="3" y="3" width="18" height="18" rx="2"/><line x1="15" y1="3" x2="15" y2="21"/></svg>
        </button>
      </div>
    </header>
  );
}

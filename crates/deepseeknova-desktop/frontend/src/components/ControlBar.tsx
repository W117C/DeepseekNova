/**
 * ControlBar.tsx — 输入区下方控制栏
 * 左：模型 | 模式 | Effort
 * 右：知识库 | 主题 | 显示模式 | 设置
 */

import { useStore } from "../store";
import { useTheme } from "../store/theme";
import ModelSelector from "./ModelSelector";
import ModeBar from "./ModeBar";
import EffortSwitcher from "./EffortSwitcher";

export default function ControlBar() {
  const capabilities = useStore((s) => s.capabilities);
  const setShowSettings = useStore((s) => s.setShowSettings);
  const theme = useTheme((s) => s.theme);
  const setTheme = useTheme((s) => s.setTheme);
  const displayMode = useTheme((s) => s.displayMode);
  const toggleDisplayMode = useTheme((s) => s.toggleDisplayMode);

  return (
    <div className="control-bar">
      <div className="control-bar-left">
        <ModelSelector />
        <div className="control-sep" />
        <ModeBar />
        {capabilities?.supports_reasoning_effort && (
          <>
            <div className="control-sep" />
            <EffortSwitcher />
          </>
        )}
      </div>

      <div className="control-bar-right">
        <div className="theme-selector">
          <button className={`theme-btn ${theme === "light" ? "active" : ""}`} onClick={() => setTheme("light")}>☀️</button>
          <button className={`theme-btn ${theme === "dark" ? "active" : ""}`} onClick={() => setTheme("dark")}>🌙</button>
          <button className={`theme-btn ${theme === "system" ? "active" : ""}`} onClick={() => setTheme("system")}>🖥️</button>
        </div>
        <div className="control-sep" />
        <button className="btn-icon" onClick={toggleDisplayMode} title={displayMode === "icon" ? "文字模式" : "图标模式"}>
          {displayMode === "icon" ? "Aa" : "📦"}
        </button>
        <button className="btn-icon" onClick={() => setShowSettings(true)} title="设置">
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <circle cx="12" cy="12" r="3"/>
            <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z"/>
          </svg>
        </button>
      </div>
    </div>
  );
}

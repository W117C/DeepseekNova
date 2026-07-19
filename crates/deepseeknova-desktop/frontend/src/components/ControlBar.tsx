/**
 * ControlBar.tsx — 输入区下方的控制栏
 * 模型选择器 | 模式切换 | Effort切换 | 知识库 | 主题切换 | 设置
 */

import { useState } from "react";
import { useStore } from "../store";
import { useTheme } from "../store/theme";
import ModelSelector from "./ModelSelector";
import ModeBar from "./ModeBar";
import EffortSwitcher from "./EffortSwitcher";
import KnowledgeBaseModal from "./KnowledgeBaseModal";

export default function ControlBar() {
  const capabilities = useStore((s) => s.capabilities);
  const setShowSettings = useStore((s) => s.setShowSettings);

  const theme = useTheme((s) => s.theme);
  const setTheme = useTheme((s) => s.setTheme);
  const displayMode = useTheme((s) => s.displayMode);
  const toggleDisplayMode = useTheme((s) => s.toggleDisplayMode);

  const [showKB, setShowKB] = useState(false);

  return (
    <>
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
          <div className="control-sep" />
          <button
            className="btn btn-ghost"
            onClick={() => setShowKB(true)}
            style={{ fontSize: "12px", padding: "4px 8px" }}
            title="知识库"
          >
            <span className="icon-only">📚</span>
            <span className="text-only">知识库</span>
          </button>
        </div>

        <div className="control-bar-right">
          {/* 主题选择器 */}
          <div className="theme-selector" title="主题模式">
            <button
              className={`theme-btn ${theme === "light" ? "active" : ""}`}
              onClick={() => setTheme("light")}
              title="浅色"
            >
              ☀️
            </button>
            <button
              className={`theme-btn ${theme === "dark" ? "active" : ""}`}
              onClick={() => setTheme("dark")}
              title="深色"
            >
              🌙
            </button>
            <button
              className={`theme-btn ${theme === "system" ? "active" : ""}`}
              onClick={() => setTheme("system")}
              title="跟随系统"
            >
              🖥️
            </button>
          </div>

          <div className="control-sep" />

          {/* 显示模式切换 */}
          <button
            className="btn-icon"
            onClick={toggleDisplayMode}
            title={displayMode === "icon" ? "切换到文字模式" : "切换到图标模式"}
          >
            {displayMode === "icon" ? "Aa" : "📦"}
          </button>

          {/* 设置 */}
          <button
            className="btn-icon"
            onClick={() => setShowSettings(true)}
            title="设置"
          >
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <circle cx="12" cy="12" r="3"/>
              <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z"/>
            </svg>
          </button>
        </div>
      </div>

      {showKB && <KnowledgeBaseModal onClose={() => setShowKB(false)} />}
    </>
  );
}

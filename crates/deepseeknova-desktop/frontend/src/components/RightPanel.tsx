/**
 * RightPanel.tsx — 右侧面板（参考 Reasonix + Hermes）
 *
 * 改进点：
 * 1. 文件面板分为"读取"和"修改"两个子区域（Reasonix 特色）
 * 2. 增加文件预览能力（Hermes 特色）
 * 3. 标签页：文件 | 上下文 | 记忆 | TODO
 */

import { useStore } from "../store";
import { useState } from "react";
import ContextPanel from "./ContextPanel";
import MemoryPanel from "./MemoryPanel";
import TodoPanel from "./TodoPanel";

export default function RightPanel() {
  const collapsed = useStore((s) => s.rightCollapsed);
  const activeTab = useStore((s) => s.activeRightTab);
  const setActiveTab = useStore((s) => s.setActiveRightTab);

  if (collapsed) return null;

  const tabs = [
    { id: "workspace" as const, label: "文件", icon: "📁" },
    { id: "context" as const, label: "上下文", icon: "🔗" },
    { id: "memory" as const, label: "记忆", icon: "🧠" },
    { id: "todo" as const, label: "TODO", icon: "✓" },
  ];

  return (
    <aside className="right-panel">
      <div className="tabs">
        {tabs.map((t) => (
          <div
            key={t.id}
            className={`tab ${activeTab === t.id ? "active" : ""}`}
            onClick={() => setActiveTab(t.id)}
            title={t.label}
          >
            <span className="icon-only">{t.icon}</span>
            <span className="text-only">{t.label}</span>
          </div>
        ))}
      </div>

      <div style={{ flex: 1, overflow: "hidden" }}>
        {activeTab === "workspace" && <FilePanelWithSections />}
        {activeTab === "context" && <ContextPanel />}
        {activeTab === "memory" && <MemoryPanel />}
        {activeTab === "todo" && <TodoPanel />}
      </div>
    </aside>
  );
}

/**
 * 文件面板 — 分为"读取"和"修改"两个子区域
 * 参考 Reasonix 的文件面板设计
 */
function FilePanelWithSections() {
  const [previewFile, setPreviewFile] = useState<string | null>(null);

  // 模拟数据
  const readFiles = [
    { name: "src/main.rs", path: "src/main.rs", size: "2.4 KB" },
    { name: "Cargo.toml", path: "Cargo.toml", size: "480 B" },
    { name: "README.md", path: "README.md", size: "3.1 KB" },
  ];

  const modifiedFiles = [
    { name: "src/api.rs", path: "src/api.rs", size: "1.8 KB", status: "M" },
    { name: "src/utils.rs", path: "src/utils.rs", size: "920 B", status: "M" },
  ];

  return (
    <div style={{ height: "100%", display: "flex", flexDirection: "column" }}>
      {previewFile ? (
        /* 文件预览模式（Hermes 特色） */
        <div style={{ height: "100%", display: "flex", flexDirection: "column" }}>
          <div className="file-preview-header">
            <button className="btn-icon" onClick={() => setPreviewFile(null)} title="返回">
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                <line x1="19" y1="12" x2="5" y2="12"/>
                <polyline points="12 19 5 12 12 5"/>
              </svg>
            </button>
            <span style={{ fontSize: 12, color: "var(--text-2)", fontFamily: "var(--font-mono)" }}>
              {previewFile}
            </span>
          </div>
          <div className="file-preview-content">
            <pre style={{ fontSize: 12, fontFamily: "var(--font-mono)", color: "var(--text-2)", whiteSpace: "pre-wrap" }}>
              {`// 文件内容预览\n// ${previewFile}\n\nfn main() {\n    println!("Hello, DeepseekNova!");\n}`}
            </pre>
          </div>
        </div>
      ) : (
        /* 文件列表模式 */
        <div style={{ height: "100%", overflowY: "auto" }}>
          {/* 修改的文件（红色标记，更醒目） */}
          <div className="file-section-header">
            <span className="file-section-icon" style={{ color: "var(--red)" }}>✏️</span>
            <span className="file-section-title">修改的文件</span>
            <span className="file-section-count">{modifiedFiles.length}</span>
          </div>
          {modifiedFiles.map((f) => (
            <div
              key={f.path}
              className="file-item file-modified"
              onClick={() => setPreviewFile(f.path)}
            >
              <span className="file-item-status" style={{ color: "var(--amber)" }}>{f.status}</span>
              <span className="file-item-name">{f.name}</span>
              <span className="file-item-size">{f.size}</span>
            </div>
          ))}

          {/* 读取的文件（蓝色标记） */}
          <div className="file-section-header">
            <span className="file-section-icon" style={{ color: "var(--blue)" }}>📖</span>
            <span className="file-section-title">读取的文件</span>
            <span className="file-section-count">{readFiles.length}</span>
          </div>
          {readFiles.map((f) => (
            <div
              key={f.path}
              className="file-item file-read"
              onClick={() => setPreviewFile(f.path)}
            >
              <span className="file-item-status" style={{ color: "var(--blue)" }}>R</span>
              <span className="file-item-name">{f.name}</span>
              <span className="file-item-size">{f.size}</span>
            </div>
          ))}

          {/* Git 状态 */}
          <div className="file-section-header">
            <span className="file-section-icon" style={{ color: "var(--green)" }}>🌿</span>
            <span className="file-section-title">Git 状态</span>
          </div>
          <div style={{ padding: "4px 12px" }}>
            <div className="git-badge">
              <span className="git-branch">main</span>
              <span className="git-dirty">● 2 modified</span>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

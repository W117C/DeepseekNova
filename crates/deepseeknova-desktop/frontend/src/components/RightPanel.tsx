/**
 * RightPanel.tsx — 右侧面板（深度参考 Reasonix）
 *
 * 标签页：文件 | 知识库 | 记忆
 * - 文件：分"修改/读取"两区（Reasonix 特色）+ 三色 token 进度条
 * - 知识库：Repo Wiki + 知识卡片 + 记忆（用户要求的新组件）
 * - 记忆：Reasonix 风格的记忆列表，带类型标签
 */

import { useStore } from "../store";
import { useState } from "react";

export default function RightPanel() {
  const collapsed = useStore((s) => s.rightCollapsed);
  const [activeTab, setActiveTab] = useState<"files" | "knowledge" | "memory">("files");
  const [previewFile, setPreviewFile] = useState<string | null>(null);

  if (collapsed) return null;

  const tabs = [
    { id: "files" as const, label: "文件", icon: "📁" },
    { id: "knowledge" as const, label: "知识库", icon: "📚" },
    { id: "memory" as const, label: "记忆", icon: "🧠" },
  ];

  return (
    <aside className="right-panel">
      {/* 三色 token 进度条（Reasonix 特色） */}
      <div className="token-bar" title="Token 分布：缓存(绿) / 未缓存(黄) / 剩余(灰)">
        <div className="token-bar-cached" style={{ width: "12%" }} />
        <div className="token-bar-uncached" style={{ width: "3%" }} />
        <div className="token-bar-remaining" />
      </div>

      <div className="tabs">
        {tabs.map((t) => (
          <div
            key={t.id}
            className={`tab ${activeTab === t.id ? "active" : ""}`}
            onClick={() => setActiveTab(t.id)}
          >
            <span className="icon-only">{t.icon}</span>
            <span className="text-only">{t.label}</span>
          </div>
        ))}
      </div>

      <div style={{ flex: 1, overflow: "hidden" }}>
        {activeTab === "files" && (
          <FilePanel previewFile={previewFile} setPreviewFile={setPreviewFile} />
        )}
        {activeTab === "knowledge" && <KnowledgePanel />}
        {activeTab === "memory" && <MemoryPanel />}
      </div>
    </aside>
  );
}

/* ============================================================
 * 文件面板 — 分"修改/读取"两区
 * ============================================================ */
function FilePanel({ previewFile, setPreviewFile }: { previewFile: string | null; setPreviewFile: (f: string | null) => void }) {
  const modifiedFiles = [
    { name: "csv_processor.py", size: "1.8 KB", status: "M" },
    { name: "requirements.txt", size: "120 B", status: "M" },
  ];
  const readFiles = [
    { name: "data/sample.csv", size: "4.2 KB", status: "R" },
    { name: "README.md", size: "3.1 KB", status: "R" },
    { name: "Cargo.toml", size: "480 B", status: "R" },
  ];

  if (previewFile) {
    return (
      <div style={{ height: "100%", display: "flex", flexDirection: "column" }}>
        <div className="file-preview-header">
          <button className="btn-icon" onClick={() => setPreviewFile(null)} title="返回">
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <line x1="19" y1="12" x2="5" y2="12"/>
              <polyline points="12 19 5 12 12 5"/>
            </svg>
          </button>
          <span style={{ fontSize: 11, color: "var(--text-2)", fontFamily: "var(--font-mono)" }}>
            {previewFile}
          </span>
        </div>
        <div className="file-preview-content">
          <pre style={{ fontSize: 11, fontFamily: "var(--font-mono)", color: "var(--text-2)", whiteSpace: "pre-wrap" }}>
            {`// ${previewFile}\n\nfn main() {\n    println!("Hello, DeepseekNova!");\n}`}
          </pre>
        </div>
      </div>
    );
  }

  return (
    <div style={{ height: "100%", overflowY: "auto" }}>
      {/* 修改的文件 */}
      <div className="file-section-header">
        <span style={{ color: "var(--amber)" }}>✏️</span>
        <span>修改</span>
        <span className="file-section-count">{modifiedFiles.length}</span>
      </div>
      {modifiedFiles.map((f) => (
        <div key={f.name} className="file-item file-modified" onClick={() => setPreviewFile(f.name)}>
          <span className="file-item-status" style={{ color: "var(--amber)" }}>{f.status}</span>
          <span className="file-item-name">{f.name}</span>
          <span className="file-item-size">{f.size}</span>
        </div>
      ))}

      {/* 读取的文件 */}
      <div className="file-section-header">
        <span style={{ color: "var(--blue)" }}>📖</span>
        <span>读取</span>
        <span className="file-section-count">{readFiles.length}</span>
      </div>
      {readFiles.map((f) => (
        <div key={f.name} className="file-item file-read" onClick={() => setPreviewFile(f.name)}>
          <span className="file-item-status" style={{ color: "var(--blue)" }}>{f.status}</span>
          <span className="file-item-name">{f.name}</span>
          <span className="file-item-size">{f.size}</span>
        </div>
      ))}

      {/* Git 状态 */}
      <div className="file-section-header">
        <span style={{ color: "var(--green)" }}>🌿</span>
        <span>Git</span>
      </div>
      <div style={{ padding: "4px 10px" }}>
        <div className="git-badge">
          <span className="git-branch">main</span>
          <span className="git-dirty">● 2</span>
        </div>
      </div>
    </div>
  );
}

/* ============================================================
 * 知识库面板 — Repo Wiki + 知识卡片 + 记忆
 * ============================================================ */
function KnowledgePanel() {
  const [section, setSection] = useState<"wiki" | "cards" | "memory">("wiki");

  const wikiPages = [
    { title: "项目架构", desc: "Rust + Tauri 2.0 + React 18", icon: "🏗️" },
    { title: "API 文档", desc: "工具调用规范、流式协议", icon: "📡" },
    { title: "开发指南", desc: "环境搭建、构建流程、调试", icon: "📖" },
    { title: "缓存机制", desc: "Prefix-Cache 三层架构", icon: "💡" },
  ];

  const knowledgeCards = [
    { title: "Rust 异步编程", desc: "tokio + async/await 最佳实践", tag: "编程" },
    { title: "Tauri IPC 通信", desc: "前后端通信机制和性能优化", tag: "架构" },
    { title: "React 性能优化", desc: "useMemo、useCallback 和 memo", tag: "前端" },
    { title: "DeepSeek API", desc: "V4 Flash 和 Pro 的选择策略", tag: "AI" },
    { title: "CSS 变量主题", desc: "三套主题的 CSS 实现", tag: "UI" },
  ];

  return (
    <div style={{ height: "100%", display: "flex", flexDirection: "column" }}>
      {/* 子标签 */}
      <div style={{ display: "flex", gap: "4px", padding: "6px 10px", borderBottom: "1px solid var(--border-1)" }}>
        {[
          { id: "wiki" as const, label: "Wiki" },
          { id: "cards" as const, label: "知识卡片" },
          { id: "memory" as const, label: "记忆" },
        ].map((s) => (
          <button
            key={s.id}
            className={`tag ${section === s.id ? "tag-active" : ""}`}
            style={{ cursor: "pointer", padding: "2px 8px" }}
            onClick={() => setSection(s.id)}
          >
            {s.label}
          </button>
        ))}
      </div>

      <div style={{ flex: 1, overflowY: "auto", padding: "8px" }}>
        {section === "wiki" && (
          <>
            <div style={{ fontSize: 10, color: "var(--text-3)", marginBottom: 6, padding: "0 4px" }}>
              自动从代码仓库生成的文档
            </div>
            {wikiPages.map((p) => (
              <div key={p.title} className="kb-card">
                <div className="kb-card-title">
                  {p.icon} {p.title}
                </div>
                <div className="kb-card-desc">{p.desc}</div>
                <div className="kb-card-meta">
                  <span>📄 wiki</span>
                  <span>·</span>
                  <span>最近更新 2 天前</span>
                </div>
              </div>
            ))}
          </>
        )}

        {section === "cards" && (
          <>
            <div style={{ fontSize: 10, color: "var(--text-3)", marginBottom: 6, padding: "0 4px" }}>
              AI 自动提取的知识要点
            </div>
            {knowledgeCards.map((c) => (
              <div key={c.title} className="kb-card">
                <div className="kb-card-title">{c.title}</div>
                <div className="kb-card-desc">{c.desc}</div>
                <div className="kb-card-meta">
                  <span className={`tag tag-cyan`}>{c.tag}</span>
                  <span>·</span>
                  <span>自动提取</span>
                </div>
              </div>
            ))}
            <button className="btn btn-ghost" style={{ width: "100%", justifyContent: "center", marginTop: 4 }}>
              + 手动添加知识卡片
            </button>
          </>
        )}

        {section === "memory" && (
          <>
            <div style={{ fontSize: 10, color: "var(--text-3)", marginBottom: 6, padding: "0 4px" }}>
              跨会话的持久记忆
            </div>
            {[
              { type: "project", text: "项目使用 Rust + Tauri 2.0 构建", tag: "项目" },
              { type: "user", text: "用户偏好使用 VS Code 和深色主题", tag: "用户" },
              { type: "global", text: "DeepSeek V4 Flash 用于日常，Pro 用于复杂推理", tag: "全局" },
              { type: "session", text: "当前会话正在重构前端布局", tag: "会话" },
            ].map((m, i) => (
              <div key={i} className="memory-item">
                <span className={`memory-item-type mem-type-${m.type}`}>{m.tag}</span>
                <span className="memory-item-text">{m.text}</span>
              </div>
            ))}
          </>
        )}
      </div>
    </div>
  );
}

/* ============================================================
 * 记忆面板 — Reasonix 风格
 * ============================================================ */
function MemoryPanel() {
  const memories = [
    { type: "project", text: "项目使用 Rust + Tauri 2.0 构建", tag: "项目" },
    { type: "user", text: "用户偏好 VS Code 和深色主题", tag: "用户" },
    { type: "global", text: "Flash 用于日常编码，Pro 用于复杂推理", tag: "全局" },
    { type: "session", text: "当前会话正在重构前端布局", tag: "会话" },
    { type: "user", text: "用户希望深度参考 Reasonix 设计", tag: "用户" },
  ];

  return (
    <div style={{ height: "100%", overflowY: "auto", padding: "8px" }}>
      {memories.map((m, i) => (
        <div key={i} className="memory-item">
          <span className={`memory-item-type mem-type-${m.type}`}>{m.tag}</span>
          <span className="memory-item-text">{m.text}</span>
        </div>
      ))}
      <button className="btn btn-ghost" style={{ width: "100%", justifyContent: "center", marginTop: 4 }}>
        + 添加记忆
      </button>
    </div>
  );
}

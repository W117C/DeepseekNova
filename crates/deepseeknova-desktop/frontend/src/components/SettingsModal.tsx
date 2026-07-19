/**
 * SettingsModal.tsx — 设置面板
 * API 配置 | 主题 | 技能管理 | 记忆设置 | 快捷键
 */

import { useState } from "react";
import { useStore } from "../store";
import { useTheme } from "../store/theme";

export default function SettingsModal() {
  const setShowSettings = useStore((s) => s.setShowSettings);
  const capabilities = useStore((s) => s.capabilities);
  const skills = useStore((s) => s.skills);
  const [activeSection, setActiveSection] = useState("general");

  const sections = [
    { id: "general", label: "通用", icon: "⚙️" },
    { id: "provider", label: "API", icon: "🔌" },
    { id: "appearance", label: "外观", icon: "🎨" },
    { id: "skills", label: "技能", icon: "⚡" },
    { id: "memory", label: "记忆", icon: "🧠" },
    { id: "shortcuts", label: "快捷键", icon: "⌨️" },
  ];

  const shortcuts = [
    { keys: "Enter", desc: "发送消息" },
    { keys: "Shift+Enter", desc: "换行" },
    { keys: "Ctrl+P", desc: "命令面板" },
    { keys: "/", desc: "Slash 命令" },
    { keys: "Escape", desc: "关闭面板" },
  ];

  return (
    <div className="modal-overlay" onClick={() => setShowSettings(false)}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-header">
          <span className="modal-title">设置</span>
          <button className="btn-icon" onClick={() => setShowSettings(false)}>✕</button>
        </div>

        <div style={{ display: "flex", flex: 1, overflow: "hidden" }}>
          {/* 侧边导航 */}
          <div style={{
            width: "160px",
            borderRight: "1px solid var(--border-1)",
            padding: "8px 0",
            background: "var(--bg-1)",
          }}>
            {sections.map((s) => (
              <div
                key={s.id}
                className={`sidebar-item ${activeSection === s.id ? "active" : ""}`}
                onClick={() => setActiveSection(s.id)}
              >
                <span>{s.icon}</span>
                <span className="sidebar-item-title">{s.label}</span>
              </div>
            ))}
          </div>

          {/* 内容区 */}
          <div className="modal-body">
            {activeSection === "general" && (
              <>
                <h4 style={{ marginBottom: "8px" }}>通用设置</h4>
                {capabilities && (
                  <div className="card" style={{ marginBottom: "8px" }}>
                    <div style={{ fontSize: "12px", color: "var(--text-3)" }}>版本</div>
                    <div style={{ fontSize: "14px" }}>{capabilities.version}</div>
                    <div className="divider" />
                    <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: "4px", fontSize: "12px" }}>
                      <div>思考: {capabilities.supports_thinking ? "✅" : "❌"}</div>
                      <div>推理深度: {capabilities.supports_reasoning_effort ? "✅" : "❌"}</div>
                      <div>工具: {capabilities.supports_tools ? "✅" : "❌"}</div>
                      <div>MCP: {capabilities.supports_mcp ? "✅" : "❌"}</div>
                      <div>图片: {capabilities.supports_images ? "✅" : "❌"}</div>
                      <div>最大步数: {capabilities.max_steps_default}</div>
                    </div>
                  </div>
                )}
                <label className="todo-item">
                  <input type="checkbox" defaultChecked />
                  <span className="todo-text">自动模式切换（简单问题→flash）</span>
                </label>
                <label className="todo-item">
                  <input type="checkbox" defaultChecked />
                  <span className="todo-text">流式输出</span>
                </label>
                <label className="todo-item">
                  <input type="checkbox" />
                  <span className="todo-text">显示推理过程</span>
                </label>
              </>
            )}

            {activeSection === "provider" && (
              <>
                <h4 style={{ marginBottom: "8px" }}>API 配置</h4>
                <div style={{ marginBottom: "8px" }}>
                  <label style={{ fontSize: "12px", color: "var(--text-3)" }}>Provider</label>
                  <input className="input" defaultValue="deepseek" style={{ marginTop: "4px" }} />
                </div>
                <div style={{ marginBottom: "8px" }}>
                  <label style={{ fontSize: "12px", color: "var(--text-3)" }}>API Key</label>
                  <input className="input" type="password" placeholder="sk-..." style={{ marginTop: "4px" }} />
                </div>
                <div style={{ marginBottom: "8px" }}>
                  <label style={{ fontSize: "12px", color: "var(--text-3)" }}>Base URL</label>
                  <input className="input" defaultValue="https://api.deepseek.com/v1" style={{ marginTop: "4px" }} />
                </div>
                <div style={{ marginBottom: "8px" }}>
                  <label style={{ fontSize: "12px", color: "var(--text-3)" }}>默认模型</label>
                  <input className="input" defaultValue="deepseek-chat" style={{ marginTop: "4px" }} />
                </div>
                <button className="btn btn-primary">测试连接</button>
              </>
            )}

            {activeSection === "appearance" && (
              <>
                <h4 style={{ marginBottom: "8px" }}>外观设置</h4>
                <div style={{ marginBottom: "12px" }}>
                  <label style={{ fontSize: "12px", color: "var(--text-3)", display: "block", marginBottom: "6px" }}>主题模式</label>
                  <div style={{ display: "flex", gap: "8px" }}>
                    <button
                      className="btn"
                      style={{ flex: 1, justifyContent: "center" }}
                      onClick={() => useTheme.getState().setTheme("light")}
                    >☀️ 浅色</button>
                    <button
                      className="btn"
                      style={{ flex: 1, justifyContent: "center" }}
                      onClick={() => useTheme.getState().setTheme("dark")}
                    >🌙 深色</button>
                    <button
                      className="btn"
                      style={{ flex: 1, justifyContent: "center" }}
                      onClick={() => useTheme.getState().setTheme("system")}
                    >🖥️ 跟随系统</button>
                  </div>
                </div>
                <div className="divider" />
                <div style={{ marginBottom: "12px" }}>
                  <label style={{ fontSize: "12px", color: "var(--text-3)", display: "block", marginBottom: "6px" }}>显示模式</label>
                  <div style={{ display: "flex", gap: "8px" }}>
                    <button
                      className="btn"
                      style={{ flex: 1, justifyContent: "center" }}
                      onClick={() => useTheme.getState().setDisplayMode("icon")}
                    >📦 图标模式</button>
                    <button
                      className="btn"
                      style={{ flex: 1, justifyContent: "center" }}
                      onClick={() => useTheme.getState().setDisplayMode("text")}
                    >Aa 文字模式</button>
                  </div>
                </div>
                <div className="divider" />
                <label className="todo-item">
                  <input type="checkbox" defaultChecked />
                  <span className="todo-text">平滑过渡动画</span>
                </label>
                <label className="todo-item">
                  <input type="checkbox" defaultChecked />
                  <span className="todo-text">代码块语法高亮</span>
                </label>
                <label className="todo-item">
                  <input type="checkbox" />
                  <span className="todo-text">紧凑模式（减少间距）</span>
                </label>
              </>
            )}

            {activeSection === "skills" && (
              <>
                <h4 style={{ marginBottom: "8px" }}>技能管理</h4>
                {skills.length === 0 ? (
                  <div className="empty-state">
                    <div className="empty-state-icon">⚡</div>
                    <div className="empty-state-text">暂无已加载技能</div>
                  </div>
                ) : (
                  skills.map((skill) => (
                    <div key={skill.name} className="card" style={{ marginBottom: "8px" }}>
                      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between" }}>
                        <span style={{ fontWeight: 600, color: "var(--accent)", fontSize: "13px" }}>
                          {skill.name}
                        </span>
                        <label className="todo-item" style={{ padding: 0 }}>
                          <input type="checkbox" defaultChecked />
                        </label>
                      </div>
                      <div style={{ fontSize: "12px", color: "var(--text-3)", marginTop: "2px" }}>
                        {skill.description}
                      </div>
                    </div>
                  ))
                )}
              </>
            )}

            {activeSection === "memory" && (
              <>
                <h4 style={{ marginBottom: "8px" }}>记忆设置</h4>
                <label className="todo-item">
                  <input type="checkbox" defaultChecked />
                  <span className="todo-text">FTS5 全文检索</span>
                </label>
                <label className="todo-item">
                  <input type="checkbox" defaultChecked />
                  <span className="todo-text">自动记忆提取</span>
                </label>
                <label className="todo-item">
                  <input type="checkbox" defaultChecked />
                  <span className="todo-text">用户画像自动更新</span>
                </label>
                <label className="todo-item">
                  <input type="checkbox" />
                  <span className="todo-text">项目完成后自动蒸馏</span>
                </label>
                <div className="divider" />
                <div style={{ marginBottom: "8px" }}>
                  <label style={{ fontSize: "12px", color: "var(--text-3)" }}>召回数量</label>
                  <input className="input" type="number" defaultValue={5} style={{ marginTop: "4px" }} />
                </div>
              </>
            )}

            {activeSection === "shortcuts" && (
              <>
                <h4 style={{ marginBottom: "8px" }}>快捷键</h4>
                <div style={{ display: "flex", flexDirection: "column", gap: "4px" }}>
                  {shortcuts.map((s) => (
                    <div key={s.keys} className="sidebar-item" style={{ cursor: "default" }}>
                      <code style={{
                        background: "var(--bg-3)",
                        padding: "2px 8px",
                        borderRadius: "4px",
                        fontFamily: "var(--font-mono)",
                        fontSize: "12px",
                        color: "var(--accent)",
                      }}>
                        {s.keys}
                      </code>
                      <span className="sidebar-item-title">{s.desc}</span>
                    </div>
                  ))}
                </div>
              </>
            )}
          </div>
        </div>

        <div className="modal-footer">
          <button className="btn btn-ghost" onClick={() => setShowSettings(false)}>取消</button>
          <button className="btn btn-primary" onClick={() => setShowSettings(false)}>保存</button>
        </div>
      </div>
    </div>
  );
}

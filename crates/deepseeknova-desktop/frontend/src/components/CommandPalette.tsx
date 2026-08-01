/**
 * CommandPalette.tsx — 命令面板（Ctrl+P 触发）
 */

import { useState } from "react";
import { useStore, slashCommands } from "../store";

export default function CommandPalette() {
  const setShowCommandPalette = useStore((s) => s.setShowCommandPalette);
  const setMode = useStore((s) => s.setMode);
  const setEffort = useStore((s) => s.setEffort);
  const clearMessages = useStore((s) => s.clearMessages);
  const setShowSettings = useStore((s) => s.setShowSettings);
  const [query, setQuery] = useState("");

  const commands = [
    ...slashCommands,
    { name: "mode agent", description: "切换到代理模式", action: () => setMode("agent") },
    { name: "mode chat", description: "切换到对话模式", action: () => setMode("chat") },
    { name: "mode plan", description: "切换到规划模式", action: () => setMode("plan") },
    { name: "mode review", description: "切换到审查模式", action: () => setMode("review") },
    { name: "effort low", description: "低推理深度", action: () => setEffort("low") },
    { name: "effort max", description: "最大推理深度", action: () => setEffort("max") },
    { name: "settings", description: "打开设置", action: () => setShowSettings(true) },
    { name: "clear", description: "清空对话", action: () => clearMessages() },
  ];

  const filtered = query
    ? commands.filter((c) => c.name.toLowerCase().includes(query.toLowerCase()))
    : commands;

  return (
    <div className="modal-overlay" onClick={() => setShowCommandPalette(false)}>
      <div
        onClick={(e) => e.stopPropagation()}
        style={{
          width: "500px",
          maxWidth: "90%",
          background: "var(--bg-2)",
          border: "1px solid var(--border-1)",
          borderRadius: "var(--radius-md)",
          boxShadow: "var(--shadow-lg)",
          overflow: "hidden",
          marginTop: "10vh",
        }}
      >
        <input
          autoFocus
          className="input"
          placeholder="输入命令名称…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          style={{
            border: "none",
            borderRadius: 0,
            background: "transparent",
            fontSize: "15px",
            padding: "12px 16px",
          }}
        />
        <div style={{ maxHeight: "300px", overflowY: "auto" }}>
          {filtered.map((cmd) => (
            <div
              key={cmd.name}
              className="sidebar-item"
              onClick={() => {
                cmd.action();
                setShowCommandPalette(false);
              }}
              style={{ padding: "8px 16px" }}
            >
              <span className="slash-item-name" style={{ fontFamily: "var(--font-mono)" }}>
                {cmd.name}
              </span>
              <span className="sidebar-item-title" style={{ marginLeft: "8px" }}>
                {cmd.description}
              </span>
            </div>
          ))}
          {filtered.length === 0 && (
            <div className="empty-state">
              <div className="empty-state-text">未找到命令</div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

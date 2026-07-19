/**
 * ModeBar.tsx — 模式选择器 (Plan / Act / YOLO)
 * 支持图标/文字双模式
 */

import { useStore } from "../store";
import { useTheme } from "../store/theme";
import type { Mode } from "../types";

const modes: { id: Mode; icon: string; label: string; title: string; color: string }[] = [
  { id: "plan", icon: "🔒", label: "Plan", title: "只读审计模式 — 不执行写操作", color: "var(--blue)" },
  { id: "act", icon: "✋", label: "Act", title: "执行模式 — 写操作需要审批", color: "var(--amber)" },
  { id: "yolo", icon: "🚀", label: "YOLO", title: "全自动模式 — 无需审批", color: "var(--red)" },
];

export default function ModeBar() {
  const mode = useStore((s) => s.mode);
  const setMode = useStore((s) => s.setMode);
  const displayMode = useTheme((s) => s.displayMode);
  const isIcon = displayMode === "icon";

  return (
    <div className="mode-selector" title="Agent 执行模式">
      {modes.map((m) => (
        <button
          key={m.id}
          className={`mode-btn ${mode === m.id ? "active" : ""}`}
          onClick={() => setMode(m.id)}
          title={m.title}
          style={mode === m.id ? { background: m.color, color: "white" } : {}}
        >
          <span className={`icon-only`}>{m.icon}</span>
          <span className={`text-only`}>{m.label}</span>
          {isIcon && <span style={{ marginLeft: m.icon ? "2px" : "0" }}>{m.label}</span>}
        </button>
      ))}
    </div>
  );
}

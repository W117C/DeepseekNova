import { memo } from "react";
import type { Mode, Effort, Capabilities } from "../types";

interface Props {
  mode: Mode;
  effort: Effort;
  thinking: boolean;
  autoMode: boolean;
  onModeChange: (m: Mode) => void;
  onEffortChange: (e: Effort) => void;
  onThinkingChange: (v: boolean) => void;
  onAutoModeChange: (v: boolean) => void;
  running: boolean;
  caps: Capabilities | null;
}

const ModeBar = memo(({ mode, effort, thinking, autoMode, onModeChange, onEffortChange, onThinkingChange, onAutoModeChange, running, caps }: Props) => {
  const modes: { key: Mode; label: string; icon: string }[] = [
    { key: "plan", label: "Plan", icon: "🔍" },
    { key: "act", label: "Act", icon: "✏️" },
    { key: "yolo", label: "YOLO", icon: "🚀" },
  ];
  const efforts: Effort[] = ["low", "medium", "high", "max"];

  return (
    <div className="dp-modebar">
      <div className="dp-btn-group">
        {modes.map(m => (
          <button
            key={m.key}
            className={`dp-pill mode-${m.key} ${mode === m.key ? "active" : ""}`}
            onClick={() => onModeChange(m.key)}
            disabled={running}
            title={m.key === "plan" ? "只读探索模式" : m.key === "act" ? "交互模式（需审批）" : "全自动模式"}
          >
            <span className="dp-pill-icon">{m.icon}</span>
            <span className="dp-pill-label">{m.label}</span>
          </button>
        ))}
      </div>

      {caps?.supports_reasoning_effort && (
        <div className="dp-btn-group">
          {efforts.map(e => (
            <button
              key={e}
              className={`dp-pill ${effort === e ? "active" : ""}`}
              onClick={() => onEffortChange(e)}
              disabled={running}
            >
              {e}
            </button>
          ))}
        </div>
      )}

      <div className="dp-btn-group dp-toggles">
        <button
          className={`dp-toggle ${thinking ? "on" : "off"}`}
          onClick={() => onThinkingChange(!thinking)}
          disabled={running}
          title="Thinking 模式"
        >
          🧠
        </button>
        <button
          className={`dp-toggle ${autoMode ? "on" : "off"}`}
          onClick={() => onAutoModeChange(!autoMode)}
          disabled={running}
          title="自动模式"
        >
          ⚡
        </button>
      </div>
    </div>
  );
});

export default ModeBar;

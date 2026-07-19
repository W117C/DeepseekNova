import { useState } from "react";

export default function HooksSettings() {
  const hooks = [
    { event: "on_session_start", command: "echo 'Session started'", enabled: true },
    { event: "on_session_end", command: "git add -A && git stash", enabled: true },
    { event: "on_file_write", command: "prettier --write $FILE", enabled: false },
    { event: "on_tool_call", command: "logger -t deepseeknova 'Tool: $TOOL'", enabled: true },
    { event: "on_error", command: "notify-send 'Error' '$ERROR_MSG'", enabled: false },
    { event: "on_approval_request", command: "paplay /usr/share/sounds/alert.wav", enabled: true },
    { event: "on_task_complete", command: "notify-send 'Done' 'Task completed'", enabled: true },
    { event: "on_budget_exceeded", command: "notify-send 'Budget!' 'Check billing'", enabled: true },
  ];

  const [hookStates, setHookStates] = useState(hooks);

  const events = [
    "on_session_start", "on_session_end", "on_message_send", "on_message_receive",
    "on_file_read", "on_file_write", "on_tool_call", "on_tool_result",
    "on_error", "on_approval_request", "on_task_complete", "on_budget_exceeded",
    "on_model_switch", "on_mode_change", "on_cache_miss",
  ];

  return (
    <div>
      <div style={{ fontSize: 10, color: "var(--text-3)", marginBottom: 8 }}>在特定事件触发时执行自定义命令</div>
      {hookStates.map((h, i) => (
        <div key={i} className="card" style={{ padding: "8px 10px", marginBottom: 6 }}>
          <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 4 }}>
            <span className="tag tag-cyan" style={{ fontFamily: "var(--font-mono)", fontSize: 10 }}>{h.event}</span>
            <label className="toggle-switch" style={{ marginLeft: "auto" }} onClick={(e) => e.stopPropagation()}>
              <input type="checkbox" checked={h.enabled} onChange={() => {
                const next = [...hookStates]; next[i] = { ...next[i], enabled: !next[i].enabled }; setHookStates(next);
              }} />
              <span className="toggle-slider"></span>
            </label>
          </div>
          <div style={{ fontSize: 11, fontFamily: "var(--font-mono)", color: "var(--text-2)", background: "var(--bg-3)", padding: "4px 6px", borderRadius: "var(--radius-sm)" }}>
            $ {h.command}
          </div>
        </div>
      ))}
      <div style={{ marginTop: 12 }}>
        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)", marginBottom: 6 }}>+ 添加新钩子</div>
        <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
          <select className="input" style={{ fontSize: 11 }}>
            {events.map(e => <option key={e} value={e}>{e}</option>)}
          </select>
          <input className="input" placeholder="shell command…" style={{ fontSize: 11, fontFamily: "var(--font-mono)" }} />
          <button className="btn btn-primary" style={{ fontSize: 11 }}>添加钩子</button>
        </div>
      </div>
      <div style={{ marginTop: 12, fontSize: 10, color: "var(--text-muted)" }}>
        可用变量：$FILE, $TOOL, $ERROR_MSG, $SESSION_ID, $MODEL, $MODE, $TOKENS
      </div>
    </div>
  );
}


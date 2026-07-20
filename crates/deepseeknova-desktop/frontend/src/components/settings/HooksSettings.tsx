import { useState, useEffect } from "react";
import { getHooks, setHook, deleteHook, type Hook } from "../../bridge";

export default function HooksSettings() {
  const [hooks, setHooks] = useState<Hook[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [newEvent, setNewEvent] = useState("on_session_start");
  const [newCommand, setNewCommand] = useState("");

  useEffect(() => {
    (async () => {
      try {
        const data = await getHooks();
        setHooks(data);
      } catch (e: any) {
        setError(String(e));
      } finally {
        setLoading(false);
      }
    })();
  }, []);

  const toggleHook = async (event: string) => {
    const hook = hooks.find((h) => h.event === event);
    if (!hook) return;
    const newEnabled = !hook.enabled;
    setHooks((prev) => prev.map((h) => h.event === event ? { ...h, enabled: newEnabled } : h));
    try {
      await setHook(event, hook.command, newEnabled);
    } catch (e: any) {
      setHooks((prev) => prev.map((h) => h.event === event ? { ...h, enabled: !newEnabled } : h));
      setError(`更新失败: ${e}`);
    }
  };

  const updateCommand = async (event: string, command: string) => {
    setHooks((prev) => prev.map((h) => h.event === event ? { ...h, command } : h));
  };

  const saveCommand = async (event: string) => {
    const hook = hooks.find((h) => h.event === event);
    if (!hook) return;
    try {
      await setHook(event, hook.command, hook.enabled);
    } catch (e: any) {
      setError(`保存失败: ${e}`);
    }
  };

  const addHook = async () => {
    if (!newCommand.trim()) return;
    try {
      await setHook(newEvent, newCommand.trim(), true);
      setHooks((prev) => [...prev, { event: newEvent, command: newCommand.trim(), enabled: true }]);
      setNewCommand("");
    } catch (e: any) {
      setError(`添加失败: ${e}`);
    }
  };

  const removeHook = async (event: string) => {
    try {
      await deleteHook(event);
      setHooks((prev) => prev.filter((h) => h.event !== event));
    } catch (e: any) {
      setError(`删除失败: ${e}`);
    }
  };

  const events = [
    "on_session_start", "on_session_end", "on_message_send", "on_message_receive",
    "on_file_read", "on_file_write", "on_tool_call", "on_tool_result",
    "on_error", "on_approval_request", "on_task_complete", "on_budget_exceeded",
    "on_model_switch", "on_mode_change", "on_cache_miss",
  ];

  if (loading) {
    return <div style={{ padding: 20, fontSize: 11, color: "var(--text-3)" }}>加载钩子配置中…</div>;
  }

  return (
    <div>
      {error && (
        <div style={{ fontSize: 10, color: "var(--red)", marginBottom: 8, padding: "4px 8px", background: "var(--bg-3)", borderRadius: 4 }}>
          {error}
        </div>
      )}
      <div style={{ fontSize: 10, color: "var(--text-3)", marginBottom: 8 }}>在特定事件触发时执行自定义命令</div>
      {hooks.map((h) => (
        <div key={h.event} className="card" style={{ padding: "8px 10px", marginBottom: 6 }}>
          <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 4 }}>
            <span className="tag tag-cyan" style={{ fontFamily: "var(--font-mono)", fontSize: 10 }}>{h.event}</span>
            <label className="toggle-switch" style={{ marginLeft: "auto" }} onClick={(e) => e.stopPropagation()}>
              <input
                type="checkbox"
                checked={h.enabled}
                onChange={() => toggleHook(h.event)}
              />
              <span className="toggle-slider"></span>
            </label>
            <button
              className="btn-icon"
              style={{ width: 20, height: 20, color: "var(--red)" }}
              title="删除"
              onClick={() => removeHook(h.event)}
            >✕</button>
          </div>
          <input
            className="input"
            style={{ fontSize: 11, fontFamily: "var(--font-mono)", width: "100%" }}
            value={h.command}
            onChange={(e) => updateCommand(h.event, e.target.value)}
            onBlur={() => saveCommand(h.event)}
          />
        </div>
      ))}
      <div style={{ marginTop: 12 }}>
        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)", marginBottom: 6 }}>+ 添加新钩子</div>
        <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
          <select
            className="input"
            style={{ fontSize: 11 }}
            value={newEvent}
            onChange={(e) => setNewEvent(e.target.value)}
          >
            {events.map((e) => <option key={e} value={e}>{e}</option>)}
          </select>
          <input
            className="input"
            placeholder="shell command…"
            value={newCommand}
            onChange={(e) => setNewCommand(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter") addHook(); }}
            style={{ fontSize: 11, fontFamily: "var(--font-mono)" }}
          />
          <button className="btn btn-primary" style={{ fontSize: 11 }} onClick={addHook}>添加钩子</button>
        </div>
      </div>
      <div style={{ marginTop: 12, fontSize: 10, color: "var(--text-muted)" }}>
        可用变量：$FILE, $TOOL, $ERROR_MSG, $SESSION_ID, $MODEL, $MODE, $TOKENS
      </div>
    </div>
  );
}

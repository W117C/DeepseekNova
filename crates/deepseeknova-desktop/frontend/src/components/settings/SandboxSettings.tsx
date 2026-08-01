import { useState, useEffect } from "react";
import { SettingRow } from "./Shared";
import { getSandboxConfig, setSandboxConfig, type SandboxConfig } from "../../bridge";

export default function SandboxSettings() {
  const [config, setConfig] = useState<SandboxConfig | null>(null);
  const [newAllowed, setNewAllowed] = useState("");
  const [newBlocked, setNewBlocked] = useState("");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    (async () => {
      try {
        const data = await getSandboxConfig();
        setConfig(data);
      } catch (e: any) {
        setError(String(e));
      } finally {
        setLoading(false);
      }
    })();
  }, []);

  const persist = async (newConfig: SandboxConfig) => {
    setConfig(newConfig);
    try {
      await setSandboxConfig(newConfig);
    } catch (e: any) {
      setError(`保存失败: ${e}`);
    }
  };

  if (loading) {
    return <div style={{ padding: 20, fontSize: 11, color: "var(--text-3)" }}>加载沙箱配置中…</div>;
  }

  if (error || !config) {
    return (
      <div>
        <div style={{ fontSize: 10, color: "var(--red)", marginBottom: 8 }}>{error || "配置加载失败"}</div>
        <button className="btn btn-primary" style={{ fontSize: 11 }} onClick={() => window.location.reload()}>重试</button>
      </div>
    );
  }

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
      <SettingRow label="启用目录沙箱" desc="所有工具仅能访问白名单内的目录">
        <label className="toggle-switch">
          <input
            type="checkbox"
            checked={config.enabled}
            onChange={(e) => persist({ ...config, enabled: e.target.checked })}
          />
          <span className="toggle-slider"></span>
        </label>
      </SettingRow>

      <div>
        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)", marginBottom: 6 }}>✅ 允许访问的路径</div>
        {config.allowed_paths.map((p, i) => (
          <div key={i} className="card" style={{ padding: "4px 8px", marginBottom: 4, display: "flex", alignItems: "center" }}>
            <span style={{ fontSize: 11, color: "var(--green)", fontFamily: "var(--font-mono)", flex: 1 }}>{p}</span>
            <button
              className="btn-icon"
              style={{ width: 20, height: 20 }}
              title="移除"
              onClick={() => persist({
                ...config,
                allowed_paths: config.allowed_paths.filter((_, idx) => idx !== i),
              })}
            >✕</button>
          </div>
        ))}
        <div style={{ display: "flex", gap: 4 }}>
          <input
            className="input"
            placeholder="添加路径…"
            value={newAllowed}
            onChange={(e) => setNewAllowed(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && newAllowed.trim()) {
                persist({ ...config, allowed_paths: [...config.allowed_paths, newAllowed.trim()] });
                setNewAllowed("");
              }
            }}
            style={{ flex: 1, fontSize: 11 }}
          />
          <button
            className="btn btn-primary"
            style={{ fontSize: 11 }}
            onClick={() => {
              if (newAllowed.trim()) {
                persist({ ...config, allowed_paths: [...config.allowed_paths, newAllowed.trim()] });
                setNewAllowed("");
              }
            }}
          >添加</button>
        </div>
      </div>

      <div>
        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)", marginBottom: 6 }}>🚫 禁止访问的路径</div>
        {config.blocked_paths.map((p, i) => (
          <div key={i} className="card" style={{ padding: "4px 8px", marginBottom: 4, display: "flex", alignItems: "center" }}>
            <span style={{ fontSize: 11, color: "var(--red)", fontFamily: "var(--font-mono)", flex: 1 }}>{p}</span>
            <button
              className="btn-icon"
              style={{ width: 20, height: 20 }}
              title="移除"
              onClick={() => persist({
                ...config,
                blocked_paths: config.blocked_paths.filter((_, idx) => idx !== i),
              })}
            >✕</button>
          </div>
        ))}
        <div style={{ display: "flex", gap: 4 }}>
          <input
            className="input"
            placeholder="添加路径…"
            value={newBlocked}
            onChange={(e) => setNewBlocked(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && newBlocked.trim()) {
                persist({ ...config, blocked_paths: [...config.blocked_paths, newBlocked.trim()] });
                setNewBlocked("");
              }
            }}
            style={{ flex: 1, fontSize: 11 }}
          />
          <button
            className="btn btn-primary"
            style={{ fontSize: 11 }}
            onClick={() => {
              if (newBlocked.trim()) {
                persist({ ...config, blocked_paths: [...config.blocked_paths, newBlocked.trim()] });
                setNewBlocked("");
              }
            }}
          >添加</button>
        </div>
      </div>

      <SettingRow label="环境变量隔离" desc="隐藏敏感环境变量（API Key 等）">
        <label className="toggle-switch">
          <input
            type="checkbox"
            checked={config.isolate_env}
            onChange={(e) => persist({ ...config, isolate_env: e.target.checked })}
          />
          <span className="toggle-slider"></span>
        </label>
      </SettingRow>
      <SettingRow label="CSP 策略" desc="内容安全策略，防止 XSS">
        <label className="toggle-switch">
          <input
            type="checkbox"
            checked={config.csp_enabled}
            onChange={(e) => persist({ ...config, csp_enabled: e.target.checked })}
          />
          <span className="toggle-slider"></span>
        </label>
      </SettingRow>
    </div>
  );
}

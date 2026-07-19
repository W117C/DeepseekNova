import { useState } from "react";
import { SettingRow } from "./Shared";

export default function SandboxSettings() {
  const [sandboxEnabled, setSandboxEnabled] = useState(true);
  const [allowedPaths] = useState(["/home/user/project", "/tmp"]);
  const [newPath, setNewPath] = useState("");
  const [blockedPaths] = useState(["/etc", "/var", "~/.ssh"]);

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
      <SettingRow label="启用目录沙箱" desc="所有工具仅能访问白名单内的目录">
        <label className="toggle-switch"><input type="checkbox" checked={sandboxEnabled} onChange={(e) => setSandboxEnabled(e.target.checked)} /><span className="toggle-slider"></span></label>
      </SettingRow>

      <div>
        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)", marginBottom: 6 }}>✅ 允许访问的路径</div>
        {allowedPaths.map((p, i) => (
          <div key={i} className="card" style={{ padding: "4px 8px", marginBottom: 4, display: "flex", alignItems: "center" }}>
            <span style={{ fontSize: 11, color: "var(--green)", fontFamily: "var(--font-mono)", flex: 1 }}>{p}</span>
            <button className="btn-icon" style={{ width: 20, height: 20 }} title="移除">✕</button>
          </div>
        ))}
        <div style={{ display: "flex", gap: 4 }}>
          <input className="input" placeholder="添加路径…" value={newPath} onChange={(e) => setNewPath(e.target.value)} style={{ flex: 1, fontSize: 11 }} />
          <button className="btn btn-primary" style={{ fontSize: 11 }}>添加</button>
        </div>
      </div>

      <div>
        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)", marginBottom: 6 }}>🚫 禁止访问的路径</div>
        {blockedPaths.map((p, i) => (
          <div key={i} className="card" style={{ padding: "4px 8px", marginBottom: 4 }}>
            <span style={{ fontSize: 11, color: "var(--red)", fontFamily: "var(--font-mono)" }}>{p}</span>
          </div>
        ))}
      </div>

      <SettingRow label="环境变量隔离" desc="隐藏敏感环境变量（API Key 等）">
        <label className="toggle-switch"><input type="checkbox" defaultChecked /><span className="toggle-slider"></span></label>
      </SettingRow>
      <SettingRow label="CSP 策略" desc="内容安全策略，防止 XSS">
        <label className="toggle-switch"><input type="checkbox" defaultChecked /><span className="toggle-slider"></span></label>
      </SettingRow>
    </div>
  );
}


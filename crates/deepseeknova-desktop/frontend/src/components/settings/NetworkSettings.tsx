import { useState } from "react";
import { SettingRow } from "./Shared";

export default function NetworkSettings() {
  const [proxy, setProxy] = useState("");
  const [timeout, setTimeout] = useState(30);
  const [retry, setRetry] = useState(3);
  const [allowNetwork, setAllowNetwork] = useState(true);

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 0 }}>
      <SettingRow label="允许网络访问" desc="允许 Agent 使用 web_search 等网络工具">
        <label className="toggle-switch"><input type="checkbox" checked={allowNetwork} onChange={(e) => setAllowNetwork(e.target.checked)} /><span className="toggle-slider"></span></label>
      </SettingRow>
      <SettingRow label="代理服务器" desc="HTTP/HTTPS 代理地址">
        <input className="input" placeholder="http://127.0.0.1:7890" value={proxy} onChange={(e) => setProxy(e.target.value)} style={{ width: 260 }} />
      </SettingRow>
      <SettingRow label="请求超时" desc="API 请求超时时间（秒）">
        <input type="number" className="input" value={timeout} onChange={(e) => setTimeout(Number(e.target.value))} style={{ width: 80 }} />
      </SettingRow>
      <SettingRow label="重试次数" desc="网络请求失败重试次数">
        <input type="number" className="input" value={retry} onChange={(e) => setRetry(Number(e.target.value))} style={{ width: 80 }} />
      </SettingRow>
      <SettingRow label="SSL 验证" desc="验证 SSL 证书">
        <label className="toggle-switch"><input type="checkbox" defaultChecked /><span className="toggle-slider"></span></label>
      </SettingRow>
      <SettingRow label="自动重连" desc="网络断开后自动重连">
        <label className="toggle-switch"><input type="checkbox" defaultChecked /><span className="toggle-slider"></span></label>
      </SettingRow>

      <div style={{ marginTop: 12 }}>
        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)", marginBottom: 6 }}>网络诊断</div>
        <div className="card" style={{ padding: 8 }}>
          <div style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 11 }}>
            <span className="status-dot ready" />
            <span style={{ color: "var(--green)" }}>DeepSeek API</span>
            <span style={{ color: "var(--text-muted)" }}>— 128ms</span>
          </div>
          <div style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 11, marginTop: 4 }}>
            <span className="status-dot ready" />
            <span style={{ color: "var(--green)" }}>GitHub API</span>
            <span style={{ color: "var(--text-muted)" }}>— 45ms</span>
          </div>
          <div style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 11, marginTop: 4 }}>
            <span className="status-dot error" />
            <span style={{ color: "var(--red)" }}>MCP: web-search</span>
            <span style={{ color: "var(--text-muted)" }}>— 未连接</span>
          </div>
        </div>
        <button className="btn" style={{ width: "100%", justifyContent: "center", fontSize: 11, marginTop: 6 }}>重新检测</button>
      </div>
    </div>
  );
}


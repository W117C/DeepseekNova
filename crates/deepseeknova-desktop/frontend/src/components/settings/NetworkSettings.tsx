import { useState, useEffect } from "react";
import { SettingRow } from "./Shared";
import { getNetworkConfig, setNetworkConfig, networkDiagnostics, type NetworkConfig } from "../../bridge";

export default function NetworkSettings() {
  const [config, setConfig] = useState<NetworkConfig | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [diagRunning, setDiagRunning] = useState(false);
  const [diagResults, setDiagResults] = useState<any[]>([]);
  const [diagError, setDiagError] = useState<string | null>(null);

  useEffect(() => {
    (async () => {
      try {
        const data = await getNetworkConfig();
        setConfig(data);
      } catch (e: any) {
        setError(String(e));
      } finally {
        setLoading(false);
      }
    })();
  }, []);

  const persist = async (newConfig: NetworkConfig) => {
    setConfig(newConfig);
    setSaving(true);
    try {
      await setNetworkConfig(newConfig);
    } catch (e: any) {
      setError(`保存失败: ${e}`);
    } finally {
      setSaving(false);
    }
  };

  const handleDiagnostics = async () => {
    setDiagRunning(true);
    setDiagError(null);
    try {
      const data = await networkDiagnostics();
      setDiagResults(Array.isArray(data) ? data : data?.results ?? []);
    } catch (e: any) {
      setDiagError(String(e));
    } finally {
      setDiagRunning(false);
    }
  };

  if (loading) {
    return <div style={{ padding: 20, fontSize: 11, color: "var(--text-3)" }}>加载网络配置中…</div>;
  }

  if (error || !config) {
    return (
      <div>
        <div style={{ fontSize: 10, color: "var(--red)", marginBottom: 8 }}>{error || "配置加载失败"}</div>
        <button className="btn btn-primary" style={{ fontSize: 11 }} onClick={() => window.location.reload()}>重试</button>
      </div>
    );
  }

  const statusClass = (s: string) =>
    s === "pass" || s === "ok" ? "ready" : s === "warn" ? "running" : "error";
  const statusColor = (s: string) =>
    s === "pass" || s === "ok" ? "var(--green)" : s === "warn" ? "var(--amber)" : "var(--red)";

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 0 }}>
      <SettingRow label="允许网络访问" desc="允许 Agent 使用 web_search 等网络工具">
        <label className="toggle-switch">
          <input
            type="checkbox"
            checked={config.allow_network}
            onChange={(e) => persist({ ...config, allow_network: e.target.checked })}
          />
          <span className="toggle-slider"></span>
        </label>
      </SettingRow>

      <SettingRow label="代理服务器" desc="HTTP/HTTPS 代理地址">
        <input
          className="input"
          placeholder="http://127.0.0.1:7890"
          value={config.proxy ?? ""}
          onChange={(e) => setConfig({ ...config, proxy: e.target.value || null })}
          onBlur={() => config && persist(config)}
          style={{ width: 260 }}
        />
      </SettingRow>

      <SettingRow label="请求超时" desc="API 请求超时时间（秒）">
        <input
          type="number"
          className="input"
          value={config.timeout_secs}
          onChange={(e) => persist({ ...config, timeout_secs: Number(e.target.value) })}
          style={{ width: 80 }}
        />
      </SettingRow>

      <SettingRow label="重试次数" desc="网络请求失败重试次数">
        <input
          type="number"
          className="input"
          value={config.max_retries}
          onChange={(e) => persist({ ...config, max_retries: Number(e.target.value) })}
          style={{ width: 80 }}
        />
      </SettingRow>

      <SettingRow label="SSL 验证" desc="验证 SSL 证书">
        <label className="toggle-switch">
          <input
            type="checkbox"
            checked={config.ssl_verify}
            onChange={(e) => persist({ ...config, ssl_verify: e.target.checked })}
          />
          <span className="toggle-slider"></span>
        </label>
      </SettingRow>

      <SettingRow label="自动重连" desc="网络断开后自动重连">
        <label className="toggle-switch">
          <input
            type="checkbox"
            checked={config.auto_reconnect}
            onChange={(e) => persist({ ...config, auto_reconnect: e.target.checked })}
          />
          <span className="toggle-slider"></span>
        </label>
      </SettingRow>

      {saving && (
        <div style={{ fontSize: 10, color: "var(--text-3)", padding: "4px 8px" }}>保存中…</div>
      )}

      <div style={{ marginTop: 12 }}>
        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)", marginBottom: 6 }}>网络诊断</div>

        {diagError && (
          <div style={{ fontSize: 10, color: "var(--red)", marginBottom: 8, padding: "4px 8px", background: "var(--bg-3)", borderRadius: 4 }}>
            诊断失败: {diagError}
          </div>
        )}

        {diagResults.length > 0 && (
          <div className="card" style={{ padding: 8, marginBottom: 6 }}>
            {diagResults.map((r, i) => (
              <div key={i} style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 11, marginTop: i > 0 ? 4 : 0 }}>
                <span className={`status-dot ${statusClass(r.status)}`} />
                <span style={{ color: statusColor(r.status) }}>{r.name}</span>
                <span style={{ color: "var(--text-muted)" }}>— {r.detail ?? r.latency ?? ""}</span>
              </div>
            ))}
          </div>
        )}

        {diagResults.length === 0 && !diagError && !diagRunning && (
          <div className="card" style={{ padding: 8, marginBottom: 6, fontSize: 10, color: "var(--text-3)" }}>
            点击"重新检测"运行网络诊断
          </div>
        )}

        <button
          className="btn"
          style={{ width: "100%", justifyContent: "center", fontSize: 11, marginTop: 6 }}
          onClick={handleDiagnostics}
          disabled={diagRunning}
        >
          {diagRunning ? "检测中…" : "重新检测"}
        </button>
      </div>
    </div>
  );
}

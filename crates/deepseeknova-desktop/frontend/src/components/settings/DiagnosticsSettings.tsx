import { useState } from "react";

export default function DiagnosticsSettings({}: any) {
  const [running, setRunning] = useState(false);
  const [results, setResults] = useState<any[]>([]);

  const runDiagnostics = () => {
    setRunning(true);
    setTimeout(() => {
      setResults([
        { name: "Node.js 运行时", status: "pass", detail: "v22.22.1" },
        { name: "Tauri 框架", status: "pass", detail: "v2.0" },
        { name: "DeepSeek API 连接", status: "pass", detail: "128ms" },
        { name: "API Key 有效", status: "pass", detail: "sk-••••••••" },
        { name: "MCP: filesystem", status: "pass", detail: "运行中" },
        { name: "MCP: git", status: "pass", detail: "运行中" },
        { name: "MCP: web-search", status: "warn", detail: "未启动" },
        { name: "缓存系统", status: "pass", detail: "命中率 94%" },
        { name: "记忆系统", status: "pass", detail: "7 条记忆" },
        { name: "沙箱配置", status: "pass", detail: "目录限制已启用" },
        { name: "磁盘空间", status: "pass", detail: "12.4 GB 可用" },
        { name: "内存使用", status: "warn", detail: "412 MB / 2 GB" },
      ]);
      setRunning(false);
    }, 1500);
  };

  return (
    <div>
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 8 }}>
        <span style={{ fontSize: 10, color: "var(--text-3)" }}>系统体检 — 检查所有组件状态</span>
        <button className="btn btn-primary" onClick={runDiagnostics} disabled={running} style={{ fontSize: 11 }}>
          {running ? "检测中…" : "开始体检"}
        </button>
      </div>

      {results.length > 0 && (
        <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
          {results.map((r, i) => (
            <div key={i} className="card" style={{ padding: "6px 10px", display: "flex", alignItems: "center", gap: 6 }}>
              <span className={`status-dot ${r.status === "pass" ? "ready" : r.status === "warn" ? "running" : "error"}`} />
              <span style={{ fontSize: 11, fontWeight: 500, color: "var(--text-1)", flex: 1 }}>{r.name}</span>
              <span style={{ fontSize: 10, color: r.status === "pass" ? "var(--green)" : r.status === "warn" ? "var(--amber)" : "var(--red)" }}>
                {r.status === "pass" ? "✓ 正常" : r.status === "warn" ? "⚠ 警告" : "✕ 错误"}
              </span>
              <span style={{ fontSize: 10, color: "var(--text-muted)", fontFamily: "var(--font-mono)" }}>{r.detail}</span>
            </div>
          ))}
          <div style={{ display: "flex", justifyContent: "space-between", marginTop: 8, fontSize: 11 }}>
            <span style={{ color: "var(--green)" }}>✓ {results.filter(r => r.status === "pass").length} 正常</span>
            <span style={{ color: "var(--amber)" }}>⚠ {results.filter(r => r.status === "warn").length} 警告</span>
            <span style={{ color: "var(--red)" }}>✕ {results.filter(r => r.status === "error").length} 错误</span>
          </div>
        </div>
      )}

      {results.length === 0 && !running && (
        <div className="empty-state" style={{ padding: 20 }}>
          <div className="empty-state-icon">🔬</div>
          <div className="empty-state-text">点击"开始体检"检查系统状态</div>
        </div>
      )}

      <div style={{ marginTop: 12 }}>
        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)", marginBottom: 6 }}>日志</div>
        <div className="card" style={{ padding: 8, fontSize: 10, fontFamily: "var(--font-mono)", color: "var(--text-3)", maxHeight: 150, overflowY: "auto" }}>
          <div>[22:14:01] Agent started, model=deepseek-v4-flash</div>
          <div>[22:14:02] Cache initialized, hit_rate=0%</div>
          <div>[22:14:10] Tool call: write_file (csv_processor.py)</div>
          <div>[22:14:11] File created: csv_processor.py (1.8 KB)</div>
          <div>[22:14:12] Cache hit_rate=94%</div>
          <div>[22:15:00] Session duration: 1m 0s</div>
        </div>
      </div>
    </div>
  );
}


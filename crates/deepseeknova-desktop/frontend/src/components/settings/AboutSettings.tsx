import { useState } from "react";

export default function AboutSettings({ capabilities }: any) {
  const [checking, setChecking] = useState(false);
  const [updateAvailable, setUpdateAvailable] = useState(false);

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 10, alignItems: "center", textAlign: "center" }}>
      <div style={{ fontSize: 32, marginBottom: 4 }}>📋🤖</div>
      <div style={{ fontSize: 16, fontWeight: 700, color: "var(--text-1)" }}>DeepseekNova</div>
      <div style={{ fontSize: 11, color: "var(--text-3)" }}>版本 v{capabilities?.version || "0.3.0"}</div>
      <div style={{ fontSize: 11, color: "var(--text-2)", maxWidth: 320, lineHeight: 1.6, marginTop: 4 }}>
        DeepSeek 原生桌面端 AI 编程助手，围绕 Prefix-Cache 机制深度优化，极致缓存命中率。
      </div>
      <button className="btn btn-primary" onClick={() => { setChecking(true); setTimeout(() => { setChecking(false); setUpdateAvailable(false); }, 2000); }} disabled={checking} style={{ marginTop: 8 }}>
        {checking ? "检查中…" : "检查更新"}
      </button>
      {updateAvailable && <div style={{ fontSize: 11, color: "var(--green)" }}>发现新版本！</div>}
      <div style={{ marginTop: 12, fontSize: 10, color: "var(--text-muted)" }}>
        <div>MIT 协议 · 开源免费</div>
        <div style={{ marginTop: 2 }}>Rust + Tauri 2.0 + React 18</div>
      </div>
      <div style={{ display: "flex", gap: 8, marginTop: 12 }}>
        <a href="https://github.com/W117C/DeepseekNova" target="_blank" style={{ fontSize: 11, color: "var(--accent)", textDecoration: "none" }}>GitHub →</a>
        <span style={{ color: "var(--text-muted)" }}>·</span>
        <a href="#" style={{ fontSize: 11, color: "var(--accent)", textDecoration: "none" }}>文档</a>
        <span style={{ color: "var(--text-muted)" }}>·</span>
        <a href="#" style={{ fontSize: 11, color: "var(--accent)", textDecoration: "none" }}>问题反馈</a>
      </div>
    </div>
  );
}


import { useState } from "react";

export default function MCPSettings() {
  const [servers] = useState([
    { name: "filesystem", command: "npx", args: "@modelcontextprotocol/server-filesystem", transport: "stdio", status: "running" },
    { name: "git", command: "npx", args: "@modelcontextprotocol/server-git", transport: "stdio", status: "running" },
    { name: "shell", command: "npx", args: "@modelcontextprotocol/server-shell", transport: "stdio", status: "running" },
    { name: "web-search", command: "npx", args: "@modelcontextprotocol/server-brave-search", transport: "stdio", status: "stopped" },
    { name: "github", command: "npx", args: "@modelcontextprotocol/server-github", transport: "stdio", status: "stopped" },
    { name: "database", command: "npx", args: "@modelcontextprotocol/server-postgres", transport: "sse", status: "stopped" },
  ]);

  return (
    <div>
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 8 }}>
        <span style={{ fontSize: 10, color: "var(--text-3)" }}>MCP 服务器（{servers.filter(s => s.status === "running").length}/{servers.length} 运行中）</span>
        <button className="btn btn-primary" style={{ fontSize: 11 }}>+ 添加</button>
      </div>

      {servers.map((s) => (
        <div key={s.name} className="card" style={{ padding: "8px 10px", marginBottom: 6 }}>
          <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 4 }}>
            <span style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)" }}>{s.name}</span>
            <span className={`tag ${s.status === "running" ? "tag-green" : ""}`} style={{ marginLeft: "auto", fontSize: 9 }}>
              {s.status === "running" ? "● 运行中" : "○ 已停止"}
            </span>
          </div>
          <div style={{ fontSize: 10, color: "var(--text-3)", fontFamily: "var(--font-mono)", marginBottom: 4 }}>
            {s.command} {s.args}
          </div>
          <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
            <span className="tag tag-blue" style={{ fontSize: 9 }}>{s.transport}</span>
            <button className="btn btn-ghost" style={{ fontSize: 10, padding: "2px 6px" }}>配置</button>
            <button className="btn btn-ghost" style={{ fontSize: 10, padding: "2px 6px" }}>{s.status === "running" ? "停止" : "启动"}</button>
            <button className="btn btn-ghost" style={{ fontSize: 10, padding: "2px 6px", color: "var(--red)" }}>删除</button>
          </div>
        </div>
      ))}

      <div style={{ marginTop: 12 }}>
        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)", marginBottom: 6 }}>支持的传输协议</div>
        <div style={{ display: "flex", gap: 8 }}>
          <div className="card" style={{ padding: "6px 10px", textAlign: "center" }}>
            <div style={{ fontSize: 11, fontWeight: 600, color: "var(--accent)" }}>stdio</div>
            <div style={{ fontSize: 9, color: "var(--text-3)" }}>本地进程</div>
          </div>
          <div className="card" style={{ padding: "6px 10px", textAlign: "center" }}>
            <div style={{ fontSize: 11, fontWeight: 600, color: "var(--accent)" }}>SSE</div>
            <div style={{ fontSize: 9, color: "var(--text-3)" }}>服务端推送</div>
          </div>
          <div className="card" style={{ padding: "6px 10px", textAlign: "center" }}>
            <div style={{ fontSize: 11, fontWeight: 600, color: "var(--accent)" }}>HTTP</div>
            <div style={{ fontSize: 9, color: "var(--text-3)" }}>流式 HTTP</div>
          </div>
        </div>
      </div>
    </div>
  );
}


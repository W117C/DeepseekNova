import { useState, useEffect } from "react";
import {
  listMcpServers,
  addMcpServer,
  removeMcpServer,
  toggleMcpServer,
  type McpServer,
} from "../../bridge";

export default function MCPSettings() {
  const [servers, setServers] = useState<McpServer[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [showAdd, setShowAdd] = useState(false);
  const [newServer, setNewServer] = useState<McpServer>({
    name: "",
    command: "npx",
    args: "",
    transport: "stdio",
    status: "stopped",
  });

  useEffect(() => {
    (async () => {
      try {
        const data = await listMcpServers();
        setServers(data);
      } catch (e: any) {
        setError(String(e));
      } finally {
        setLoading(false);
      }
    })();
  }, []);

  const handleToggle = async (name: string, currentlyRunning: boolean) => {
    const start = !currentlyRunning;
    setServers((prev) => prev.map((s) => s.name === name ? { ...s, status: start ? "running" : "stopped" } : s));
    try {
      await toggleMcpServer(name, start);
    } catch (e: any) {
      setServers((prev) => prev.map((s) => s.name === name ? { ...s, status: start ? "stopped" : "running" } : s));
      setError(`操作失败: ${e}`);
    }
  };

  const handleRemove = async (name: string) => {
    try {
      await removeMcpServer(name);
      setServers((prev) => prev.filter((s) => s.name !== name));
    } catch (e: any) {
      setError(`删除失败: ${e}`);
    }
  };

  const handleAdd = async () => {
    if (!newServer.name.trim()) return;
    try {
      await addMcpServer({ ...newServer, name: newServer.name.trim() });
      setServers((prev) => [...prev, { ...newServer, name: newServer.name.trim() }]);
      setNewServer({ name: "", command: "npx", args: "", transport: "stdio", status: "stopped" });
      setShowAdd(false);
    } catch (e: any) {
      setError(`添加失败: ${e}`);
    }
  };

  if (loading) {
    return <div style={{ padding: 20, fontSize: 11, color: "var(--text-3)" }}>加载 MCP 服务器列表中…</div>;
  }

  return (
    <div>
      {error && (
        <div style={{ fontSize: 10, color: "var(--red)", marginBottom: 8, padding: "4px 8px", background: "var(--bg-3)", borderRadius: 4 }}>
          {error}
        </div>
      )}

      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 8 }}>
        <span style={{ fontSize: 10, color: "var(--text-3)" }}>
          MCP 服务器（{servers.filter((s) => s.status === "running").length}/{servers.length} 运行中）
        </span>
        <button
          className="btn btn-primary"
          style={{ fontSize: 11 }}
          onClick={() => setShowAdd(!showAdd)}
        >+ 添加</button>
      </div>

      {servers.map((s) => (
        <div key={s.name} className="card" style={{ padding: "8px 10px", marginBottom: 6 }}>
          <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 4 }}>
            <span style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)" }}>{s.name}</span>
            <span
              className={`tag ${s.status === "running" ? "tag-green" : ""}`}
              style={{ marginLeft: "auto", fontSize: 9 }}
            >
              {s.status === "running" ? "● 运行中" : "○ 已停止"}
            </span>
          </div>
          <div style={{ fontSize: 10, color: "var(--text-3)", fontFamily: "var(--font-mono)", marginBottom: 4 }}>
            {s.command} {s.args}
          </div>
          <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
            <span className="tag tag-blue" style={{ fontSize: 9 }}>{s.transport}</span>
            <button
              className="btn btn-ghost"
              style={{ fontSize: 10, padding: "2px 6px" }}
              onClick={() => handleToggle(s.name, s.status === "running")}
            >{s.status === "running" ? "停止" : "启动"}</button>
            <button
              className="btn btn-ghost"
              style={{ fontSize: 10, padding: "2px 6px", color: "var(--red)" }}
              onClick={() => handleRemove(s.name)}
            >删除</button>
          </div>
        </div>
      ))}

      {showAdd && (
        <div className="card" style={{ padding: 10, marginBottom: 8 }}>
          <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)", marginBottom: 6 }}>添加 MCP 服务器</div>
          <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
            <input
              className="input"
              placeholder="名称（如 filesystem）"
              value={newServer.name}
              onChange={(e) => setNewServer({ ...newServer, name: e.target.value })}
              style={{ fontSize: 11 }}
            />
            <input
              className="input"
              placeholder="命令（如 npx）"
              value={newServer.command}
              onChange={(e) => setNewServer({ ...newServer, command: e.target.value })}
              style={{ fontSize: 11, fontFamily: "var(--font-mono)" }}
            />
            <input
              className="input"
              placeholder="参数（如 @modelcontextprotocol/server-filesystem）"
              value={newServer.args}
              onChange={(e) => setNewServer({ ...newServer, args: e.target.value })}
              style={{ fontSize: 11, fontFamily: "var(--font-mono)" }}
            />
            <select
              className="input"
              style={{ fontSize: 11 }}
              value={newServer.transport}
              onChange={(e) => setNewServer({ ...newServer, transport: e.target.value })}
            >
              <option value="stdio">stdio</option>
              <option value="sse">SSE</option>
              <option value="http">HTTP</option>
            </select>
            <div style={{ display: "flex", gap: 4 }}>
              <button className="btn btn-primary" style={{ fontSize: 11, flex: 1 }} onClick={handleAdd}>添加</button>
              <button className="btn btn-ghost" style={{ fontSize: 11 }} onClick={() => setShowAdd(false)}>取消</button>
            </div>
          </div>
        </div>
      )}

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



export default function SubAgentsSettings() {
  const agents = [
    { name: "code-reviewer", desc: "代码审查专家", model: "deepseek-v4-pro", status: "idle", tasks: 12 },
    { name: "bug-hunter", desc: "Bug 检测和根因分析", model: "deepseek-v4-pro", status: "idle", tasks: 5 },
    { name: "test-generator", desc: "自动生成测试用例", model: "deepseek-v4-flash", status: "idle", tasks: 8 },
    { name: "refactor-assistant", desc: "代码重构建议", model: "deepseek-v4-pro", status: "running", tasks: 3 },
    { name: "frontend-design", desc: "前端 UI/UX 设计", model: "deepseek-v4-flash", status: "idle", tasks: 7 },
    { name: "doc-generator", desc: "文档生成", model: "deepseek-v4-flash", status: "idle", tasks: 15 },
  ];

  return (
    <div>
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 8 }}>
        <span style={{ fontSize: 10, color: "var(--text-3)" }}>子智能体（{agents.length} 个，{agents.filter(a => a.status === "running").length} 个运行中）</span>
        <button className="btn btn-primary" style={{ fontSize: 11 }}>+ 创建</button>
      </div>

      {agents.map((a) => (
        <div key={a.name} className="card" style={{ padding: "8px 10px", marginBottom: 6 }}>
          <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 4 }}>
            <span style={{ fontSize: 12, fontWeight: 600, color: "var(--accent)", fontFamily: "var(--font-mono)" }}>{a.name}</span>
            <span className={`tag ${a.status === "running" ? "tag-amber" : "tag-green"}`} style={{ marginLeft: "auto", fontSize: 9 }}>
              {a.status === "running" ? "● 运行中" : "● 空闲"}
            </span>
          </div>
          <div style={{ fontSize: 11, color: "var(--text-3)", marginBottom: 4 }}>{a.desc}</div>
          <div style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 10 }}>
            <span className="tag tag-cyan" style={{ fontSize: 9 }}>{a.model}</span>
            <span style={{ color: "var(--text-muted)" }}>·</span>
            <span style={{ color: "var(--text-3)" }}>{a.tasks} 次任务</span>
            <div style={{ marginLeft: "auto", display: "flex", gap: 4 }}>
              <button className="btn btn-ghost" style={{ fontSize: 10, padding: "2px 6px" }}>配置</button>
              <button className="btn btn-ghost" style={{ fontSize: 10, padding: "2px 6px" }}>调用</button>
            </div>
          </div>
        </div>
      ))}
    </div>
  );
}




export default function SkillsSettings({ skills }: { skills: any[] }) {
  const allSkills = skills.length > 0 ? skills : [
    { name: "csv-processor", description: "CSV 文件处理和数据分析", tools_allowed: ["read_file", "write_file"] },
    { name: "git-master", description: "Git 操作和分支管理", tools_allowed: ["run_command", "git_diff"] },
    { name: "api-tester", description: "API 测试和接口文档", tools_allowed: ["web_search", "run_command"] },
    { name: "doc-writer", description: "项目文档自动生成", tools_allowed: ["read_file", "write_file", "list_dir"] },
    { name: "refactor-pro", description: "代码重构和优化建议", tools_allowed: ["read_file", "search_files"] },
  ];

  return (
    <div>
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 8 }}>
        <span style={{ fontSize: 10, color: "var(--text-3)" }}>已安装技能（{allSkills.length} 个）</span>
        <div style={{ display: "flex", gap: 4 }}>
          <button className="btn" style={{ fontSize: 11 }}>📦 技能市场</button>
          <button className="btn btn-primary" style={{ fontSize: 11 }}>+ 创建</button>
        </div>
      </div>

      {allSkills.map((s) => (
        <div key={s.name} className="card" style={{ padding: "8px 10px", marginBottom: 6 }}>
          <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 4 }}>
            <span style={{ fontSize: 12, fontWeight: 600, color: "var(--accent)", fontFamily: "var(--font-mono)" }}>{s.name}</span>
            <div style={{ marginLeft: "auto", display: "flex", gap: 4 }}>
              <button className="btn btn-ghost" style={{ fontSize: 10, padding: "2px 6px" }}>编辑</button>
              <button className="btn btn-ghost" style={{ fontSize: 10, padding: "2px 6px", color: "var(--red)" }}>卸载</button>
            </div>
          </div>
          <div style={{ fontSize: 11, color: "var(--text-3)", marginBottom: 4 }}>{s.description}</div>
          <div style={{ display: "flex", gap: 3, flexWrap: "wrap" }}>
            {s.tools_allowed.map((t: string) => (
              <span key={t} className="tag tag-cyan" style={{ fontSize: 9 }}>{t}</span>
            ))}
          </div>
        </div>
      ))}

      <div style={{ marginTop: 12 }}>
        <div style={{ fontSize: 10, color: "var(--text-3)", lineHeight: 1.6 }}>
          技能使用 Markdown 格式定义，放在 .deepseeknova/skills/ 目录下。兼容 Claude Code 的 .claude/skills/ 格式，跨项目复用。
        </div>
      </div>
    </div>
  );
}


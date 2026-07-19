import { useState } from "react";

export default function PermissionsSettings() {
  const rules = [
    { name: "目录沙箱", desc: "所有工具仅能访问启动时的项目目录", enabled: true, type: "文件" },
    { name: "Plan 模式", desc: "AI 只能读，不能写，必须先提交计划", enabled: false, type: "执行" },
    { name: "Review 审批", desc: "写操作进入审核队列，每次确认", enabled: true, type: "执行" },
    { name: "Shell 命令确认", desc: "所有 Shell 命令都需要用户确认", enabled: true, type: "执行" },
    { name: "自动提交", desc: "Agent 完成任务后自动 git commit", enabled: false, type: "Git" },
    { name: "网络访问", desc: "允许 Agent 访问网络", enabled: true, type: "网络" },
    { name: "文件删除", desc: "允许 Agent 删除文件", enabled: false, type: "文件" },
    { name: "文件大小限制", desc: "单文件读写最大 10MB", enabled: true, type: "限制" },
    { name: "Token 预算", desc: "单会话 Token 上限 500K", enabled: true, type: "限制" },
    { name: "敏感文件保护", desc: "禁止访问 .env、.ssh、.aws 等", enabled: true, type: "安全" },
    { name: "多标签隔离", desc: "标签之间完全隔离，不共享上下文", enabled: true, type: "隔离" },
    { name: "剪贴板访问", desc: "允许 Agent 读取剪贴板内容", enabled: false, type: "隐私" },
  ];

  const [ruleStates, setRuleStates] = useState(rules);

  const toggleRule = (idx: number) => {
    const next = [...ruleStates];
    next[idx] = { ...next[idx], enabled: !next[idx].enabled };
    setRuleStates(next);
  };

  return (
    <div>
      <div style={{ fontSize: 10, color: "var(--text-3)", marginBottom: 8 }}>当前生效的权限配置（共 {ruleStates.length} 条）</div>
      {ruleStates.map((r, i) => (
        <div key={r.name} className="card" style={{ padding: "6px 8px", marginBottom: 4 }}>
          <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
            <span className={`tag ${r.type === "安全" ? "tag-red" : r.type === "执行" ? "tag-amber" : r.type === "Git" ? "tag-cyan" : r.type === "网络" ? "tag-blue" : r.type === "隐私" ? "tag-red" : "tag-blue"}`} style={{ fontSize: 9 }}>{r.type}</span>
            <span style={{ fontSize: 12, fontWeight: 500, color: "var(--text-1)" }}>{r.name}</span>
            <label className="toggle-switch" style={{ marginLeft: "auto" }} onClick={(e) => e.stopPropagation()}>
              <input type="checkbox" checked={r.enabled} onChange={() => toggleRule(i)} />
              <span className="toggle-slider"></span>
            </label>
          </div>
          <div style={{ fontSize: 10, color: "var(--text-3)", marginTop: 3 }}>{r.desc}</div>
        </div>
      ))}
    </div>
  );
}


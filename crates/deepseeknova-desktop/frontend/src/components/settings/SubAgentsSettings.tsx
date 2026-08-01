import { useState, useEffect } from "react";
import { listSubagents } from "../../bridge";

interface AgentInfo {
  name: string;
  desc: string;
  model: string;
  status: string;
  tasks: number;
}

export default function SubAgentsSettings() {
  const [agents, setAgents] = useState<AgentInfo[]>([]);
  const [isMock, setIsMock] = useState(false);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    listSubagents()
      .then((data: any) => {
        if (data?.mock) {
          setIsMock(true);
          setAgents([]);
        } else if (Array.isArray(data)) {
          setAgents(data);
        } else if (data?.agents) {
          setAgents(data.agents);
        }
      })
      .catch(() => setAgents([]))
      .finally(() => setLoading(false));
  }, []);

  const runningCount = agents.filter(a => a.status === "running").length;

  return (
    <div>
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 8 }}>
        <span style={{ fontSize: 10, color: "var(--text-3)" }}>
          子智能体（{agents.length} 个{runningCount > 0 ? `，${runningCount} 个运行中` : ""}）
        </span>
        <button className="btn btn-primary" style={{ fontSize: 11 }} disabled>+ 创建</button>
      </div>

      {isMock && (
        <div style={{ fontSize: 10, color: "var(--amber)", marginBottom: 8, padding: "4px 8px", background: "var(--bg-3)", borderRadius: 4 }}>
          ⚠ 演示模式 — 子智能体状态尚未接入编排层后端
        </div>
      )}

      {loading && (
        <div style={{ textAlign: "center", padding: 20, color: "var(--text-3)", fontSize: 11 }}>加载中…</div>
      )}

      {!loading && agents.length === 0 && !isMock && (
        <div className="empty-state" style={{ padding: 20 }}>
          <div className="empty-state-icon">🤖</div>
          <div className="empty-state-text">暂无子智能体</div>
        </div>
      )}

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

      {/* mockup 定稿新增：协作模式与失败处理 */}
      <div className="setting-row">
        <div style={{ flexShrink: 0 }}>
          <div className="setting-row-label">协作模式</div>
          <div className="setting-row-desc">串行流水线 / 并行投票 / 辩论仲裁</div>
        </div>
        <div className="setting-row-control"><span className="tag">串行流水线</span></div>
      </div>
      <div className="setting-row">
        <div style={{ flexShrink: 0 }}>
          <div className="setting-row-label">失败处理</div>
          <div className="setting-row-desc">子代理失败时的策略</div>
        </div>
        <div className="setting-row-control"><span className="tag">重试 2 次 · 降级跳过</span></div>
      </div>
    </div>
  );
}

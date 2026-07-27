/**
 * TaskDrawer.tsx — 任务抽屉（挤压式，网格第四列，mockup 定稿）
 * Tab「任务」：进度 / 文件变更 / 工作树 / 子智能体 / 编排进度（SVG 圆环）
 * Tab「上下文」：收编原 RightPanel（文件/知识库/记忆/Trace）
 * 编排进度轮询仅在抽屉打开时启动（1Hz），关闭即停。
 */

import { useEffect, useState, type ReactNode } from "react";
import { useStore } from "../store";
import { useI18n } from "../i18n";
import { listSubagents, getOrchProgress } from "../bridge";
import { invokeOrStub, STUB_CHANGED_FILES, STUB_WORKTREES } from "../lib/stubs";
import type { ChangedFile, WorktreeInfo } from "../types";
import RightPanel from "./RightPanel";

/** 折叠分区（默认展开） */
function Section({ label, meta, children, defaultOpen = true }: {
  label: string; meta?: ReactNode; children: ReactNode; defaultOpen?: boolean;
}) {
  const [open, setOpen] = useState(defaultOpen);
  return (
    <div className={`dsec row-fold ${open ? "open" : ""}`}>
      <div className="row-fold-h" onClick={() => setOpen(!open)}>
        <span className="tri">▶</span>
        <span className="lbl">{label}</span>
        <span className="right">{meta && <span className="meta">{meta}</span>}</span>
      </div>
      <div className="row-fold-b">{children}</div>
    </div>
  );
}

/** 进度步骤：由本次 run 的工具消息派生 */
function ProgressSection() {
  const { t } = useI18n();
  const messages = useStore((s) => s.messages);
  const running = useStore((s) => s.running);
  const tools = messages.filter((m) => m.role === "tool");
  const doneCount = tools.filter((m) => !!m.toolResult).length;

  return (
    <Section label={t("drawer.progress")} meta={`${doneCount}/${tools.length}`}>
      {tools.length === 0 && <div className="step">暂无任务步骤</div>}
      {tools.slice(-12).map((m) => {
        const done = !!m.toolResult;
        return (
          <div key={m.id} className={`step ${done ? "done" : running ? "run" : ""}`}>
            <span className="k">
              {done ? "✓" : running ? <span className="ring act" /> : <span className="ring wait" />}
            </span>
            <span className="mono">{m.toolName}</span>
          </div>
        );
      })}
    </Section>
  );
}

/** 文件变更：get_changed_files；点击在 Diff 面板打开 */
function FilesSection() {
  const { t } = useI18n();
  const changedFiles = useStore((s) => s.changedFiles);
  const setActiveDiffFile = useStore((s) => s.setActiveDiffFile);
  const setDiffOpen = useStore((s) => s.setDiffOpen);

  return (
    <Section label={t("drawer.files")} meta={changedFiles.length}>
      {changedFiles.length === 0 && <div className="step">工作区干净</div>}
      {changedFiles.map((f) => (
        <div
          key={f.path}
          className="dfile"
          onClick={() => { setActiveDiffFile(f.path); setDiffOpen(true); }}
        >
          <span className={`ftag ${f.tag.toLowerCase()}`}>{f.tag}</span>
          <span className="fn">{f.path}</span>
          <span className="df">
            {f.additions > 0 && <span className="p">+{f.additions}</span>}{" "}
            {f.deletions > 0 && <span className="n">-{f.deletions}</span>}
          </span>
        </div>
      ))}
    </Section>
  );
}

/** 工作树：list_worktrees */
function WorktreeSection() {
  const { t } = useI18n();
  const worktrees = useStore((s) => s.worktrees);
  return (
    <Section label={t("drawer.worktree")} meta={worktrees.length}>
      {worktrees.length === 0 && <div className="step">无工作树信息</div>}
      {worktrees.map((w) => (
        <div key={w.branch} className={`wt ${w.is_current ? "on" : ""}`}>
          <span className="k">{w.is_current ? "⌥" : "⎇"}</span>
          <span className="fn mono">{w.branch}</span>
          <span className="meta">
            {w.is_current ? "当前" : "主分支"}{w.dirty ? " · 有改动" : " · 干净"}
          </span>
        </div>
      ))}
    </Section>
  );
}

/** 子智能体：list_subagents（真实命令） */
interface SubAgentRow { id: string; name: string; model: string; status: string; }
function SubAgentsSection() {
  const { t } = useI18n();
  const [agents, setAgents] = useState<SubAgentRow[]>([]);
  useEffect(() => {
    let alive = true;
    listSubagents()
      .then((res: any) => {
        if (!alive) return;
        const list = Array.isArray(res) ? res : res?.agents ?? [];
        setAgents(list.map((a: any) => ({
          id: a.id ?? a.name, name: a.name ?? a.id, model: a.model ?? "", status: a.status ?? "ready",
        })));
      })
      .catch(() => {});
    return () => { alive = false; };
  }, []);

  const icon = (st: string) =>
    st === "running" ? <span className="ring act" /> : st === "done" ? "✓" : <span className="ring wait" />;
  const badge = (st: string) =>
    st === "running" ? <span className="stx run">运行</span>
      : st === "done" ? <span className="stx ok">完成</span>
      : <span className="stx pend">就绪</span>;

  return (
    <Section label={t("drawer.subagents")} meta={agents.length}>
      {agents.length === 0 && <div className="step">编排层未启用</div>}
      {agents.map((a) => (
        <div key={a.id} className="sa">
          <span className="k">{icon(a.status)}</span>
          <span className="fn">{a.name}</span>
          <span className="sub">{a.model}</span>
          {badge(a.status)}
        </div>
      ))}
    </Section>
  );
}

/** 编排进度：SVG 圆环 + 阶段列表（抽屉打开时 1Hz 轮询） */
interface OrchAction { action_id: string; name: string; status: unknown; }
interface OrchReport {
  status: unknown; goal: string | null;
  total_actions: number; completed_actions: number;
  in_progress_actions: number; actions: OrchAction[];
}
const RING_R = 18;
const RING_C = 2 * Math.PI * RING_R;

function normStatus(st: unknown): string {
  if (typeof st === "string") return st.toLowerCase();
  if (st && typeof st === "object") return Object.keys(st)[0]?.toLowerCase() ?? "";
  return "";
}

function OrchSection() {
  const { t } = useI18n();
  const [report, setReport] = useState<OrchReport | null>(null);

  useEffect(() => {
    let alive = true;
    const tick = () => getOrchProgress().then((r) => { if (alive) setReport(r); }).catch(() => {});
    tick();
    const timer = setInterval(tick, 1000);
    return () => { alive = false; clearInterval(timer); };
  }, []);

  const total = report?.total_actions ?? 0;
  const pct = total > 0 ? Math.round(((report?.completed_actions ?? 0) / total) * 100) : 0;
  const status = normStatus(report?.status);
  const current = report?.actions.find((a) => normStatus(a.status).startsWith("in"));

  return (
    <Section label={t("drawer.orch")} meta={`${pct}%`} defaultOpen={status !== "idle" && !!report}>
      {(!report || status === "idle") && <div className="step">编排空闲</div>}
      {report && status !== "idle" && (
        <>
          <div className="orch">
            <div className="oring">
              <svg width="44" height="44" viewBox="0 0 44 44">
                <circle className="bgc" cx="22" cy="22" r={RING_R} />
                <circle
                  className="fgc" cx="22" cy="22" r={RING_R}
                  strokeDasharray={RING_C}
                  strokeDashoffset={RING_C * (1 - pct / 100)}
                />
              </svg>
              <span className="ot">{pct}%</span>
            </div>
            <div className="ocur">
              <div className="on1">{current?.name ?? report.goal ?? "—"}</div>
              <div className="on2">
                {status === "completed"
                  ? "全部阶段完成"
                  : `${report.completed_actions}/${total} · ${status === "planning" ? "规划中" : "运行中"}`}
              </div>
            </div>
          </div>
          {report.actions.slice(0, 8).map((a) => {
            const st = normStatus(a.status);
            const done = st === "completed";
            const run = st.startsWith("in");
            return (
              <div key={a.action_id} className="oact">
                <span className="k" style={done ? { color: "var(--green)" } : undefined}>
                  {done ? "✓" : run ? <span className="ring act" /> : <span className="ring wait" />}
                </span>
                <span className="mono" style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{a.name}</span>
                {done ? <span className="stx ok">完成</span> : run ? <span className="stx run">运行</span> : <span className="stx pend">等待</span>}
              </div>
            );
          })}
        </>
      )}
    </Section>
  );
}

export default function TaskDrawer() {
  const { t } = useI18n();
  const drawerOpen = useStore((s) => s.drawerOpen);
  const setDrawerOpen = useStore((s) => s.setDrawerOpen);
  const setWorktrees = useStore((s) => s.setWorktrees);
  const setChangedFiles = useStore((s) => s.setChangedFiles);
  const [tab, setTab] = useState<"task" | "context">("task");

  // 打开抽屉时刷新文件变更/工作树
  useEffect(() => {
    if (!drawerOpen) return;
    let alive = true;
    (async () => {
      const [wt, cf] = await Promise.all([
        invokeOrStub<WorktreeInfo[]>("list_worktrees", undefined, STUB_WORKTREES),
        invokeOrStub<ChangedFile[]>("get_changed_files", undefined, STUB_CHANGED_FILES),
      ]);
      if (!alive) return;
      setWorktrees(wt);
      setChangedFiles(cf);
    })().catch(() => {});
    return () => { alive = false; };
  }, [drawerOpen, setWorktrees, setChangedFiles]);

  if (!drawerOpen) return <aside className="task-drawer" aria-hidden />;

  return (
    <aside className="task-drawer">
      <div className="td-h">
        <div className="td-tabs">
          <div className={`td-tab ${tab === "task" ? "active" : ""}`} onClick={() => setTab("task")}>{t("drawer.task")}</div>
          <div className={`td-tab ${tab === "context" ? "active" : ""}`} onClick={() => setTab("context")}>{t("drawer.context")}</div>
        </div>
        <button className="btn-icon x" onClick={() => setDrawerOpen(false)} title={t("drawer.close")}>✕</button>
      </div>
      {tab === "task" ? (
        <div className="td-b">
          <ProgressSection />
          <FilesSection />
          <WorktreeSection />
          <SubAgentsSection />
          <OrchSection />
        </div>
      ) : (
        <RightPanel />
      )}
    </aside>
  );
}

/**
 * DiffPanel.tsx — Diff 对比面板（挤压式，网格第三列，mockup 定稿）
 * 源代码 vs 修改后成对并排（单列表虚拟化，天然对齐）
 * 逐文件审查：接受/拒绝 → 自动切换下一个待审文件，全部处理完自动关闭
 */

import { useEffect, useMemo, useState } from "react";
import { Virtuoso } from "react-virtuoso";
import { useStore } from "../store";
import { useI18n } from "../i18n";
import { getFileDiff } from "../bridge";
import { invokeOrStub, STUB_DIFF } from "../lib/stubs";
import { parseUnifiedDiff, type ParsedDiff } from "../lib/parseDiff";
import type { DiffRow } from "../types";

function Cell({ no, text, cls }: { no: number | null; text: string; cls: string }) {
  return (
    <div className={`dl ${cls}`}>
      <span className="no">{no ?? ""}</span>
      <span className="cd">{text || " "}</span>
    </div>
  );
}

function PairRow({ row }: { row: DiffRow }) {
  if (row.type === "hunk") {
    return (
      <div className="dl-pair">
        <Cell no={null} text={row.oldText} cls="empty" />
        <Cell no={null} text="" cls="empty" />
      </div>
    );
  }
  const left =
    row.type === "add" ? { cls: "empty", no: null, text: "" }
      : row.type === "ctx" ? { cls: "ctx", no: row.oldNo, text: row.oldText }
      : { cls: "del", no: row.oldNo, text: row.oldText };
  const right =
    row.type === "del" ? { cls: "empty", no: null, text: "" }
      : row.type === "ctx" ? { cls: "ctx", no: row.newNo, text: row.newText }
      : { cls: "add", no: row.newNo, text: row.newText };
  return (
    <div className="dl-pair">
      <Cell no={left.no} text={left.text} cls={left.cls} />
      <Cell no={right.no} text={right.text} cls={right.cls} />
    </div>
  );
}

export default function DiffPanel() {
  const { t } = useI18n();
  const diffOpen = useStore((s) => s.diffOpen);
  const setDiffOpen = useStore((s) => s.setDiffOpen);
  const activeDiffFile = useStore((s) => s.activeDiffFile);
  const setActiveDiffFile = useStore((s) => s.setActiveDiffFile);
  const changedFiles = useStore((s) => s.changedFiles);
  const rvState = useStore((s) => s.rvState);
  const setRvDecision = useStore((s) => s.setRvDecision);
  const running = useStore((s) => s.running);

  const [parsed, setParsed] = useState<ParsedDiff | null>(null);
  const [loading, setLoading] = useState(false);

  // 拉取并解析当前文件 diff
  useEffect(() => {
    if (!diffOpen || !activeDiffFile) return;
    let alive = true;
    setLoading(true);
    getFileDiff(activeDiffFile)
      .catch(() => invokeOrStub<string>("__diff_stub__", undefined, STUB_DIFF))
      .then((raw) => {
        if (!alive) return;
        setParsed(parseUnifiedDiff(raw || ""));
      })
      .finally(() => { if (alive) setLoading(false); });
    return () => { alive = false; };
  }, [diffOpen, activeDiffFile]);

  const fileMeta = changedFiles.find((f) => f.path === activeDiffFile);
  const pending = useMemo(
    () => changedFiles.filter((f) => !(f.path in rvState)),
    [changedFiles, rvState]
  );

  const decide = async (accepted: boolean) => {
    if (!activeDiffFile) return;
    const cmd = accepted ? "accept_file_change" : "reject_file_change";
    await invokeOrStub<void>(cmd, { path: activeDiffFile }, undefined);
    setRvDecision(activeDiffFile, accepted);
    // 推进到下一个待审文件；全部处理完自动关闭面板
    const next = pending.find((f) => f.path !== activeDiffFile);
    if (next) {
      setActiveDiffFile(next.path);
    } else {
      setDiffOpen(false);
    }
  };

  if (!diffOpen) return <section className="dpanel" aria-hidden />;

  const decided = activeDiffFile ? rvState[activeDiffFile] : undefined;
  const tag = fileMeta?.tag ?? "M";
  const stat = parsed ? `+${parsed.additions} −${parsed.deletions}` : "";

  return (
    <section className="dpanel">
      <div className="dp-h">
        <span className={`ftag ${tag === "A" ? "a" : ""}`}>{tag}</span>
        <span className="mono" style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
          {activeDiffFile ?? "—"}
        </span>
        <span className="meta">{stat}{parsed?.truncated ? ` · ${t("diff.truncated")}` : ""}</span>
        <button className="btn-icon x" onClick={() => setDiffOpen(false)} title={t("app.close")}>✕</button>
      </div>

      <div className="dp-ch"><div>{t("diff.source")}</div><div>{t("diff.modified")}</div></div>

      {loading ? (
        <div className="empty-state" style={{ flex: 1 }}>
          <span className="ring act" />
          <div className="empty-state-text" style={{ marginTop: 8 }}>{t("diff.loading")}</div>
        </div>
      ) : !parsed || parsed.rows.length === 0 ? (
        <div className="empty-state" style={{ flex: 1 }}>
          <div className="empty-state-icon">≡</div>
          <div className="empty-state-text">{t("diff.empty")}</div>
        </div>
      ) : (
        <Virtuoso
          className="dp-cols"
          style={{ flex: 1 }}
          data={parsed.rows}
          itemContent={(_, row) => <PairRow row={row} />}
          increaseViewportBy={{ top: 300, bottom: 300 }}
        />
      )}

      <div className="dp-foot">
        {decided !== undefined ? (
          <span className="meta" style={{ color: decided ? "var(--green)" : "var(--red)", fontWeight: 600 }}>
            {decided ? t("diff.accepted") : t("diff.rejected")}
          </span>
        ) : (
          <span className="meta">
            {t("diff.pendingLeft")} {pending.length} {t("diff.pendingFiles")}；{t("diff.pendingHint")}
          </span>
        )}
        <span className="spacer" />
        {decided === undefined && (
          <>
            <button className="btn" onClick={() => decide(false)} disabled={running} title={running ? "Agent 运行中，暂不可审查" : t("diff.pendingHint")}>
              {t("diff.reject")}
            </button>
            <button className="btn btn-primary" onClick={() => decide(true)} disabled={running} title={running ? "Agent 运行中，暂不可审查" : t("diff.pendingHint")}>
              {t("diff.accept")}
            </button>
          </>
        )}
      </div>
    </section>
  );
}

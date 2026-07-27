/**
 * ChipRow.tsx — 输入框上方小长方形（mockup 定稿）
 * 分支/工作树 chip + 代码审查入口 chip（挂载时经 invokeOrStub 拉取）
 */

import { useEffect } from "react";
import { useStore } from "../store";
import { useI18n } from "../i18n";
import { invokeOrStub, STUB_CHANGED_FILES, STUB_WORKTREES } from "../lib/stubs";
import type { ChangedFile, WorktreeInfo } from "../types";

export default function ChipRow() {
  const { t } = useI18n();
  const worktrees = useStore((s) => s.worktrees);
  const changedFiles = useStore((s) => s.changedFiles);
  const rvState = useStore((s) => s.rvState);
  const setWorktrees = useStore((s) => s.setWorktrees);
  const setChangedFiles = useStore((s) => s.setChangedFiles);
  const setActiveDiffFile = useStore((s) => s.setActiveDiffFile);
  const setDiffOpen = useStore((s) => s.setDiffOpen);

  useEffect(() => {
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
  }, [setWorktrees, setChangedFiles]);

  const main = worktrees.find((w) => !w.is_current);
  const current = worktrees.find((w) => w.is_current);
  const pending = changedFiles.filter((f) => !(f.path in rvState));
  const adds = pending.reduce((n, f) => n + f.additions, 0);
  const dels = pending.reduce((n, f) => n + f.deletions, 0);

  const openReview = () => {
    if (!pending.length) return;
    setActiveDiffFile(pending[0].path);
    setDiffOpen(true);
  };

  if (!worktrees.length && !pending.length) return null;

  return (
    <div className="chiprow">
      {worktrees.length > 0 && (
        <span className="chipx" title="主分支 → 当前工作树">
          ⎇ {main?.branch ?? "main"}{current ? ` → ⌥ ${current.branch}` : ""}
        </span>
      )}
      {pending.length > 0 && (
        <span className="chipx" title="代码改动待审查 · 点击逐文件对比" onClick={openReview}>
          {t("chip.review")} <b>{pending.length}</b>
          <span className="p">+{adds}</span>
          <span className="n">−{dels}</span>
        </span>
      )}
    </div>
  );
}

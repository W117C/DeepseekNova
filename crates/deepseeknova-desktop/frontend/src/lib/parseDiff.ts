/**
 * lib/parseDiff.ts — unified diff → 成对并排行
 *
 * 输入：`git diff` 原始输出（现有 get_file_diff 命令返回值，后端零改动）。
 * 输出：DiffRow[]，左列(源代码)与右列(修改后)天然对齐：
 *   - ctx  两侧同行
 *   - del  左侧删除行，右侧斜纹占位
 *   - add  左侧斜纹占位，右侧新增行
 *   - mod  同一 hunk 内删/增配对成"修改"行（左删右增同一行显示）
 *   - hunk 块分隔（@@ 头）
 */

import type { DiffRow } from "../types";

export const MAX_DIFF_ROWS = 2000;

export interface ParsedDiff {
  rows: DiffRow[];
  truncated: boolean;
  additions: number;
  deletions: number;
}

export function parseUnifiedDiff(diff: string): ParsedDiff {
  const rows: DiffRow[] = [];
  let additions = 0;
  let deletions = 0;
  let oldNo = 0;
  let newNo = 0;
  let inHunk = false;

  // 待配对的删除行缓冲：hunk 内连续 del 后紧跟 add 时配对为 mod
  let pendingDel: { no: number; text: string }[] = [];

  const flushPendingDel = () => {
    for (const d of pendingDel) {
      rows.push({ type: "del", oldNo: d.no, oldText: d.text, newNo: null, newText: "" });
    }
    pendingDel = [];
  };

  const lines = diff.split("\n");
  // 末尾换行产生的空字符串是分割伪影，不是真实行
  if (lines.length > 0 && lines[lines.length - 1] === "") lines.pop();
  for (const line of lines) {
    if (rows.length >= MAX_DIFF_ROWS) {
      flushPendingDel();
      return { rows, truncated: true, additions, deletions };
    }

    const hunkMatch = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@(.*)$/.exec(line);
    if (hunkMatch) {
      flushPendingDel();
      oldNo = parseInt(hunkMatch[1], 10);
      newNo = parseInt(hunkMatch[2], 10);
      inHunk = true;
      rows.push({ type: "hunk", oldNo: null, oldText: line, newNo: null, newText: line });
      continue;
    }
    if (!inHunk) continue; // 跳过 diff --git / index / --- / +++ 头
    if (line.startsWith("\\")) continue; // "\ No newline at end of file"

    if (line.startsWith("-")) {
      pendingDel.push({ no: oldNo++, text: line.slice(1) });
      deletions++;
    } else if (line.startsWith("+")) {
      additions++;
      const paired = pendingDel.shift();
      if (paired) {
        rows.push({ type: "mod", oldNo: paired.no, oldText: paired.text, newNo: newNo++, newText: line.slice(1) });
      } else {
        rows.push({ type: "add", oldNo: null, oldText: "", newNo: newNo++, newText: line.slice(1) });
      }
    } else if (line.startsWith(" ") || line === "") {
      flushPendingDel();
      const text = line.startsWith(" ") ? line.slice(1) : "";
      rows.push({ type: "ctx", oldNo: oldNo++, oldText: text, newNo: newNo++, newText: text });
    } else {
      // 新文件头等非 hunk 内容 → 结束当前 hunk
      flushPendingDel();
      inHunk = false;
    }
  }
  flushPendingDel();
  return { rows, truncated: false, additions, deletions };
}

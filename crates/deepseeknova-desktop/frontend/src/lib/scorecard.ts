/**
 * 测光·评分卡：六维光度表解析与渲染口径。
 */

export const SCORECARD_DIMS = ['治理', '验证', '反思', '审查', '协议', '综合'] as const;

export interface ScorecardRow {
  dim: string;
  score: number;
}

/** 兼容 serve 落盘 JSON 的常见形态（`scores` 对象或 `scorecard` 数组）。 */
export function parseScorecard(json: unknown): ScorecardRow[] | null {
  const raw = json as Record<string, unknown>;
  const scores = (raw?.scores ?? raw?.scorecard ?? raw) as Record<string, unknown>;
  if (!scores || typeof scores !== 'object') return null;
  const rows: ScorecardRow[] = [];
  for (const dim of SCORECARD_DIMS) {
    const v = Number(scores[dim]);
    if (Number.isFinite(v)) rows.push({ dim, score: Math.max(0, Math.min(100, v)) });
  }
  return rows.length > 0 ? rows : null;
}

/** 细靛蓝横条宽度（0..100 钳制）。 */
export function barWidth(score: number): string {
  return `${Math.max(0, Math.min(100, score))}%`;
}

export function formatScore(score: number): string {
  return score.toFixed(1);
}

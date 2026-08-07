/**
 * token 预算刻度尺：细线标尺 + 用量填充（纯函数）。
 */

export interface BudgetScale {
  percent: string;
  fillPercent: number;
  ticks: number[];
}

export function budgetScale(used: number, limit: number, tickCount = 6): BudgetScale {
  const safe = limit > 0 ? limit : 1;
  const ratio = Math.max(0, Math.min(1, used / safe));
  const ticks: number[] = [];
  for (let i = 0; i < tickCount; i++) ticks.push(Math.round((i / (tickCount - 1)) * 100));
  return { percent: `${(ratio * 100).toFixed(1)}%`, fillPercent: ratio * 100, ticks };
}

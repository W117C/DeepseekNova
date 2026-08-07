import { describe, expect, it } from 'vitest';
import { budgetScale } from './budget';

describe('budgetScale', () => {
  it('computes percent and fill', () => {
    const s = budgetScale(128.7, 200, 6);
    expect(s.percent).toBe('64.3%');
    expect(s.fillPercent).toBeCloseTo(64.35, 1);
    expect(s.ticks).toHaveLength(6);
  });
  it('clamps ratio and survives zero limit', () => {
    expect(budgetScale(300, 200).fillPercent).toBe(100);
    expect(budgetScale(1, 0).fillPercent).toBe(100);
  });
});

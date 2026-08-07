import { describe, expect, it } from 'vitest';
import { barWidth, formatScore, parseScorecard } from './scorecard';

describe('parseScorecard', () => {
  it('parses scores object and preserves dim order', () => {
    const rows = parseScorecard({
      scores: { 治理: 92.3, 验证: 94.7, 反思: 88.1, 审查: 90.5, 协议: 96.2, 综合: 92.0 },
    });
    expect(rows?.map((r) => r.dim)).toEqual(['治理', '验证', '反思', '审查', '协议', '综合']);
    expect(rows?.[5].score).toBe(92.0);
  });
  it('rejects malformed input', () => {
    expect(parseScorecard(null)).toBeNull();
    expect(parseScorecard({ scores: { nope: 1 } })).toBeNull();
  });
});

describe('barWidth / formatScore', () => {
  it('clamps to 0..100', () => {
    expect(barWidth(120)).toBe('100%');
    expect(barWidth(-5)).toBe('0%');
    expect(barWidth(42)).toBe('42%');
  });
  it('formats one decimal', () => {
    expect(formatScore(92.3)).toBe('92.3');
  });
});

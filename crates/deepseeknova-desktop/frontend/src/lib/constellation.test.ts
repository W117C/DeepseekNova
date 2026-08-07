import { describe, expect, it } from 'vitest';
import { constellationPath, layoutConstellation } from './constellation';

describe('layoutConstellation', () => {
  const events = [
    { label: 'plan', weight: 2 },
    { label: 'read', weight: 3 },
    { label: 'edit', weight: 4 },
    { label: 'test', weight: 5 },
  ];
  it('returns one node per event, ordered along the vertical axis', () => {
    const nodes = layoutConstellation(events, 340, 430);
    expect(nodes).toHaveLength(4);
    expect(nodes[0].y).toBeLessThan(nodes[1].y);
    expect(nodes[1].y).toBeLessThan(nodes[2].y);
    expect(nodes[2].y).toBeLessThan(nodes[3].y);
  });
  it('keeps nodes inside the frame', () => {
    const nodes = layoutConstellation(events, 340, 430);
    for (const n of nodes) {
      expect(n.x).toBeGreaterThanOrEqual(50);
      expect(n.x).toBeLessThanOrEqual(260);
      expect(n.y).toBeGreaterThanOrEqual(51);
      expect(n.y).toBeLessThanOrEqual(378);
    }
  });
  it('is deterministic and heavier events get larger dots', () => {
    const a = layoutConstellation(events);
    const b = layoutConstellation(events);
    expect(a).toEqual(b);
    expect(a[3].r).toBeGreaterThan(a[0].r);
  });
  it('builds a connected path', () => {
    const nodes = layoutConstellation(events);
    const path = constellationPath(nodes);
    expect(path.startsWith('M')).toBe(true);
    expect(path.match(/L/g)?.length).toBe(3);
  });
});

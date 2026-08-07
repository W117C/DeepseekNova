import { describe, expect, it } from 'vitest';
import { groupSessionsByNight, nightKeyFromId } from './sessions';

describe('nightKeyFromId', () => {
  it('extracts night date from chat id', () => {
    expect(nightKeyFromId('chat-20260807-164211')).toBe('08-07');
  });
  it('falls back for unknown shape', () => {
    expect(nightKeyFromId('weird')).toBe('----');
  });
});

describe('groupSessionsByNight', () => {
  const sessions = [
    { id: 'chat-20260806-220000', title: '安全审计报告整理' },
    { id: 'chat-20260807-131000', title: 'README 截图资产补齐' },
    { id: 'chat-20260807-160000', title: '修复权限门控回归', current: true },
  ];
  it('groups newest night first and sorts entries desc', () => {
    const groups = groupSessionsByNight(sessions);
    expect(groups.map((g) => g.night)).toEqual(['08-07', '08-06']);
    expect(groups[0].entries[0].title).toBe('修复权限门控回归');
  });
  it('assigns magnitude 3 to current, 2 to same night, 1 to older nights', () => {
    const groups = groupSessionsByNight(sessions);
    expect(groups[0].entries.find((e) => e.title === '修复权限门控回归')?.magnitude).toBe(3);
    expect(groups[0].entries.find((e) => e.title === 'README 截图资产补齐')?.magnitude).toBe(2);
    expect(groups[1].entries[0].magnitude).toBe(1);
  });
});

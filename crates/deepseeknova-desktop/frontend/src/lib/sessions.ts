/**
 * 观测日志：会话按夜次分组 + 星等三档。
 * 星等语义：当前会话 ◉(3) / 同夜其它 ●(2) / 更早夜次 ·(1)。
 */

export interface SessionMeta {
  id: string;
  title: string;
  current?: boolean;
}

export interface SessionEntry {
  id: string;
  title: string;
  magnitude: 1 | 2 | 3;
}

export interface NightGroup {
  night: string;
  entries: SessionEntry[];
}

/** `chat-20260807-164211` → `08-07`；无日期前缀回退 `----`。 */
export function nightKeyFromId(id: string): string {
  const m = /(\d{4})(\d{2})(\d{2})-\d{6}/.exec(id);
  if (!m) return '----';
  return `${m[2]}-${m[3]}`;
}

/** 按 id 夜次倒序分组（同夜按 id 倒序，最新在前）。 */
export function groupSessionsByNight(sessions: SessionMeta[]): NightGroup[] {
  const byNight = new Map<string, SessionMeta[]>();
  for (const s of sessions) {
    const night = nightKeyFromId(s.id);
    const list = byNight.get(night) ?? [];
    list.push(s);
    byNight.set(night, list);
  }
  const groups = [...byNight.entries()]
    .sort((a, b) => b[0].localeCompare(a[0]))
    .map(([night, list]) => {
      const sorted = [...list].sort((a, b) => b.id.localeCompare(a.id));
      const entries: SessionEntry[] = sorted.map((s) => ({
        id: s.id,
        title: s.title,
        magnitude: s.current ? (3 as const) : (2 as const),
      }));
      return { night, entries };
    });
  // 更早夜次降为 1 档（保留第一组星等 2/3）。
  for (let i = 1; i < groups.length; i++) {
    for (const e of groups[i].entries) e.magnitude = 1;
  }
  return groups;
}

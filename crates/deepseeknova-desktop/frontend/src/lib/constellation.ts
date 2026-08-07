/**
 * 本次观测星座图：事件星点沿竖向时间轴分布，细线连成星座路径。
 * 布局是纯函数：同一输入永远得到同一组节点坐标（便于测试与截图复现）。
 */

export type NodeStatus = 'ok' | 'warn' | 'running' | 'fail';

export interface ConstellationEvent {
  label: string;
  weight: number; // 1..5，事件权重（工具调用 > 消息 > 心跳）
  status?: NodeStatus;
}

export interface ConstellationNode {
  label: string;
  x: number;
  y: number;
  r: number;
  status: NodeStatus;
}

function hash(s: string): number {
  let h = 0;
  for (let i = 0; i < s.length; i++) h = (h * 31 + s.charCodeAt(i)) | 0;
  return Math.abs(h);
}

/** 竖向时间轴：Y = 顺序刻度，X = 权重/标签偏移；节点铺满图框中部 2/3。 */
export function layoutConstellation(
  events: ConstellationEvent[],
  width = 340,
  height = 430,
): ConstellationNode[] {
  const n = events.length;
  const top = height * 0.12;
  const bottom = height * 0.88;
  return events.map((ev, i) => {
    const t = n <= 1 ? 0.5 : i / (n - 1);
    const y = Math.round(top + t * (bottom - top));
    const offset = (hash(ev.label) % 170) + 40;
    const x = Math.min(Math.max(50, offset), width - 80);
    const r = Math.min(6, 2.5 + Math.max(0, ev.weight - 1) * 1.1);
    return {
      label: ev.label,
      x,
      y,
      r,
      status: ev.status ?? 'ok',
    };
  });
}

/** 星座路径：按时间顺序连成折线 SVG path。 */
export function constellationPath(nodes: ConstellationNode[]): string {
  if (nodes.length === 0) return '';
  return nodes
    .map((p, i) => `${i === 0 ? 'M' : 'L'}${p.x} ${p.y}`)
    .join(' ');
}

import { For } from 'solid-js';
import { budgetScale } from './lib/budget';
import {
  constellationPath,
  layoutConstellation,
  type ConstellationEvent,
} from './lib/constellation';
import { barWidth, formatScore, parseScorecard } from './lib/scorecard';
import { groupSessionsByNight, type SessionMeta } from './lib/sessions';

const SESSIONS: SessionMeta[] = [
  { id: 'chat-20260807-164211', title: '修复权限门控回归', current: true },
  { id: 'chat-20260807-150832', title: '重构策略缓存层' },
  { id: 'chat-20260807-135544', title: 'README 截图资产补齐' },
  { id: 'chat-20260806-224156', title: '安全审计报告整理' },
  { id: 'chat-20260806-201732', title: 'cargo-deny 规则调整' },
  { id: 'chat-20260806-180520', title: '夹具临时目录修复' },
];

const EVENTS: ConstellationEvent[] = [
  { label: 'plan', weight: 2 },
  { label: 'read', weight: 3 },
  { label: 'edit', weight: 4 },
  { label: 'test', weight: 5, status: 'running' },
  { label: 'diff', weight: 4 },
  { label: 'review', weight: 3 },
  { label: 'fix', weight: 2 },
];

const SCORECARD = parseScorecard({
  scores: { 治理: 92.3, 验证: 94.7, 反思: 88.1, 审查: 90.5, 协议: 96.2, 综合: 92.0 },
});

const BUDGET = budgetScale(128.7, 200);
const NODES = layoutConstellation(EVENTS);
const PATH = constellationPath(NODES);
const GROUPS = groupSessionsByNight(SESSIONS);

const MAGNITUDE_SIZE = { 1: 2, 2: 3, 3: 4 } as const;
const MAGNITUDE_GLYPH = { 1: '·', 2: '●', 3: '◉' } as const;

export default function App() {
  return (
    <div class="h-full w-full flex flex-col">
      <header class="band1">
        <div class="flex items-center gap-2 text-[13px] font-semibold">
          <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#EDF1FB" stroke-width="1.5">
            <path d="M2 12a6 6 0 0 1 12 0" />
            <path d="M5 12a3 3 0 0 1 6 0" />
            <line x1="1" y1="13" x2="15" y2="13" />
          </svg>
          修复权限门控回归
          <span class="w-1.5 h-1.5 rounded-full bg-[var(--color-accent)]" />
        </div>
        <div class="flex items-center gap-3">
          <span class="mono text-[11px] text-[var(--color-dim)]">2025-08-07 16:42:11Z</span>
          <div class="flex items-center gap-2">
            <span class="mono text-[11px] text-[var(--color-dim)]">128.7K / 200.0K</span>
            <div class="budget-track">
              <div class="budget-fill" style={{ width: `${BUDGET.fillPercent}%` }} />
            </div>
          </div>
        </div>
      </header>

      <div class="flex flex-1 min-h-0">
        <aside class="sidebar">
          <div class="flex items-center gap-2 text-[13px] font-semibold tracking-wide">
            <svg viewBox="0 0 14 14" width="14" height="14" fill="none" stroke="var(--color-accent)" stroke-width="1.5">
              <path d="M7 1l1.8 3.7 4.1.6-3 2.9.7 4.1L7 10.5 3.4 12.3l.7-4.1-3-2.9 4.1-.6z" stroke-linejoin="round" />
            </svg>
            DeepseekNova
          </div>
          <button class="new-session mt-3.5">＋ 新会话</button>

          <For each={GROUPS}>
            {(group) => (
              <div class="mt-4">
                <div class="night-head">{group.night} 夜</div>
                <For each={group.entries}>
                  {(entry) => (
                    <div classList={{ 'log-entry': true, active: entry.magnitude === 3 }}>
                      <span
                        classList={{ star: true, dim: entry.magnitude === 1 }}
                        style={{ width: `${MAGNITUDE_SIZE[entry.magnitude]}px`, height: `${MAGNITUDE_SIZE[entry.magnitude]}px` }}
                        title={MAGNITUDE_GLYPH[entry.magnitude]}
                      />
                      <div class="min-w-0 flex-1">
                        <div class="text-[12.5px] truncate">{entry.title}</div>
                        <div class="mono text-[10.5px] text-[#5A6480]">{entry.id.slice(-6)}</div>
                      </div>
                    </div>
                  )}
                </For>
              </div>
            )}
          </For>

          <div class="flex-1" />
          <div class="side-foot">
            <svg viewBox="0 0 16 16" width="15" height="15" fill="none" stroke="var(--color-dim)" stroke-width="1.4">
              <circle cx="8" cy="8" r="2.4" />
              <path d="M8 1.8v2M8 12.2v2M1.8 8h2M12.2 8h2M3.6 3.6l1.4 1.4M11 11l1.4 1.4M12.4 3.6L11 5M5 11l-1.4 1.4" />
            </svg>
            <div class="ml-auto flex items-center gap-1.5 text-[11px] text-[var(--color-dim)]">
              <span class="w-1.5 h-1.5 rounded-full bg-[var(--color-success)]" /> serve 已连接
            </div>
          </div>
        </aside>

        <div class="flex flex-1 flex-col min-w-0">
          <div class="runs">
            <span class="text-[11px] tracking-[0.1em] text-[var(--color-dim)]">观测之夜</span>
            <span class="chip"><span class="w-1.5 h-1.5 rounded-full bg-[var(--color-success)]" />✓ 已完成</span>
            <span class="chip"><span class="w-1.5 h-1.5 rounded-full bg-[var(--color-amber)]" />⏸ 已中断 ↻</span>
            <span class="chip"><span class="w-1.5 h-1.5 rounded-full bg-[var(--color-accent)]" />● 运行中</span>
            <a class="ml-auto text-[11.5px] text-[var(--color-accent)] no-underline" href="#">全部 runs →</a>
          </div>

          <div class="flex flex-1 min-h-0">
            <section class="chat">
              <div class="logbook">
                <div class="msg">
                  <div class="msg-head">
                    <span class="text-[12px] font-bold text-[var(--color-accent)]">你</span>
                    <span class="mono ml-auto text-[10.5px] text-[#5A6480]">16:40:12</span>
                  </div>
                  <p class="text-[13.5px] text-[#DDE3F5]">
                    非管理员用户仍然可以访问 /api/admin/stats，疑似门控回归。
                  </p>
                </div>
                <div class="msg">
                  <div class="msg-head">
                    <span class="text-[12px] font-bold text-[var(--color-ink)]">deepseek-v4-pro</span>
                    <span class="mono ml-auto text-[10.5px] text-[#5A6480]">16:40:15</span>
                  </div>
                  <div class="text-[12px] italic text-[var(--color-dim)] py-1">▸ 推理过程 (12s)</div>
                  <div class="tool-row">
                    <span class="mono text-[#C7CFE8]">⚙ shell · cargo test -p deepseeknova-security</span>
                    <span class="text-[var(--color-success)]">✓</span>
                  </div>
                  <div class="diff mono">
                    <div class="line text-[#AEB9F2]">@@ -114,7 +114,8 @@ pub async fn admin_stats(</div>
                    <div class="line del">{'-    if role != Role::Admin { return Err(Forbidden); }'}</div>
                    <div class="line del">-    let stats = store.admin_stats().await?;</div>
                    <div class="line add">+    gate.assert(Role::Admin, "admin/stats").await?;</div>
                    <div class="line add">+    let stats = store.admin_stats().await?;</div>
                    <div class="line text-[#C9D1EA]">     Ok(Json(stats))</div>
                  </div>
                  <p class="text-[13.5px] text-[#DDE3F5]">
                    已把角色校验收敛到共享 PermissionGate，并补了回归测试。
                  </p>
                </div>
              </div>
              <div class="composer">
                <span class="mono text-[13px] text-[var(--color-accent)]">❯</span>
                <span class="flex-1 text-[13px] text-[#68718F]">输入消息…</span>
                <span class="mono text-[11px] text-[var(--color-dim)] border border-[var(--color-hairline)] rounded-full px-2 py-0.5">
                  deepseek-v4-pro
                </span>
                <span class="mono text-[10.5px] text-[#5A6480]">{BUDGET.percent}</span>
              </div>
            </section>

            <aside class="right">
              <div class="frame flex-[5.2] min-h-0">
                <div class="frame-title">本次观测</div>
                <svg class="block h-full w-full" viewBox="0 0 340 430" fill="none">
                  <defs>
                    <pattern id="grid" width="34" height="34" patternUnits="userSpaceOnUse">
                      <path d="M34 0H0V34" stroke="#232C4A" stroke-opacity="0.35" />
                    </pattern>
                  </defs>
                  <rect width="340" height="430" fill="url(#grid)" />
                  <line x1="286" y1="16" x2="286" y2="414" stroke="#232C4A" />
                  <g font-family="ui-monospace, Menlo, monospace" font-size="9" fill="#8A93B0">
                    <text x="294" y="26">16:50</text>
                    <text x="294" y="126">16:40</text>
                    <text x="294" y="226">16:30</text>
                    <text x="294" y="326">16:20</text>
                    <text x="294" y="414">BUDGET 128.7K / 200.0K</text>
                  </g>
                  <path d={PATH} stroke="#4D6BFE" stroke-opacity="0.55" stroke-width="1" />
                  <For each={NODES}>
                    {(node) => (
                      <circle
                        cx={node.x}
                        cy={node.y}
                        r={node.r}
                        fill={node.status === 'running' ? '#E8A33D' : '#4D6BFE'}
                      />
                    )}
                  </For>
                  <g font-family="ui-monospace, Menlo, monospace" font-size="9" fill="#B7BFD8">
                    <For each={NODES}>
                      {(node) => (
                        <text x={node.x + node.r + 3} y={node.y + 3}>
                          {node.label}
                        </text>
                      )}
                    </For>
                  </g>
                </svg>
              </div>

              <div class="frame flex-[4.8] min-h-0">
                <div class="frame-title">测光 · 评分卡</div>
                <div class="phot flex flex-1 flex-col justify-around">
                  <For each={SCORECARD ?? []}>
                    {(row, i) => (
                      <div classList={{ prow: true, total: i() === (SCORECARD?.length ?? 0) - 1 }}>
                        <span class="name">{row.dim}</span>
                        <div class="track">
                          <div class="bar" style={{ width: barWidth(row.score) }} />
                        </div>
                        <span class="mono w-11 text-right text-[11.5px] text-[var(--color-dim)]">
                          {formatScore(row.score)}
                        </span>
                      </div>
                    )}
                  </For>
                </div>
              </div>
            </aside>
          </div>
        </div>
      </div>
    </div>
  );
}

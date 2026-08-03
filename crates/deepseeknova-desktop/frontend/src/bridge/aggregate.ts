/**
 * aggregate.ts — WireEvent → Message 列表的纯聚合函数。
 *
 * 与 React 时代版本不同：此模块不依赖 Solid，纯函数便于 node:test 直接验证。
 * 语义与后端 WireEvent 契约对齐（见 deepseeknova-core/src/runner.rs）：
 * - text_delta 增量累积到当前 assistant 消息
 * - reasoning_delta 累积到 reasoning 消息（与 assistant 分段）
 * - tool_call_start/delta/end/result 生命周期
 * - done 携带**全量最终文本**（后端 final_text 累积），直接替换而非追加，
 *   避免与已聚合的 delta 重复
 * - paused 标记运行暂停（reason 展示）
 * - error 生成 error 消息
 */

import type { Message, WireEvent } from "../types";

export interface AggregateResult {
  messages: Message[];
  /** done/error/paused 到达后应结束本次运行 */
  finished: boolean;
  /** paused 的 reason（供 UI 展示可恢复提示） */
  pausedReason?: string;
}

export function uid(): string {
  return crypto.randomUUID();
}

export function aggregateMessages(messages: Message[], ev: WireEvent): AggregateResult {
  const next = [...messages];
  const last = next[next.length - 1];
  let finished = false;
  let pausedReason: string | undefined;

  switch (ev.kind) {
    case "text_delta": {
      if (last && last.role === "assistant") {
        next[next.length - 1] = { ...last, content: last.content + ev.text };
      } else {
        next.push({ id: uid(), role: "assistant", content: ev.text });
      }
      break;
    }
    case "reasoning_delta": {
      if (last && last.role === "reasoning") {
        next[next.length - 1] = { ...last, content: last.content + ev.text };
      } else {
        next.push({ id: uid(), role: "reasoning", content: ev.text, reasoningDone: false });
      }
      break;
    }
    case "tool_call_start": {
      next.push({
        id: ev.id,
        role: "tool",
        content: "",
        toolName: ev.name,
        toolId: ev.id,
        toolArgs: "",
        startTs: Date.now(),
      });
      break;
    }
    case "tool_call_delta": {
      const i = next.findIndex((m) => m.toolId === ev.id);
      if (i !== -1) {
        const t = next[i];
        next[i] = { ...t, toolArgs: (t.toolArgs ?? "") + ev.args_delta };
      }
      break;
    }
    case "tool_call_end": {
      const i = next.findIndex((m) => m.toolId === ev.id);
      if (i !== -1) {
        next[i] = {
          ...next[i],
          toolName: ev.name,
          toolArgs: ev.arguments,
          endTs: Date.now(),
        };
      }
      break;
    }
    case "tool_result": {
      const i = next.findIndex((m) => m.toolId === ev.call_id);
      if (i !== -1) {
        next[i] = { ...next[i], toolResult: ev.result };
      }
      break;
    }
    case "done": {
      // reasoning 消息收尾
      if (last && last.role === "reasoning") {
        next[next.length - 1] = { ...last, reasoningDone: true };
      }
      // done.text 是全量最终文本（后端累积），直接替换最新 assistant 消息，
      // 避免与已聚合的 delta 重复
      const lastIdx = next.length - 1;
      const tail = next[lastIdx];
      if (ev.text) {
        if (tail && tail.role === "assistant") {
          next[lastIdx] = { ...tail, content: ev.text };
        } else {
          next.push({ id: uid(), role: "assistant", content: ev.text });
        }
      }
      finished = true;
      break;
    }
    case "paused": {
      finished = true;
      pausedReason = ev.reason;
      break;
    }
    case "error": {
      next.push({ id: uid(), role: "error", content: ev.message });
      finished = true;
      break;
    }
    default:
      break;
  }

  return { messages: next, finished, pausedReason };
}
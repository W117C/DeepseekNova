/**
 * bus.ts — WireEvent 信号总线。
 *
 * 把 Rust 后端通过 Tauri Channel 推送的 WireEvent 流，转成 Solid 可订阅的信号。
 * 组件通过 onEvent() 订阅任意消息类型；bus 内部维护事件累积与工具调用状态，
 * 供会话转录（Transcript）与工具调用面板消费。
 */

import { createSignal } from "solid-js";
import type { WireEvent, UsageInfo, ApprovalRequest } from "../types";

export type { WireEvent, UsageInfo, ApprovalRequest };

interface BusState {
  /** 消息事件（text_delta / reasoning_delta 聚合为完整消息） */
  events: WireEvent[];
  /** 当前正在聚合的助手消息（增量累积） */
  running: boolean;
  phase: "idle" | "thinking" | "reasoning" | "replying" | "done" | "stopped";
  lastUsage: UsageInfo | null;
  pendingApproval: ApprovalRequest | null;
  toolCalls: Map<string, { name: string; args: string; result?: string }>;
  turnCount: number;
}

// 模块级信号：无组件生命周期。
const [state, setState] = createSignal<BusState>({
  events: [],
  running: false,
  phase: "idle",
  lastUsage: null,
  pendingApproval: null,
  toolCalls: new Map(),
  turnCount: 0,
});

const [handlers, setHandlers] = createSignal<Record<string, ((ev: WireEvent) => void)[]>>({});

function emit(kind: string, ev: WireEvent) {
  handlers()[kind]?.forEach((fn) => fn(ev));
  // "*" 通配订阅者接收所有事件
  handlers()["*"]?.forEach((fn) => fn(ev));
}

function push(ev: WireEvent) {
  setState((s) => {
    const next: BusState = { ...s, events: [...s.events, ev] };
    switch (ev.kind) {
      case "text_delta":
      case "reasoning_delta":
        next.phase = ev.kind === "text_delta" ? "replying" : "thinking";
        next.running = true;
        break;
      case "tool_call_start":
      case "tool_call_delta":
      case "tool_call_end":
      case "tool_result":
        next.running = true;
        next.toolCalls = new Map(s.toolCalls);
        if (ev.kind === "tool_call_start") {
          next.toolCalls.set(ev.id, { name: ev.name, args: "" });
        } else if (ev.kind === "tool_call_delta") {
          const t = next.toolCalls.get(ev.id);
          if (t) next.toolCalls.set(ev.id, { ...t, args: t.args + ev.args_delta });
        } else if (ev.kind === "tool_call_end") {
          next.toolCalls.set(ev.id, { name: ev.name, args: ev.arguments });
        } else {
          const t = next.toolCalls.get(ev.call_id);
          if (t) next.toolCalls.set(ev.call_id, { ...t, result: ev.result });
        }
        break;
      case "usage":
        next.lastUsage = {
          prompt_tokens: ev.prompt_tokens,
          completion_tokens: ev.completion_tokens,
          total_tokens: ev.total_tokens,
          cache_hit_tokens: ev.cache_hit_tokens,
          cache_miss_tokens: ev.cache_miss_tokens,
          reasoning_tokens: ev.reasoning_tokens,
          session_cache_hit_tokens: ev.session_cache_hit_tokens,
          session_cache_miss_tokens: ev.session_cache_miss_tokens,
        };
        break;
      case "approval_request":
        next.pendingApproval = {
          id: ev.id,
          title: ev.title,
          description: ev.description,
        };
        break;
      case "turn_complete":
        next.turnCount = s.turnCount + 1;
        break;
      case "done":
        next.phase = "done";
        next.running = false;
        break;
      case "error":
        next.phase = "stopped";
        next.running = false;
        break;
      default:
        break;
    }
    return next;
  });
  emit(ev.kind, ev);
}

function reset() {
  setState((s) => ({
    ...s,
    events: [],
    running: false,
    phase: "idle",
    lastUsage: null,
    pendingApproval: null,
    toolCalls: new Map(),
    turnCount: 0,
  }));
}

/** 订阅指定消息类型；返回取消订阅函数 */
function subscribe(kind: string, fn: (ev: WireEvent) => void): () => void {
  setHandlers((h) => ({ ...h, [kind]: [...(h[kind] ?? []), fn] }));
  return () =>
    setHandlers((h) => ({ ...h, [kind]: (h[kind] ?? []).filter((f) => f !== fn) }));
}

/** 订阅总线状态（组件外用）——返回 state 的 getter，需以 useBus()() 读取或经 createMemo 派生 */
export function useBus(): () => BusState {
  return state;
}

/** 订阅特定消息类型的事件流 */
export function onEvent(kind: string, fn: (ev: WireEvent) => void): () => void {
  return subscribe(kind, fn);
}

/** 推入一个 WireEvent（由 bridge.submitPrompt 的 Channel 回调调用） */
export function pushEvent(ev: WireEvent) {
  push(ev);
}

/** 清空当前会话事件与状态 */
export function resetBus() {
  reset();
}

/** 手动设置审批结果（respond_approval 后清理挂起审批） */
export function clearPendingApproval() {
  setState((s: BusState) => ({ ...s, pendingApproval: null }));
}

export { state as busState, setState as setBusState };
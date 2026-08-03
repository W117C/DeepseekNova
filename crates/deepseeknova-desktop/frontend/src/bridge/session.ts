/**
 * session.ts — 会话级 Solid hooks。
 *
 * 封装 bridge.ts 的 submitPrompt（Channel 事件流）+ 会话 CRUD。
 *
 * 关键设计（Bugbot 审查后重构）：
 * - sendPrompt 在 invoke 回调内**直接调用纯聚合函数**，不再经 bus 订阅中转，
 *   避免「订阅在流式事件到达前被销毁」的竞态（C1）。
 * - done 携带后端累积的全量文本，直接替换而非追加，避免内容翻倍（H1）。
 * - running 标记立即置 true（同步），杜绝双击提交并发（M2）。
 * - 会话切换时调用 clearSessionMessages 清理模块级消息（M1）。
 */

import { createSignal, createResource, createMemo } from "solid-js";
import {
  submitPrompt as invokeSubmit,
  cancelRun as invokeCancel,
  respondApproval,
  listSessions,
  createSession,
  deleteSession,
  renameSession,
  type SessionInfo,
} from "../bridge.ts";
import { pushEvent, resetBus, clearPendingApproval, useBus } from "./bus";
import { aggregateMessages } from "./aggregate";
import type { Message, WireEvent, SubmitRequest } from "../types";

// ── 消息状态（模块级：会话页面消费）──────────────────────────────

interface MessageState {
  messages: Message[];
  /** 当前是否有运行中的 run（同步守卫，杜绝双击提交） */
  running: boolean;
  /** paused 的 reason（供 UI 展示可恢复提示） */
  pausedReason: string | null;
}

const [msg, setMsg] = createSignal<MessageState>({
  messages: [],
  running: false,
  pausedReason: null,
});

function applyEvent(ev: WireEvent) {
  setMsg((s) => {
    const { messages, finished, pausedReason } = aggregateMessages(s.messages, ev);
    return {
      messages,
      running: finished ? false : s.running,
      pausedReason: pausedReason ?? s.pausedReason,
    };
  });
}

// ── 会话 hooks ────────────────────────────────────────────────────────

export function useMessages() {
  return msg;
}

export function useRunning() {
  return createMemo(() => msg().running);
}

export function usePhase() {
  return createMemo(() => useBus()().phase);
}

export function usePendingApproval() {
  return createMemo(() => useBus()().pendingApproval);
}

export function useToolCalls() {
  return createMemo(() => useBus()().toolCalls);
}

export function usePausedReason() {
  return createMemo(() => msg().pausedReason);
}

/** 提交一条用户消息；invoke 回调内同步聚合事件流。 */
export async function sendPrompt(request: Omit<SubmitRequest, "prompt"> & { prompt: string }) {
  resetBus();
  setMsg({ messages: [], running: true, pausedReason: null });

  await invokeSubmit(request, {
    onText: (text) => applyEvent({ kind: "text_delta", text }),
    onReasoning: (text, signature) => applyEvent({ kind: "reasoning_delta", text, signature: signature ?? null }),
    onToolCallStart: (id, name) => applyEvent({ kind: "tool_call_start", id, name }),
    onToolCallDelta: (id, argsDelta) => applyEvent({ kind: "tool_call_delta", id, args_delta: argsDelta }),
    onToolCallEnd: (id, name, arguments_) => applyEvent({ kind: "tool_call_end", id, name, arguments: arguments_ }),
    onToolResult: (callId, result) => applyEvent({ kind: "tool_result", call_id: callId, result }),
    onUsage: (usage) => pushEvent({ kind: "usage", ...usage }),
    onApprovalRequest: (req) => pushEvent({ kind: "approval_request", id: req.id, title: req.title, description: req.description }),
    onPaused: (ev) => applyEvent({ kind: "paused", reason: ev.reason, session_id: ev.session_id }),
    onDone: (text, usage) => {
      applyEvent({ kind: "done", text, usage: usage ?? null });
      pushEvent({ kind: "done", text, usage: usage ?? null });
    },
    onError: (message) => {
      applyEvent({ kind: "error", message });
      pushEvent({ kind: "error", message });
    },
    onTurnComplete: () => pushEvent({ kind: "turn_complete" }),
  });
}

export async function cancelRun() {
  await invokeCancel();
  applyEvent({ kind: "done", text: "", usage: null });
  pushEvent({ kind: "done", text: "", usage: null });
}

export async function approve(requestId: string, approved: boolean) {
  await respondApproval(requestId, approved);
  clearPendingApproval();
}

// ── 会话列表 CRUD ─────────────────────────────────────────────────────

export function useSessionList() {
  return createResource(() => listSessions(), { initialValue: [] as SessionInfo[] });
}

export function useCreateSession() {
  return async (title?: string) => await createSession(title);
}

export function useDeleteSession() {
  return async (id: string) => await deleteSession(id);
}

export function useRenameSession() {
  return async (id: string, title: string) => await renameSession(id, title);
}

/** 会话切换/卸载时清理模块级消息与总线（防止串会话显示陈旧转录） */
export function clearSessionMessages() {
  setMsg({ messages: [], running: false, pausedReason: null });
  resetBus();
}

export { useBus };
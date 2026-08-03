/**
 * session.ts — 会话级 Solid hooks。
 *
 * 封装 bridge.ts 的 submitPrompt（Channel 事件流）+ 会话 CRUD，
 * 将 WireEvent 流聚合为消息列表信号，供会话页面消费。
 */

import { createSignal, createResource, createMemo, onCleanup } from "solid-js";
import { submitPrompt as invokeSubmit, cancelRun as invokeCancel, respondApproval, listSessions, createSession, deleteSession, renameSession, type SessionInfo } from "../bridge";
import { pushEvent, resetBus, clearPendingApproval, useBus, onEvent } from "./bus";
import type { Message, WireEvent, SubmitRequest } from "../types";

// ── 消息聚合 ──────────────────────────────────────────────────────────

interface MessageState {
  messages: Message[];
}

const [msg, setMsg] = createSignal<MessageState>({ messages: [] });

function aggregate(ev: WireEvent) {
  setMsg((s) => {
    const messages = [...s.messages];
    const last = messages[messages.length - 1];

    switch (ev.kind) {
      case "text_delta": {
        if (last && last.role === "assistant") {
          last.content += ev.text;
        } else {
          messages.push({
            id: `msg-${messages.length}`,
            role: "assistant",
            content: ev.text,
          });
        }
        break;
      }
      case "reasoning_delta": {
        if (last && last.role === "reasoning") {
          last.content += ev.text;
        } else {
          messages.push({
            id: `msg-${messages.length}`,
            role: "reasoning",
            content: ev.text,
            reasoningDone: false,
          });
        }
        break;
      }
      case "tool_call_start": {
        messages.push({
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
        const t = messages.find((m) => m.toolId === ev.id);
        if (t) t.toolArgs = (t.toolArgs ?? "") + ev.args_delta;
        break;
      }
      case "tool_call_end": {
        const t = messages.find((m) => m.toolId === ev.id);
        if (t) {
          t.toolName = ev.name;
          t.toolArgs = ev.arguments;
          t.endTs = Date.now();
        }
        break;
      }
      case "tool_result": {
        const t = messages.find((m) => m.toolId === ev.call_id);
        if (t) t.toolResult = ev.result;
        break;
      }
      case "done": {
        if (last && last.role === "reasoning") last.reasoningDone = true;
        if (ev.text) {
          if (last && last.role === "assistant") {
            last.content += ev.text;
          } else {
            messages.push({ id: `msg-${messages.length}`, role: "assistant", content: ev.text });
          }
        }
        break;
      }
      case "error": {
        messages.push({ id: `msg-${messages.length}`, role: "error", content: ev.message });
        break;
      }
      default:
        break;
    }
    return { messages };
  });
}

// ── 会话 hooks ────────────────────────────────────────────────────────

export function useMessages() {
  return msg;
}

export function useRunning() {
  return createMemo(() => useBus()().running);
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

/** 提交一条用户消息；自动订阅后端事件流聚合消息 */
export async function sendPrompt(request: Omit<SubmitRequest, "prompt"> & { prompt: string }) {
  resetBus();
  setMsg({ messages: [] });

  const unsub = onEvent("*", aggregate);
  try {
    await invokeSubmit(request, {
      onText: (text) => pushEvent({ kind: "text_delta", text }),
      onReasoning: (text, signature) =>
        pushEvent({ kind: "reasoning_delta", text, signature: signature ?? null }),
      onToolCallStart: (id, name) => pushEvent({ kind: "tool_call_start", id, name }),
      onToolCallDelta: (id, argsDelta) =>
        pushEvent({ kind: "tool_call_delta", id, args_delta: argsDelta }),
      onToolCallEnd: (id, name, arguments_) =>
        pushEvent({ kind: "tool_call_end", id, name, arguments: arguments_ }),
      onToolResult: (callId, result) =>
        pushEvent({ kind: "tool_result", call_id: callId, result }),
      onUsage: (usage) => pushEvent({ kind: "usage", ...usage }),
      onApprovalRequest: (req) =>
        pushEvent({ kind: "approval_request", id: req.id, title: req.title, description: req.description }),
      onDone: (text, usage) =>
        pushEvent({ kind: "done", text, usage: usage ?? null }),
      onError: (message) => pushEvent({ kind: "error", message }),
      onTurnComplete: () => pushEvent({ kind: "turn_complete" }),
    });
  } finally {
    unsub();
  }
}

export async function cancelRun() {
  await invokeCancel();
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

// 清理函数（供会话切换/卸载时调用）
export function clearSessionMessages() {
  setMsg({ messages: [] });
  resetBus();
}

export { useBus };
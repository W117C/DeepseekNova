/**
 * bridge.ts — Tauri IPC bridge for DPronix desktop frontend.
 *
 * Inspired by DeepSeek-DPronix desktop/frontend/src/lib/bridge.ts
 * but adapted from Wails (`window.go.main.App.*`) to Tauri (`invoke()`).
 *
 * Architecture:
 *   Commands → invoke("command_name", args) → Rust handler
 *   Events  → Channel<WireEvent>.onmessage → React reducer
 */

import { invoke, Channel } from "@tauri-apps/api/core";
import type { WireEvent, SubmitRequest, SkillSummary, ProviderSummary, UsageInfo } from "./types";

// ---------------------------------------------------------------------------
// Event handlers — frontend subscribes to these callbacks from submitPrompt
// ---------------------------------------------------------------------------
export interface EventHandlers {
  onText?: (text: string) => void;
  onReasoning?: (text: string, signature?: string) => void;
  onToolCallStart?: (id: string, name: string) => void;
  onToolCallDelta?: (id: string, argsDelta: string) => void;
  onToolCallEnd?: (id: string, name: string, arguments_: string) => void;
  onToolResult?: (callId: string, result: string) => void;
  onUsage?: (usage: UsageInfo) => void;
  onTurnComplete?: () => void;
  onDone?: (text: string, usage?: UsageInfo) => void;
  onError?: (message: string) => void;
}

// ---------------------------------------------------------------------------
// Commands (matching commands.rs)
// ---------------------------------------------------------------------------

/** Submit a prompt and stream WireEvent chunks back. */
export async function submitPrompt(
  request: SubmitRequest,
  handlers: EventHandlers,
): Promise<void> {
  const channel = new Channel<WireEvent>();

  channel.onmessage = (event: WireEvent) => {
    switch (event.kind) {
      case "text_delta":
        handlers.onText?.(event.text);
        break;
      case "reasoning_delta":
        handlers.onReasoning?.(event.text, event.signature);
        break;
      case "tool_call_start":
        handlers.onToolCallStart?.(event.id, event.name);
        break;
      case "tool_call_delta":
        handlers.onToolCallDelta?.(event.id, event.args_delta);
        break;
      case "tool_call_end":
        handlers.onToolCallEnd?.(event.id, event.name, event.arguments);
        break;
      case "tool_result":
        handlers.onToolResult?.(event.call_id, event.result);
        break;
      case "usage":
        handlers.onUsage?.({
          prompt_tokens: event.prompt_tokens,
          completion_tokens: event.completion_tokens,
          total_tokens: event.total_tokens,
          cache_hit_tokens: event.cache_hit_tokens,
          cache_miss_tokens: event.cache_miss_tokens,
        });
        break;
      case "turn_complete":
        handlers.onTurnComplete?.();
        break;
      case "done":
        handlers.onDone?.(event.text, event.usage);
        break;
      case "error":
        handlers.onError?.(event.message);
        break;
    }
  };

  await invoke("submit_prompt", { request, onEvent: channel });
}

/** Cancel the current agent run. */
export async function cancelRun(): Promise<void> {
  await invoke("cancel_run");
}

/** List available skills. */
export async function listSkills(): Promise<SkillSummary[]> {
  return await invoke("list_skills");
}

/** List configured providers. */
export async function listProviders(): Promise<ProviderSummary[]> {
  return await invoke("list_providers");
}

/** Get current config as JSON string. */
export async function getConfig(): Promise<string> {
  return await invoke("get_config");
}

/** Get agent capabilities. */
export async function getCapabilities(): Promise<Record<string, unknown>> {
  return await invoke("get_capabilities");
}

/** Health check. */
export async function healthCheck(): Promise<string> {
  return await invoke("health_check");
}

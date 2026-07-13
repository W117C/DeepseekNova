/**
 * types.ts — Wire contract matching commands.rs.
 * Mirrors the approach of DeepSeek-Reasonix desktop/frontend/src/lib/types.ts
 * but simplified for our Tauri backend.
 */

/** A single event pushed from the Rust backend through a Tauri Channel. */
export type WireEvent =
  | { kind: "text_delta"; text: string }
  | { kind: "reasoning_delta"; text: string; signature?: string }
  | { kind: "tool_call_start"; id: string; name: string }
  | { kind: "tool_call_delta"; id: string; args_delta: string }
  | { kind: "tool_call_end"; id: string; name: string; arguments: string }
  | { kind: "tool_result"; call_id: string; result: string }
  | {
      kind: "usage";
      prompt_tokens: number;
      completion_tokens: number;
      total_tokens: number;
      cache_hit_tokens: number;
      cache_miss_tokens: number;
    }
  | { kind: "turn_complete" }
  | { kind: "done"; text: string; usage?: UsageInfo }
  | { kind: "error"; message: string };

export interface UsageInfo {
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
  cache_hit_tokens: number;
  cache_miss_tokens: number;
}

export interface SubmitRequest {
  prompt: string;
  model?: string;
  reasoning_effort?: string;
  thinking_enabled?: boolean;
}

export interface SkillSummary {
  name: string;
  description: string;
  tools_allowed: string[];
}

export interface ProviderSummary {
  name: string;
  kind: string;
  model?: string;
  base_url?: string;
}

export interface Capabilities {
  version: string;
  supports_thinking: boolean;
  supports_reasoning_effort: boolean;
  supports_tools: boolean;
  supports_mcp: boolean;
  supports_images: boolean;
  max_steps_default: number;
  reasoning_effort_levels: string[];
}

/** One message in the conversation transcript. */
export type MessageRole = "user" | "assistant" | "reasoning" | "tool";

export interface Message {
  id: string;
  role: MessageRole;
  content: string;
  toolName?: string;
  toolId?: string;
  toolArgs?: string;
  toolResult?: string;
  reasoningDone?: boolean;
}

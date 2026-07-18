/**
 * types.ts — Wire contract matching commands.rs.
 *
 * Enhanced with Mode/Effort/Session/MCP/Config types
 * while preserving Reasonix UI types (ApprovalRequest, ToolCall, etc.).
 */

// ── Mode & Effort ──────────────────────────────────────────────
export type Mode = "plan" | "act" | "yolo";
export type Effort = "low" | "medium" | "high" | "max";

// ── Wire Events (from Rust backend) ────────────────────────────
export interface UsageInfo {
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
  cache_hit_tokens: number;
  cache_miss_tokens: number;
  reasoning_tokens: number;
}

export type WireEvent =
  | { kind: "text_delta"; text: string }
  | { kind: "reasoning_delta"; text: string; signature?: string }
  | { kind: "tool_call_start"; id: string; name: string }
  | { kind: "tool_call_delta"; id: string; args_delta: string }
  | { kind: "tool_call_end"; id: string; name: string; arguments: string }
  | { kind: "tool_result"; call_id: string; result: string }
  | { kind: "mode_change"; mode: Mode; auto_mode: boolean }
  | { kind: "mcp_status"; server: string; status: "connected" | "disconnected" | "error"; tools: number }
  | { kind: "session_saved"; session_id: string; title: string }
  | { kind: "approval_request"; id: string; title: string; description?: string }
  | {
      kind: "usage";
      prompt_tokens: number;
      completion_tokens: number;
      total_tokens: number;
      cache_hit_tokens: number;
      cache_miss_tokens: number;
      reasoning_tokens: number;
    }
  | { kind: "turn_complete" }
  | { kind: "done"; text: string; usage?: UsageInfo }
  | { kind: "error"; message: string };

// ── Submit Request ──────────────────────────────────────────────
export interface SubmitRequest {
  prompt: string;
  model?: string;
  reasoning_effort?: Effort;
  thinking_enabled?: boolean;
  mode?: Mode;
  auto_mode?: boolean;
  max_steps?: number;
}

// ── Skills ─────────────────────────────────────────────────────
export interface SkillSummary {
  name: string;
  description: string;
  tools_allowed: string[];
}

// ── Providers ──────────────────────────────────────────────────
export interface ProviderSummary {
  name: string;
  kind: string;
  model?: string;
  base_url?: string;
  connected: boolean;
}

// ── Capabilities ──────────────────────────────────────────────
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

// ── Sessions ──────────────────────────────────────────────────
export interface SessionSummary {
  id: string;
  title: string;
  active?: boolean;
  created_at?: number;
  updated_at?: number;
  message_count?: number;
  preview?: string;
}

// ── MCP ───────────────────────────────────────────────────────
export interface McpTool {
  name: string;
  description: string;
  input_schema?: string;
}

export interface McpServer {
  name: string;
  command: string;
  args: string[];
  env: Record<string, string>;
  status: "connected" | "disconnected" | "error";
  tool_count: number;
  tools: McpTool[];
  error?: string;
}

// ── Model Config ──────────────────────────────────────────────
export interface ModelConfig {
  provider: string;
  model: string;
  api_key: string;
  base_url: string;
  temperature: number;
  max_tokens: number;
}

// ── App Config ────────────────────────────────────────────────
export interface AppConfig {
  default_mode: Mode;
  default_effort: Effort;
  thinking_enabled: boolean;
  auto_mode: boolean;
  max_steps: number;
  theme: "dark" | "light";
  models: ModelConfig[];
}

export const DEFAULT_CONFIG: AppConfig = {
  default_mode: "act",
  default_effort: "high",
  thinking_enabled: true,
  auto_mode: false,
  max_steps: 50,
  theme: "dark",
  models: [],
};

// ── Messages ──────────────────────────────────────────────────
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
  timestamp?: number;
}

// ── Reasonix UI types ────────────────────────────────────────
export type ToolStatus = "running" | "success" | "error";
export interface ToolCall {
  id: string; command: string; status: ToolStatus; detail?: string; durationMs?: number;
}
export type PlanStepStatus = "done" | "active" | "pending";
export interface PlanStep { id: string; title: string; status: PlanStepStatus; }
export interface ApprovalRequest { id: string; title: string; description?: string; }
export type FileChangeType = "added" | "modified" | "removed";
export interface ContextFile { path: string; changeType?: FileChangeType; }
export type AgentStatus = "ready" | "running";

/**
 * types.ts — Wire contract matching commands.rs and runner.rs WireEvent.
 */

/** A single event pushed from the Rust backend through a Tauri Channel. */
export type WireEvent =
  | { kind: "text_delta"; text: string }
  | { kind: "reasoning_delta"; text: string; signature: string | null }
  | { kind: "tool_call_start"; id: string; name: string }
  | { kind: "tool_call_delta"; id: string; args_delta: string }
  | { kind: "tool_call_end"; id: string; name: string; arguments: string }
  | { kind: "tool_result"; call_id: string; result: string }
  | { kind: "verification"; command: string; passed: boolean; summary: string }
  | {
      kind: "usage";
      prompt_tokens: number;
      completion_tokens: number;
      total_tokens: number;
      cache_hit_tokens: number;
      cache_miss_tokens: number;
      reasoning_tokens: number;
      session_cache_hit_tokens: number;
      session_cache_miss_tokens: number;
    }
  | { kind: "turn_complete" }
  | { kind: "approval_request"; id: string; title: string; description: string | null }
  /** 运行被暂停（max-steps 暂停或预算拒绝），任务可恢复 */
  | { kind: "paused"; reason: string; session_id: string | null }
  | { kind: "done"; text: string; usage: UsageInfo | null }
  | { kind: "error"; message: string };

export interface UsageInfo {
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
  cache_hit_tokens: number;
  cache_miss_tokens: number;
  reasoning_tokens: number;
  session_cache_hit_tokens: number;
  session_cache_miss_tokens: number;
}

export interface SubmitRequest {
  prompt: string;
  model?: string;
  reasoning_effort?: string;
  thinking_enabled?: boolean;
  /** 四档模式（后端可选字段，缺省 agent） */
  agent_mode?: string;
  /** 附件绝对路径列表（后端可选字段） */
  attachments?: string[];
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
  connected?: boolean;
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
export type MessageRole = "user" | "assistant" | "reasoning" | "tool" | "error";

export interface Message {
  id: string;
  role: MessageRole;
  content: string;
  toolName?: string;
  toolId?: string;
  toolArgs?: string;
  toolResult?: string;
  reasoningDone?: boolean;
  /** 工具调用计时（前端事件时间戳派生） */
  startTs?: number;
  endTs?: number;
}

/** A pending tool approval request (Act mode). */
export interface ApprovalRequest {
  id: string;
  title: string;
  description: string | null;
  toolName?: string;
  toolArgs?: string;
}

// ── Desktop-only types (UI state, not wire protocol) ──────────────────────

/** Agent execution mode（mockup 定稿四档：代理/对话/规划/审查） */
export type Mode = "agent" | "chat" | "plan" | "review";

/** Reasoning effort level. */
export type Effort = "low" | "medium" | "high" | "max";

/** Agent runtime status. */
export type AgentStatus = "ready" | "running";

/** File change type for context panel badges. */
export type FileChangeType = "added" | "removed" | "modified";

/** A file entry in the context panel. */
export interface ContextFile {
  path: string;
  changeType?: FileChangeType;
}

/** A session summary for the sidebar. */
export interface SessionSummary {
  id: string;
  title: string;
  active?: boolean;
}

/** MCP server status for settings/sidebar. */
export interface McpServer {
  name: string;
  status: "connected" | "disconnected" | "error";
  command: string;
  args: string[];
  tool_count: number;
  error?: string;
}

/** App config for settings panel. */
export interface AppConfig {
  default_mode: Mode;
  max_steps: number;
  auto_mode: boolean;
}

// ── 扩展类型（UI 层使用）────────────────────────────────────

/** TODO 项 */
export interface TodoItem {
  id: string;
  text: string;
  done: boolean;
  status: "pending" | "in_progress" | "completed";
}

/** 记忆项 */
export interface MemoryItem {
  id: string;
  text: string;
  createdAt: number;
}

/** 文件树节点 */
export interface FileTreeNode {
  name: string;
  path: string;
  isDir: boolean;
  children?: FileTreeNode[];
  gitStatus?: "modified" | "added" | "deleted" | "untracked";
}

/** Slash 命令 */
export interface SlashCommand {
  name: string;
  description: string;
}

/** 工具调用信息 */
export interface ToolCallInfo {
  id: string;
  name: string;
  args: string;
  result?: string;
  status: "running" | "done" | "error";
}

// ── Mockup 移植新增类型（阶段 0）──────────────────────

/** 代码改动文件（get_changed_files） */
export interface ChangedFile {
  path: string;
  tag: "M" | "A" | "D";
  additions: number;
  deletions: number;
}

/** Git 工作树（list_worktrees） */
export interface WorktreeInfo {
  branch: string;
  path: string;
  is_current: boolean;
  dirty?: boolean;
}

/** 附件 chip */
export interface AttachmentInfo {
  path: string;
  name: string;
  size?: number;
}

/** AI 工作阶段（思考中/推理中/回复中） */
export type RunPhase = "idle" | "thinking" | "reasoning" | "replying" | "done" | "stopped";

/** 对比面板的成对 diff 行：
 * ctx 上下文 / del 仅删除 / add 仅新增 / mod 修改（左删右增配对）/ hunk 块分隔 */
export type DiffRowType = "ctx" | "del" | "add" | "mod" | "hunk";
export interface DiffRow {
  type: DiffRowType;
  oldNo: number | null;
  oldText: string;
  newNo: number | null;
  newText: string;
}

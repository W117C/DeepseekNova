/**
 * bridge.ts — Tauri IPC bridge for DPronix desktop frontend.
 *
 * Enhanced with session management, MCP, config, and model switching.
 *
 * Architecture:
 *   Commands → invoke("command_name", args) → Rust handler
 *   Events  → Channel<WireEvent>.onmessage → React reducer
 *   Dev mock → when !window.__TAURI__, simulates a complete agent turn
 */

import type { WireEvent, SubmitRequest, SkillSummary, ProviderSummary, UsageInfo, SessionSummary, McpServer, AppConfig } from "./types";

// ---------------------------------------------------------------------------
// Declare Tauri globals for strict TypeScript
// ---------------------------------------------------------------------------
declare global {
  interface Window {
    __TAURI__?: unknown;
  }
}

// ---------------------------------------------------------------------------
// Detect Tauri shell
// ---------------------------------------------------------------------------
function isTauri(): boolean {
  try {
    return typeof window !== "undefined" && window.__TAURI__ !== undefined;
  } catch {
    return false;
  }
}

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
  onModeChange?: (mode: string, autoMode: boolean) => void;
  onMcpStatus?: (server: string, status: string, tools: number) => void;
  onSessionSaved?: (sessionId: string, title: string) => void;
  onUsage?: (usage: UsageInfo) => void;
  onTurnComplete?: () => void;
  onApprovalRequest?: (request: { id: string; title: string; description?: string }) => void;
  onDone?: (text: string, usage?: UsageInfo) => void;
  onError?: (message: string) => void;
}

// ---------------------------------------------------------------------------
// Dev mock — simulates a full agent turn for frontend-only development
// ---------------------------------------------------------------------------
async function devMockSubmit(
  _request: SubmitRequest,
  handlers: EventHandlers,
): Promise<void> {
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
  const uid = () => (Date.now() + Math.random()).toString(36);
  const toolId = () => `call_${uid()}`;

  // 1. Reasoning delta (simulated thinking)
  await sleep(300);
  handlers.onReasoning?.("分析中：用户需要了解 Rust 的所有权系统", "sig_dev");

  await sleep(400);
  handlers.onReasoning?.("所有权是 Rust 最核心的概念，它保证了内存安全", "sig_dev");

  // 2. Text deltas (streaming the response)
  await sleep(200);
  const fullText = [
    "## Rust 所有权系统\n\n",
    "Rust 的所有权（Ownership）系统是语言最独特的特性，它在**编译时**保证内存安全，无需垃圾回收器。\n\n",
    "### 三条核心规则\n\n",
    "1. **每个值在 Rust 中都有一个所有者（owner）**\n",
    "2. **同一时间只能有一个所有者**\n",
    "3. **当所有者离开作用域，值将被丢弃**\n\n",
    "### 示例\n\n```rust\nfn main() {\n    let s1 = String::from(\"hello\");\n    let s2 = s1; // s1 的所有权移动到 s2\n    // println!(\"{}\", s1); // ❌ 编译错误\n    println!(\"{}\", s2); // ✅ 正确\n}\n```\n\n",
    "需要我帮你写更多的 Rust 代码示例吗？",
  ];

  for (const chunk of fullText) {
    await sleep(50 + Math.random() * 80);
    handlers.onText?.(chunk);
  }

  // 3. Tool call sequence (simulated)
  await sleep(300);

  // Tool call 1: read_file
  const t1 = toolId();
  handlers.onToolCallStart?.(t1, "read_file");
  await sleep(100);
  handlers.onToolCallDelta?.(t1, JSON.stringify({ path: "src/main.rs" }));
  await sleep(200);
  handlers.onToolCallEnd?.(t1, "read_file", JSON.stringify({ path: "src/main.rs" }));
  await sleep(100);
  handlers.onToolResult?.(t1, 'fn main() {\n    println!("Hello, world!");\n}\n');

  // 4. More text after tool results
  await sleep(200);
  handlers.onText?.("\n\n我已经读取了 `src/main.rs`。这是一个简单的 Hello World 程序。");

  // 5. Usage info
  await sleep(100);
  handlers.onUsage?.({
    prompt_tokens: 452,
    completion_tokens: 318,
    total_tokens: 770,
    cache_hit_tokens: 280,
    cache_miss_tokens: 172,
    reasoning_tokens: 96,
  });

  // 6. Done
  await sleep(100);
  handlers.onDone?.(fullText.join(""), {
    prompt_tokens: 452,
    completion_tokens: 318,
    total_tokens: 770,
    cache_hit_tokens: 280,
    cache_miss_tokens: 172,
    reasoning_tokens: 96,
  });
}

// ---------------------------------------------------------------------------
// Tauri IPC imports (lazy — only when in Tauri shell)
// ---------------------------------------------------------------------------
let tauriInvoke: any = null;
type TauriChannelCtor = { new <T>(onmessage?: (response: T) => void): { onmessage: ((event: T) => void) | null } };
let tauriChannel: TauriChannelCtor | null = null;

async function ensureTauriImports() {
  if (!tauriInvoke) {
    const mod = await import("@tauri-apps/api/core");
    tauriInvoke = mod.invoke;
    tauriChannel = mod.Channel;
  }
}

// ---------------------------------------------------------------------------
// Commands (matching commands.rs)
// ---------------------------------------------------------------------------

/** Submit a prompt and stream WireEvent chunks back. */
export async function submitPrompt(
  request: SubmitRequest,
  handlers: EventHandlers,
): Promise<void> {
  if (!isTauri()) {
    return devMockSubmit(request, handlers);
  }

  await ensureTauriImports();
  const Ctor = tauriChannel!;
  const channel = new Ctor<WireEvent>();

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
          reasoning_tokens: event.reasoning_tokens,
        });
        break;
      case "mode_change":
        handlers.onModeChange?.(event.mode, event.auto_mode);
        break;
      case "mcp_status":
        handlers.onMcpStatus?.(event.server, event.status, event.tools);
        break;
      case "session_saved":
        handlers.onSessionSaved?.(event.session_id, event.title);
        break;
      case "turn_complete":
        handlers.onTurnComplete?.();
        break;
      case "approval_request":
        handlers.onApprovalRequest?.({ id: event.id, title: event.title, description: event.description });
        break;
      case "done":
        handlers.onDone?.(event.text, event.usage);
        break;
      case "error":
        handlers.onError?.(event.message);
        break;
    }
  };

  await tauriInvoke("submit_prompt", { request, onEvent: channel });
}

/** Cancel the current agent run. */
export async function cancelRun(): Promise<void> {
  if (!isTauri()) return;
  await ensureTauriImports();
  await tauriInvoke("cancel_run");
}

/** Start a fresh conversation, clearing the backend's persistent history. */
export async function newSession(): Promise<void> {
  if (!isTauri()) return;
  await ensureTauriImports();
  await tauriInvoke("new_session");
}

/** List available skills. */
export async function listSkills(): Promise<SkillSummary[]> {
  if (!isTauri()) return [];
  await ensureTauriImports();
  return await tauriInvoke("list_skills");
}

/** List configured providers. */
export async function listProviders(): Promise<ProviderSummary[]> {
  if (!isTauri()) return [];
  await ensureTauriImports();
  return await tauriInvoke("list_providers");
}

/** Get current config as JSON string. */
export async function getConfig(): Promise<string> {
  if (!isTauri()) return "{}";
  await ensureTauriImports();
  return await tauriInvoke("get_config");
}

/** Get agent capabilities. */
export async function getCapabilities(): Promise<Record<string, unknown>> {
  if (!isTauri()) {
    return {
      version: "dev",
      supports_thinking: true,
      supports_reasoning_effort: true,
      supports_tools: true,
      supports_mcp: false,
      supports_images: false,
      max_steps_default: 20,
      reasoning_effort_levels: ["low", "medium", "high"],
    };
  }
  await ensureTauriImports();
  return await tauriInvoke("get_capabilities");
}

/** Health check. */
export async function healthCheck(): Promise<string> {
  if (!isTauri()) return "dev-mock";
  await ensureTauriImports();
  return await tauriInvoke("health_check");
}

/** Respond to an approval request. */
export async function respondApproval(requestId: string, approved: boolean): Promise<void> {
  if (!isTauri()) return;
  await ensureTauriImports();
  await tauriInvoke("respond_approval", { requestId, approved });
}

/** List workspace files for the Files tab. */
export async function getWorkspaceFiles(): Promise<string[]> {
  if (!isTauri()) return [];
  await ensureTauriImports();
  return await tauriInvoke("get_workspace_files");
}

// ── Session persistence ───────────────────────────────────

export interface SessionInfo {
  id: string;
  title: string;
  message_count: number;
  created_at: string;
}

/** List all conversation sessions. */
export async function listSessions(): Promise<SessionInfo[]> {
  if (!isTauri()) return [{ id: "default", title: "Current Session", message_count: 0, created_at: "" }];
  await ensureTauriImports();
  return await tauriInvoke("list_sessions");
}

/** Create a new session. */
export async function createSession(title?: string): Promise<SessionInfo> {
  if (!isTauri()) return { id: "dev", title: title ?? "Untitled", message_count: 0, created_at: "" };
  await ensureTauriImports();
  return await tauriInvoke("create_session", { title });
}

/** Delete a session by id. */
export async function deleteSession(id: string): Promise<void> {
  if (!isTauri()) return;
  await ensureTauriImports();
  await tauriInvoke("delete_session", { id });
}

// ── Enhanced: Session Management ────────────────────────────

/** Load a session by ID. */
export async function loadSession(id: string): Promise<{ messages: unknown[] }> {
  if (!isTauri()) return { messages: [] };
  await ensureTauriImports();
  return await tauriInvoke("load_session", { id });
}

/** Export a session as markdown/JSON. */
export async function exportSession(id: string): Promise<string> {
  if (!isTauri()) return "";
  await ensureTauriImports();
  return await tauriInvoke("export_session", { id });
}

// ── Enhanced: Config Management ─────────────────────────────

/** Save application config. */
export async function saveConfig(config: AppConfig): Promise<void> {
  if (!isTauri()) return;
  await ensureTauriImports();
  await tauriInvoke("save_config", { config });
}

// ── Enhanced: MCP Server Management ─────────────────────────

/** List MCP servers. */
export async function listMcpServers(): Promise<McpServer[]> {
  if (!isTauri()) return [];
  await ensureTauriImports();
  return await tauriInvoke("list_mcp_servers");
}

/** Add an MCP server. */
export async function addMcpServer(config: {
  name: string;
  command: string;
  args: string[];
  env: Record<string, string>;
}): Promise<void> {
  if (!isTauri()) return;
  await ensureTauriImports();
  await tauriInvoke("add_mcp_server", { config });
}

/** Remove an MCP server by name. */
export async function removeMcpServer(name: string): Promise<void> {
  if (!isTauri()) return;
  await ensureTauriImports();
  await tauriInvoke("remove_mcp_server", { name });
}

// ── Enhanced: Model Switching ────────────────────────────────

/** Switch the active model. */
export async function switchModel(provider: string, model: string): Promise<void> {
  if (!isTauri()) return;
  await ensureTauriImports();
  await tauriInvoke("switch_model", { provider, model });
}

/**
 * bridge.ts — Tauri IPC bridge for DPronix desktop frontend.
 *
 * Architecture (inspired by DeepSeek-Reasonix):
 *   Commands → invoke("command_name", args) → Rust handler
 *   Events  → Channel<WireEvent>.onmessage → React reducer
 *   Dev mock → when !window.__TAURI__, simulates a complete agent turn
 */

import type { WireEvent, SubmitRequest, SkillSummary, ProviderSummary, UsageInfo } from "./types";

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
  onUsage?: (usage: UsageInfo) => void;
  onTurnComplete?: () => void;
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
let tauriChannel: any = null;

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
  const channel = new tauriChannel<WireEvent>();

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

  await tauriInvoke("submit_prompt", { request, onEvent: channel });
}

/** Cancel the current agent run. */
export async function cancelRun(): Promise<void> {
  if (!isTauri()) return;
  await ensureTauriImports();
  await tauriInvoke("cancel_run");
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

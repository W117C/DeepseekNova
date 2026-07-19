/**
 * bridge.ts — Tauri IPC bridge for DeepseekNova desktop frontend.
 *
 * 全部后端命令的 TypeScript 桥接层。
 * Commands → invoke("command_name", args) → Rust handler
 * Events  → Channel<WireEvent>.onmessage → React reducer
 */

import { invoke, Channel } from "@tauri-apps/api/core";
import type { WireEvent, SubmitRequest, SkillSummary, ProviderSummary, UsageInfo, Capabilities } from "./types";

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
  onApprovalRequest?: (req: { id: string; title: string; description: string | null }) => void;
  onDone?: (text: string, usage?: UsageInfo) => void;
  onError?: (message: string) => void;
}

// ---------------------------------------------------------------------------
// Core — Agent interaction
// ---------------------------------------------------------------------------

export async function submitPrompt(request: SubmitRequest, handlers: EventHandlers): Promise<void> {
  const channel = new Channel<WireEvent>();
  channel.onmessage = (event: WireEvent) => {
    switch (event.kind) {
      case "text_delta": handlers.onText?.(event.text); break;
      case "reasoning_delta": handlers.onReasoning?.(event.text, event.signature ?? undefined); break;
      case "tool_call_start": handlers.onToolCallStart?.(event.id, event.name); break;
      case "tool_call_delta": handlers.onToolCallDelta?.(event.id, event.args_delta); break;
      case "tool_call_end": handlers.onToolCallEnd?.(event.id, event.name, event.arguments); break;
      case "tool_result": handlers.onToolResult?.(event.call_id, event.result); break;
      case "usage": handlers.onUsage?.({
        prompt_tokens: event.prompt_tokens, completion_tokens: event.completion_tokens,
        total_tokens: event.total_tokens, cache_hit_tokens: event.cache_hit_tokens,
        cache_miss_tokens: event.cache_miss_tokens, reasoning_tokens: event.reasoning_tokens,
        session_cache_hit_tokens: event.session_cache_hit_tokens,
        session_cache_miss_tokens: event.session_cache_miss_tokens,
      }); break;
      case "turn_complete": handlers.onTurnComplete?.(); break;
      case "approval_request": handlers.onApprovalRequest?.({ id: event.id, title: event.title, description: event.description }); break;
      case "done": handlers.onDone?.(event.text, event.usage ?? undefined); break;
      case "error": handlers.onError?.(event.message); break;
      default: console.warn("Unknown WireEvent kind:", (event as { kind: string }).kind);
    }
  };
  await invoke("submit_prompt", { request, onEvent: channel });
}

export async function cancelRun(): Promise<void> { await invoke("cancel_run"); }
export async function newSession(): Promise<void> { await invoke("new_session"); }
export async function healthCheck(): Promise<string> { return await invoke("health_check"); }

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

export interface SessionInfo { id: string; title: string; message_count: number; created_at: string; }
export async function listSessions(): Promise<SessionInfo[]> { return await invoke("list_sessions"); }
export async function createSession(title?: string): Promise<SessionInfo> { return await invoke("create_session", { title }); }
export async function deleteSession(id: string): Promise<void> { return await invoke("delete_session", { id }); }

// ---------------------------------------------------------------------------
// Skills & Providers
// ---------------------------------------------------------------------------

export async function listSkills(): Promise<SkillSummary[]> { return await invoke("list_skills"); }
export async function listProviders(): Promise<ProviderSummary[]> { return await invoke("list_providers"); }
export async function getConfig(): Promise<string> { return await invoke("get_config"); }
export async function getCapabilities(): Promise<Capabilities> { return await invoke("get_capabilities"); }

// ---------------------------------------------------------------------------
// Workspace
// ---------------------------------------------------------------------------

export async function getWorkspaceFiles(): Promise<string[]> { return await invoke("get_workspace_files"); }
export async function getFileDiff(filePath: string): Promise<string> { return await invoke("get_file_diff", { filePath }); }

// ---------------------------------------------------------------------------
// Approval
// ---------------------------------------------------------------------------

export async function respondApproval(requestId: string, approved: boolean): Promise<void> {
  await invoke("respond_approval", { requestId, approved });
}

// ---------------------------------------------------------------------------
// Sandbox
// ---------------------------------------------------------------------------

export interface SandboxConfig { enabled: boolean; allowed_paths: string[]; blocked_paths: string[]; isolate_env: boolean; csp_enabled: boolean; }
export async function getSandboxConfig(): Promise<SandboxConfig> { return await invoke("get_sandbox_config"); }
export async function setSandboxConfig(config: SandboxConfig): Promise<void> { return await invoke("set_sandbox_config", { config }); }

// ---------------------------------------------------------------------------
// Network
// ---------------------------------------------------------------------------

export interface NetworkConfig { allow_network: boolean; proxy: string | null; timeout_secs: number; max_retries: number; ssl_verify: boolean; auto_reconnect: boolean; }
export async function getNetworkConfig(): Promise<NetworkConfig> { return await invoke("get_network_config"); }
export async function setNetworkConfig(config: NetworkConfig): Promise<void> { return await invoke("set_network_config", { config }); }
export async function networkDiagnostics(): Promise<any> { return await invoke("network_diagnostics"); }

// ---------------------------------------------------------------------------
// Permissions
// ---------------------------------------------------------------------------

export interface PermissionRule { name: string; description: string; enabled: boolean; rule_type: string; }
export async function getPermissions(): Promise<PermissionRule[]> { return await invoke("get_permissions"); }
export async function setPermissionRule(name: string, enabled: boolean): Promise<void> { return await invoke("set_permission_rule", { name, enabled }); }

// ---------------------------------------------------------------------------
// Hooks
// ---------------------------------------------------------------------------

export interface Hook { event: string; command: string; enabled: boolean; }
export async function getHooks(): Promise<Hook[]> { return await invoke("get_hooks"); }
export async function setHook(event: string, command: string, enabled: boolean): Promise<void> { return await invoke("set_hook", { event, command, enabled }); }
export async function deleteHook(event: string): Promise<void> { return await invoke("delete_hook", { event }); }

// ---------------------------------------------------------------------------
// MCP
// ---------------------------------------------------------------------------

export interface McpServer { name: string; command: string; args: string; transport: string; status: string; }
export async function listMcpServers(): Promise<McpServer[]> { return await invoke("list_mcp_servers"); }
export async function addMcpServer(server: McpServer): Promise<void> { return await invoke("add_mcp_server", { server }); }
export async function removeMcpServer(name: string): Promise<void> { return await invoke("remove_mcp_server", { name }); }
export async function toggleMcpServer(name: string, start: boolean): Promise<void> { return await invoke("toggle_mcp_server", { name, start }); }

// ---------------------------------------------------------------------------
// Sub-Agents
// ---------------------------------------------------------------------------

export interface SubAgent { name: string; description: string; model: string; status: string; tasks: number; }
export async function listSubagents(): Promise<SubAgent[]> { return await invoke("list_subagents"); }

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

export async function runDiagnostics(): Promise<any> { return await invoke("run_diagnostics"); }

// ---------------------------------------------------------------------------
// Billing
// ---------------------------------------------------------------------------

export async function getBillingStats(): Promise<any> { return await invoke("get_billing_stats"); }

// ---------------------------------------------------------------------------
// Knowledge Base
// ---------------------------------------------------------------------------

export async function getWikiPages(): Promise<any> { return await invoke("get_wiki_pages"); }
export async function getKnowledgeCards(): Promise<any> { return await invoke("get_knowledge_cards"); }

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

export interface MemoryEntry { id: string; memory_type: string; text: string; created_at: string; }
export async function getMemories(): Promise<MemoryEntry[]> { return await invoke("get_memories"); }
export async function addMemory(memoryType: string, text: string): Promise<MemoryEntry> { return await invoke("add_memory", { memoryType, text }); }
export async function deleteMemory(id: string): Promise<void> { return await invoke("delete_memory", { id }); }

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

export async function saveSettings(settings: any): Promise<void> { return await invoke("save_settings", { settings }); }
export async function loadSettings(): Promise<any> { return await invoke("load_settings"); }

// ---------------------------------------------------------------------------
// Shortcuts
// ---------------------------------------------------------------------------

export async function getShortcuts(): Promise<any> { return await invoke("get_shortcuts"); }

// ---------------------------------------------------------------------------
// Update
// ---------------------------------------------------------------------------

export async function checkForUpdates(): Promise<any> { return await invoke("check_for_updates"); }

// ---------------------------------------------------------------------------
// Tabs
// ---------------------------------------------------------------------------

export interface TabInfo { id: string; title: string; }
export async function listTabs(): Promise<TabInfo[]> { return await invoke("list_tabs"); }
export async function createTab(title: string): Promise<TabInfo> { return await invoke("create_tab", { title }); }
export async function closeTab(id: string): Promise<void> { return await invoke("close_tab", { id }); }

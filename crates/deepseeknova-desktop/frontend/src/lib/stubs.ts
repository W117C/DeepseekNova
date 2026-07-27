/**
 * lib/stubs.ts — 前后端解耦降级层
 *
 * invokeOrStub(cmd, args, stubData)：后端命令尚未落地（invoke 抛
 * "command not found"）时返回 stub 数据并 console.warn，保证前端
 * 各阶段可独立开发与验收。后端命令落地后无需改前端代码。
 *
 * VITE_USE_STUBS=1 可强制走 stub（跳过 invoke），用于纯前端联调。
 */

import { invoke } from "@tauri-apps/api/core";

const FORCE_STUBS =
  (import.meta as unknown as { env?: Record<string, string> }).env?.VITE_USE_STUBS === "1";

/** 已警告过的命令，避免刷屏 */
const warned = new Set<string>();

function isCommandNotFound(err: unknown): boolean {
  const msg = String(err ?? "");
  return (
    msg.includes("command not found") ||
    msg.includes("Command not found") ||
    msg.includes("not found: ") ||
    msg.includes("unknown command")
  );
}

export async function invokeOrStub<T>(
  cmd: string,
  args: Record<string, unknown> | undefined,
  stubData: T | (() => T)
): Promise<T> {
  const stub = () => (typeof stubData === "function" ? (stubData as () => T)() : stubData);
  if (FORCE_STUBS) return stub();
  try {
    return await invoke<T>(cmd, args);
  } catch (err) {
    if (isCommandNotFound(err)) {
      if (!warned.has(cmd)) {
        warned.add(cmd);
        console.warn(`[stubs] 后端命令未实现，走 stub：${cmd}`, err);
      }
      return stub();
    }
    throw err;
  }
}

// ── 各缺失命令的默认 stub 数据（后端落地后自动失效） ──────────

import type { ChangedFile, WorktreeInfo } from "../types";

export const STUB_CHANGED_FILES: ChangedFile[] = [
  { path: "runtime/src/lib.rs", tag: "M", additions: 56, deletions: 4 },
  { path: "agent/src/agent.rs", tag: "M", additions: 114, deletions: 23 },
  { path: "serve/src/approval.rs", tag: "A", additions: 72, deletions: 0 },
];

export const STUB_WORKTREES: WorktreeInfo[] = [
  { branch: "main", path: "", is_current: false, dirty: false },
  { branch: "nova/permission-gate", path: "", is_current: true, dirty: true },
];

export const STUB_DIFF = `diff --git a/runtime/src/lib.rs b/runtime/src/lib.rs
index 1111111..2222222 100644
--- a/runtime/src/lib.rs
+++ b/runtime/src/lib.rs
@@ -239,8 +239,12 @@ pub fn build_agent(
 pub fn build_agent(
     config: &Config,
     ws: &Workspace,
 ) -> Agent {
     let mut agent = Agent::new(config);
-    // (previously: tools executed unconditionally)
+    // Permission gate — opt-in. Reuse the session-cached gate.
+    let gate = gate.or_else(|| permission_gate_for(config, &ws));
+    if let Some(g) = gate {
+        agent = agent.with_permission_gate(g);
+    }
     agent
 }
`;

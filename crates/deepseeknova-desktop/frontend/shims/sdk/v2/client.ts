/**
 * @opencode-ai/sdk/v2/client shim — DeepseekNova 桌面端移植。
 *
 * UI 层对 opencode 的客户端 API 只做类型引用（Message/Session/Part 等）；
 * 真正的数据流由 src/bridge 适配层经 Tauri IPC 提供，因此这里只 re-export
 * 生成类型，并给出 createOpencodeClient 的空实现占位（调用方是 server 层，
 * 移植时会重写，不会在运行时走到这里）。
 */

export * from "./types.gen"

export interface OpencodeClientConfig {
  baseUrl?: string
  fetch?: typeof fetch
  headers?: Record<string, string>
  directory?: string
}

/** 占位：真实数据访问在 src/bridge。调用方会被重写，不应在运行时被调用。 */
export function createOpencodeClient(_config?: OpencodeClientConfig): never {
  throw new Error(
    "createOpencodeClient 不应被调用：DeepseekNova 桌面端数据流经 Tauri IPC（src/bridge）。",
  )
}
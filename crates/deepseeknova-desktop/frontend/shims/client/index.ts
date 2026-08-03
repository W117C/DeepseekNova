/**
 * @opencode-ai/client shim — DeepseekNova 桌面端移植。
 * 顶层入口：仅 re-export promise 兼容面，真实数据流在 src/bridge。
 */

export * from "./promise"

/** opencode client 错误基类（仅被 import 引用，实际错误由 bridge 层归一）。 */
export class ClientError extends Error {
  constructor(message: string, readonly status?: number) {
    super(message)
  }
}
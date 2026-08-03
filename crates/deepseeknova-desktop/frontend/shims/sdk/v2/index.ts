/**
 * @opencode-ai/sdk/v2 shim — DeepseekNova 桌面端移植。
 *
 * UI 层（vendor/app + vendor/session-ui）大量 type-only 引用 opencode 的
 * openapi 生成类型；此处复用 vendor 的 types.gen.ts 保证类型语义一致，
 * 并仅实现 UI 实际用到的一小部分运行时符号。
 *
 * 数据访问不经过 opencode 的 HTTP SDK：全部由 src/bridge 适配层走 Tauri IPC。
 */

export * from "./types.gen"

/** 运行时符号（UI 层 value import 的最小集）。 */

export interface ErrorResponse {
  name: string
  data: Record<string, unknown>
}

export class EventSessionError extends Error {
  name = "EventSessionError"
  constructor(
    readonly sessionID: string,
    readonly error: ErrorResponse | string,
  ) {
    super(typeof error === "string" ? error : error.name)
  }
}

export class Project {
  constructor(readonly id: string, readonly path: string, readonly title?: string) {}
}
/**
 * @opencode-ai/client/promise shim — DeepseekNova 桌面端移植。
 *
 * opencode 的 promise 客户端（HTTP API 封装）被 vendor/app 的 server 层引用；
 * 移植后数据流改走 Tauri IPC（src/bridge），server 层会被重写，因此这里只
 * 提供类型声明让 tsc 通过，运行时 OpenCode 类不可用（调用方不应走到）。
 */

import type { Message, Part, Session, VcsInfo } from "@opencode-ai/sdk/v2"

export { type Message, type Part, type Session, type VcsInfo }

/** opencode 事件流的统一载荷（后端 Channel 事件在此适配）。 */
export type OpenCodeEvent = {
  event: string
  properties: unknown
}

export interface SessionMessageInfo {
  session: Session
  message: Message
}

export interface SessionPendingMessage {
  sessionID: string
  messageID: string
}

export interface FileDiffInfo {
  file: string
  patch?: string
  before?: string
  after?: string
  additions: number
  deletions: number
  status?: "added" | "deleted" | "modified"
}

export interface CommandInfo {
  id: string
  title: string
  description?: string
  category?: string
  keybind?: string
}

export interface McpServer {
  name: string
  command: string
  args: string[]
  status: string
  tools?: string[]
}

export interface McpResource {
  uri: string
  name: string
}

/** 以下为 opencode v1 API 遗留类型（仅被 import type 引用）。 */
export type IntegrationMethod = Record<string, unknown>
export type IntegrationOauthConnectOutput = Record<string, unknown>
export interface SessionListInput {
  directory?: string
  limit?: number
}
export interface SessionInfo {
  id: string
  title: string
  messageCount: number
  createdAt: string
}

/** opencode 客户端 API 面（占位：真实调用走 src/bridge 的 Tauri IPC）。 */
export type SessionApi = Record<string, never>

export type OpenCodeClient = Record<string, unknown>

/** 占位客户端类。移植后由 src/bridge 提供等价能力。 */
export class OpenCode {
  constructor(_options: Record<string, unknown>) {}
}
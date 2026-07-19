/**
 * store/index.ts — 全局状态管理 (Zustand)
 * 管理会话、消息、技能、配置、UI 布局等状态
 */

import { create } from "zustand";
import { subscribeWithSelector } from "zustand/middleware";
import type {
  Message,
  Mode,
  Effort,
  AgentStatus,
  UsageInfo,
  SkillSummary,
  Capabilities,
  SessionSummary,
  ContextFile,
  ApprovalRequest,
} from "../types";

// ── UI 布局状态 ────────────────────────────────────────────
interface LayoutState {
  sidebarCollapsed: boolean;
  rightCollapsed: boolean;
  activeRightTab: "context" | "workspace" | "memory" | "todo";
  showSettings: boolean;
  showCommandPalette: boolean;
}

// ── 聊天状态 ────────────────────────────────────────────────
interface ChatState {
  messages: Message[];
  input: string;
  running: boolean;
  status: AgentStatus;
  mode: Mode;
  effort: Effort;
  model: string;
  pendingApproval: ApprovalRequest | null;
}

// ── 使用量状态 ──────────────────────────────────────────────
interface UsageState {
  lastUsage: UsageInfo | null;
  sessionCache: { hit: number; miss: number };
  totalTokens: number;
}

// ── 数据状态 ────────────────────────────────────────────────
interface DataState {
  sessions: SessionSummary[];
  activeSessionId: string | null;
  skills: SkillSummary[];
  capabilities: Capabilities | null;
  contextFiles: ContextFile[];
  todos: TodoItem[];
  memories: MemoryItem[];
}

// ── TODO 项 ────────────────────────────────────────────────
interface TodoItem {
  id: string;
  text: string;
  done: boolean;
  status: "pending" | "in_progress" | "completed";
}

// ── 记忆项 ──────────────────────────────────────────────────
interface MemoryItem {
  id: string;
  text: string;
  createdAt: number;
}

// ── 组合 Store ──────────────────────────────────────────────
type Store = LayoutState & ChatState & UsageState & DataState & {
  // 布局操作
  toggleSidebar: () => void;
  toggleRightPanel: () => void;
  setActiveRightTab: (tab: LayoutState["activeRightTab"]) => void;
  setShowSettings: (show: boolean) => void;
  setShowCommandPalette: (show: boolean) => void;

  // 聊天操作
  setInput: (v: string) => void;
  setRunning: (r: boolean) => void;
  setMode: (m: Mode) => void;
  setEffort: (e: Effort) => void;
  setModel: (m: string) => void;
  addMessage: (m: Message) => void;
  updateMessage: (id: string, updater: (m: Message) => Message) => void;
  clearMessages: () => void;
  setPendingApproval: (a: ApprovalRequest | null) => void;

  // 使用量操作
  setLastUsage: (u: UsageInfo) => void;
  addCacheTokens: (hit: number, miss: number) => void;

  // 数据操作
  setSessions: (s: SessionSummary[]) => void;
  setActiveSession: (id: string) => void;
  setSkills: (s: SkillSummary[]) => void;
  setCapabilities: (c: Capabilities) => void;
  setContextFiles: (f: ContextFile[]) => void;
  setTodos: (t: TodoItem[]) => void;
  setMemories: (m: MemoryItem[]) => void;
  addTodo: (text: string) => void;
  toggleTodo: (id: string) => void;
};

export const useStore = create<Store>()(
  subscribeWithSelector((set) => ({
    // ── 初始布局 ──
    sidebarCollapsed: false,
    rightCollapsed: false,
    activeRightTab: "context",
    showSettings: false,
    showCommandPalette: false,

    // ── 初始聊天 ──
    messages: [],
    input: "",
    running: false,
    status: "ready",
    mode: "act",
    effort: "high",
    model: "deepseek-v4-flash",
    pendingApproval: null,

    // ── 初始使用量 ──
    lastUsage: null,
    sessionCache: { hit: 0, miss: 0 },
    totalTokens: 0,

    // ── 初始数据 ──
    sessions: [],
    activeSessionId: null,
    skills: [],
    capabilities: null,
    contextFiles: [],
    todos: [],
    memories: [],

    // ── 布局操作 ──
    toggleSidebar: () => set((s) => ({ sidebarCollapsed: !s.sidebarCollapsed })),
    toggleRightPanel: () => set((s) => ({ rightCollapsed: !s.rightCollapsed })),
    setActiveRightTab: (tab) => set({ activeRightTab: tab }),
    setShowSettings: (show) => set({ showSettings: show }),
    setShowCommandPalette: (show) => set({ showCommandPalette: show }),

    // ── 聊天操作 ──
    setInput: (v) => set({ input: v }),
    setRunning: (r) => set({ running: r, status: r ? "running" : "ready" }),
    setMode: (m) => set({ mode: m }),
    setEffort: (e) => set({ effort: e }),
    setModel: (m) => set({ model: m }),
    addMessage: (m) => set((s) => ({ messages: [...s.messages, m] })),
    updateMessage: (id, updater) =>
      set((s) => ({
        messages: s.messages.map((m) => (m.id === id ? updater(m) : m)),
      })),
    clearMessages: () => set({ messages: [] }),
    setPendingApproval: (a) => set({ pendingApproval: a }),

    // ── 使用量操作 ──
    setLastUsage: (u) => set({ lastUsage: u }),
    addCacheTokens: (hit, miss) =>
      set((s) => ({
        sessionCache: {
          hit: s.sessionCache.hit + hit,
          miss: s.sessionCache.miss + miss,
        },
        totalTokens: s.totalTokens + hit + miss,
      })),

    // ── 数据操作 ──
    setSessions: (sessions) => set({ sessions }),
    setActiveSession: (id) => set({ activeSessionId: id }),
    setSkills: (skills) => set({ skills }),
    setCapabilities: (capabilities) => set({ capabilities }),
    setContextFiles: (files) => set({ contextFiles: files }),
    setTodos: (todos) => set({ todos }),
    setMemories: (memories) => set({ memories }),
    addTodo: (text) =>
      set((s) => ({
        todos: [
          ...s.todos,
          { id: crypto.randomUUID(), text, done: false, status: "pending" },
        ],
      })),
    toggleTodo: (id) =>
      set((s) => ({
        todos: s.todos.map((t) =>
          t.id === id ? { ...t, done: !t.done, status: !t.done ? "completed" : "pending" } : t
        ),
      })),
  }))
);

// ── 辅助函数 ────────────────────────────────────────────────
export function uid(): string {
  return crypto.randomUUID();
}

// ── Slash 命令定义 ──────────────────────────────────────────
export interface SlashCommand {
  name: string;
  description: string;
  action: () => void;
}

export const slashCommands: SlashCommand[] = [
  { name: "/clear", description: "清空对话", action: () => useStore.getState().clearMessages() },
  { name: "/mode plan", description: "切换到规划模式", action: () => useStore.getState().setMode("plan") },
  { name: "/mode act", description: "切换到执行模式", action: () => useStore.getState().setMode("act") },
  { name: "/mode yolo", description: "切换到 YOLO 模式", action: () => useStore.getState().setMode("yolo") },
  { name: "/effort low", description: "低推理深度", action: () => useStore.getState().setEffort("low") },
  { name: "/effort high", description: "高推理深度", action: () => useStore.getState().setEffort("high") },
  { name: "/skills", description: "查看已加载技能", action: () => useStore.getState().setActiveRightTab("context") },
  { name: "/settings", description: "打开设置", action: () => useStore.getState().setShowSettings(true) },
];

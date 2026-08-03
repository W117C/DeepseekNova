/**
 * App.tsx — 应用入口与路由。
 *
 * 布局参考 opencode 桌面端：Home（会话列表）→ Session（对话主界面）。
 * 视觉全部使用 opencode 的 v2 组件与主题 token，数据经 Tauri IPC 适配层。
 */

import { createSignal, createEffect, onCleanup, Show, type JSX } from "solid-js";
import { Icon } from "@opencode-ai/ui/v2/icon";
import Home from "./pages/Home";
import Session from "./pages/Session";
import SettingsDialog from "./components/SettingsDialog";
import CommandPalette from "./components/CommandPalette";
import { clearSessionMessages } from "./bridge/session";
import { createSession } from "./bridge";
import type { SessionInfo } from "./bridge";

export default function App() {
  const [view, setView] = createSignal<"home" | "session">("home");
  const [activeSession, setActiveSession] = createSignal<SessionInfo | null>(null);
  const [showSettings, setShowSettings] = createSignal(false);
  const [showPalette, setShowPalette] = createSignal(false);

  const openSession = (s: SessionInfo) => {
    // 进入新会话前清理模块级消息，防止串会话显示陈旧转录（M1）
    clearSessionMessages();
    setActiveSession(s);
    setView("session");
  };

  const backToHome = () => {
    clearSessionMessages();
    setView("home");
    setActiveSession(null);
  };

  // 全局快捷键：Ctrl/Cmd+K 打开命令面板
  createEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setShowPalette((v) => !v);
      }
    };
    window.addEventListener("keydown", onKey);
    onCleanup(() => window.removeEventListener("keydown", onKey));
  });

  const handleCommand = async (cmd: { id: string }) => {
    switch (cmd.id) {
      case "session.new": {
        // L3：命令面板"新建会话"与 Home 按钮行为一致——真正创建并进入
        const s = await createSession();
        openSession(s);
        break;
      }
      case "session.back":
        backToHome();
        break;
      case "settings.open":
        setShowSettings(true);
        break;
      default:
        break;
    }
  };

  return (
    <div
      class="relative flex h-dvh min-w-0 flex-col bg-v2-background-bg-base select-none [&_input]:select-text [&_textarea]:select-text [&_[contenteditable]]:select-text"
      data-new-layout
    >
      <Show when={view() === "home"} fallback={<Session session={activeSession()!} onBack={backToHome} />}>
        <Home onOpenSession={openSession} />
      </Show>

      {/* 全局设置入口（右下角，opencode help-button 视觉） */}
      <button
        type="button"
        class="absolute bottom-4 right-4 z-40 flex size-8 items-center justify-center rounded-[6px] text-v2-icon-icon-muted hover:bg-v2-overlay-simple-overlay-hover"
        onClick={() => setShowSettings(true)}
        aria-label="设置"
      >
        <Icon name="settings-gear" />
      </button>

      <SettingsDialog open={showSettings()} onOpenChange={setShowSettings} />
      <CommandPalette open={showPalette()} onOpenChange={setShowPalette} onSelect={handleCommand} />
    </div>
  );
}

export type { JSX };
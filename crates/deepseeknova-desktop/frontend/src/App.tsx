/**
 * App.tsx — 应用入口与路由。
 *
 * 布局参考 opencode 桌面端：Home（会话列表）→ Session（对话主界面）。
 * 视觉全部使用 opencode 的 v2 组件与主题 token，数据经 Tauri IPC 适配层。
 */

import { createSignal, Show, type JSX } from "solid-js";
import { Icon } from "@opencode-ai/ui/v2/icon";
import { ButtonV2 } from "@opencode-ai/ui/v2/button-v2";
import Home from "./pages/Home";
import Session from "./pages/Session";
import type { SessionInfo } from "./bridge";

export default function App() {
  const [view, setView] = createSignal<"home" | "session">("home");
  const [activeSession, setActiveSession] = createSignal<SessionInfo | null>(null);

  const openSession = (s: SessionInfo) => {
    setActiveSession(s);
    setView("session");
  };

  const backToHome = () => {
    setView("home");
    setActiveSession(null);
  };

  return (
    <div
      class="relative flex h-dvh min-w-0 flex-col bg-v2-background-bg-base select-none [&_input]:select-text [&_textarea]:select-text [&_[contenteditable]]:select-text"
      data-new-layout
    >
      <Show when={view() === "home"} fallback={<Session session={activeSession()!} onBack={backToHome} />}>
        <Home onOpenSession={openSession} />
      </Show>
    </div>
  );
}

export type { JSX };
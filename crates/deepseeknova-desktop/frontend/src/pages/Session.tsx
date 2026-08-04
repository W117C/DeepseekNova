/**
 * Session.tsx — 会话主界面（对话页）。
 *
 * 布局参考 opencode session 页：标题栏 + 消息流 + Composer。
 * 消息经适配层聚合（WireEvent → Message），视觉用 v2 主题 token。
 */

import { createSignal, For, Show, createMemo } from "solid-js";
import { Icon } from "@opencode-ai/ui/v2/icon";
import { ButtonV2 } from "@opencode-ai/ui/v2/button-v2";
import { ScrollView } from "@opencode-ai/ui/scroll-view";
import { Markdown } from "@opencode-ai/session-ui/markdown";
import { sendPrompt, cancelRun, approve, useMessages, useRunning, usePendingApproval } from "../bridge/session";
import type { SessionInfo } from "../bridge";
import type { Message } from "../types";

interface SessionProps {
  session: SessionInfo;
  onBack: () => void;
}

// ── 消息渲染 ──────────────────────────────────────────────────────────

function ReasoningBlock(props: { message: Message }) {
  return (
    <div class="flex min-w-0 flex-col gap-1.5">
      <div class="flex items-center gap-2 text-v2-text-text-muted">
        <Icon name="status-active" size="small" />
        <span class="text-[12px] leading-[16px] [font-weight:440]">推理中</span>
      </div>
      <p class="whitespace-pre-wrap text-[13px] leading-5 tracking-[-0.04px] text-v2-text-text-muted [font-weight:440]">
        {props.message.content}
      </p>
    </div>
  );
}

function ToolBlock(props: { message: Message }) {
  const title = () => props.message.toolName ?? "工具调用";
  return (
    <div class="flex min-w-0 flex-col gap-1.5 rounded-[8px] border border-v2-border-border-base bg-v2-background-bg-layer-01 p-3">
      <div class="flex items-center gap-2">
        <Icon name="status-active" size="small" />
        <span class="text-[12px] leading-[16px] text-v2-text-text-base [font-weight:530]">{title()}</span>
      </div>
      <Show when={props.message.toolArgs}>
        <pre class="max-h-48 overflow-auto whitespace-pre-wrap text-[12px] leading-4 text-v2-text-text-muted [font-weight:440]">
          {props.message.toolArgs}
        </pre>
      </Show>
      <Show when={props.message.toolResult}>
        <div class="max-h-32 overflow-auto rounded-[4px] bg-v2-background-bg-deep p-2 text-[12px] leading-4 text-v2-text-text-muted">
          {props.message.toolResult}
        </div>
      </Show>
    </div>
  );
}

function ErrorBlock(props: { message: Message }) {
  return (
    <div class="rounded-[8px] border border-v2-border-border-danger bg-v2-background-bg-danger p-3 text-[13px] leading-5 text-v2-text-text-danger [font-weight:440]">
      {props.message.content}
    </div>
  );
}

function MessageItem(props: { message: Message; streaming?: boolean }) {
  const isUser = () => props.message.role === "user";
  return (
    <div class="flex w-full flex-col gap-2 px-4" data-role={props.message.role}>
      <Show when={isUser()}>
        <div class="self-end max-w-[85%] rounded-[10px] rounded-br-[4px] bg-v2-background-bg-accent px-3.5 py-2.5 text-[13px] leading-5 tracking-[-0.04px] text-white [font-weight:440]">
          {props.message.content}
        </div>
      </Show>
      <Show when={props.message.role === "assistant"}>
        <Markdown
          text={props.message.content}
          streaming={props.streaming ?? false}
          class="text-[13.5px] leading-[21px] tracking-[-0.04px] text-v2-text-text-base [font-weight:440]"
        />
      </Show>
      <Show when={props.message.role === "reasoning"}>
        <ReasoningBlock message={props.message} />
      </Show>
      <Show when={props.message.role === "tool"}>
        <ToolBlock message={props.message} />
      </Show>
      <Show when={props.message.role === "error"}>
        <ErrorBlock message={props.message} />
      </Show>
    </div>
  );
}

// ── 审批卡片 ──────────────────────────────────────────────────────────

function ApprovalCard() {
  const pending = usePendingApproval();
  const req = () => pending();
  return (
    <Show when={req()}>
      {(r) => (
        <div class="flex w-full max-w-[560px] flex-col gap-3 rounded-[10px] border border-v2-border-border-base bg-v2-background-bg-layer-01 p-4">
          <div class="flex items-center gap-2 text-v2-text-text-base [font-weight:530]">
            <Icon name="status-active" size="small" />
            {r().title}
          </div>
          <Show when={r().description}>
            <p class="text-[13px] leading-5 text-v2-text-text-muted [font-weight:440]">{r().description}</p>
          </Show>
          <div class="flex gap-2">
            <ButtonV2 variant="danger" size="normal" onClick={() => approve(r().id, false)}>
              拒绝
            </ButtonV2>
            <ButtonV2 variant="neutral" size="normal" onClick={() => approve(r().id, true)}>
              允许
            </ButtonV2>
          </div>
        </div>
      )}
    </Show>
  );
}

// ── Composer ──────────────────────────────────────────────────────────

function Composer() {
  const [input, setInput] = createSignal("");
  const running = useRunning();

  const submit = async () => {
    const text = input().trim();
    if (!text || running()) return;
    setInput("");
    await sendPrompt({ prompt: text });
  };

  return (
    <div class="shrink-0 px-4 pb-4 pt-2">
      <div
        class={`
          flex min-h-0 flex-col rounded-[12px] border border-v2-border-border-base
          bg-v2-background-bg-layer-01 transition-[border-color,box-shadow] duration-[120ms]
          focus-within:border-v2-border-border-muted
        `}
      >
        <textarea
          value={input()}
          placeholder="发送消息，或输入 / 查看命令…"
          rows={2}
          class="max-h-[240px] min-h-[44px] w-full resize-none border-0 bg-transparent px-3.5 py-3 text-[13.5px] leading-[21px] text-v2-text-text-base outline-none placeholder:text-v2-text-text-faint"
          onInput={(e) => setInput(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
              e.preventDefault();
              void submit();
            }
          }}
        />
        <div class="flex items-center justify-between px-3 pb-2.5">
          <div class="text-v2-text-text-faint [font-weight:440]">
            <Show when={running()} fallback="Enter 发送，Shift+Enter 换行">
              运行中…
            </Show>
          </div>
          <Show when={running()} fallback={<ButtonV2 variant="neutral" size="normal" icon="edit" onClick={submit} disabled={!input().trim()}>发送</ButtonV2>}>
            <ButtonV2 variant="danger" size="normal" onClick={() => cancelRun()}>
              停止
            </ButtonV2>
          </Show>
        </div>
      </div>
    </div>
  );
}

// ── 页面 ──────────────────────────────────────────────────────────────

export default function Session(props: SessionProps) {
  const messages = useMessages();
  const running = useRunning();
  const list = createMemo(() => messages().messages);

  return (
    <div class="flex min-h-0 min-w-0 flex-1 flex-col bg-v2-background-bg-base">
      {/* 标题栏（opencode titlebar 视觉） */}
      <header class="flex h-12 shrink-0 items-center gap-2 border-b border-v2-border-border-base px-3">
        <button
          type="button"
          class="flex size-8 items-center justify-center rounded-[6px] text-v2-icon-icon-muted hover:bg-v2-overlay-simple-overlay-hover"
          onClick={props.onBack}
          aria-label="返回会话列表"
        >
          <Icon name="outline-chevron-down" class="rotate-90" />
        </button>
        <div class="flex min-w-0 flex-col">
          <span class="truncate text-[13px] leading-[16px] text-v2-text-text-base [font-weight:530]">
            {props.session.title}
          </span>
          <span class="text-[11px] leading-[14px] text-v2-text-text-faint [font-weight:440]">
            DeepseekNova · {props.session.message_count} 条消息
          </span>
        </div>
        <Show when={running()}>
          <div class="ml-auto flex items-center gap-1.5 text-v2-text-text-muted">
            <Icon name="status-active" size="small" />
            <span class="text-[12px] [font-weight:440]">运行中</span>
          </div>
        </Show>
      </header>

      {/* 消息流 */}
      <ScrollView class="min-h-0 flex-1">
        <div class="mx-auto flex w-full max-w-[860px] min-h-full flex-col gap-6 py-6">
          <Show
            when={list().length > 0}
            fallback={
              <div class="flex flex-1 flex-col items-center justify-center gap-2 text-center">
                <div class="text-[15px] leading-[20px] text-v2-text-text-base [font-weight:530]">
                  {props.session.title}
                </div>
                <p class="text-[13px] leading-5 text-v2-text-text-muted [font-weight:440]">
                  向 DeepseekNova 描述你的任务，开始协作
                </p>
              </div>
            }
          >
            <For each={list()}>
              {(m, i) => (
                <MessageItem
                  message={m}
                  streaming={running() && i() === list().length - 1 && m.role === "assistant"}
                />
              )}
            </For>
          </Show>
          <ApprovalCard />
        </div>
      </ScrollView>

      <Composer />
    </div>
  );
}
/**
 * CommandPalette.tsx — 命令面板（opencode command-palette 视觉）。
 *
 * Ctrl/Cmd+K 打开，支持模糊搜索会话操作命令；数据来自后端 capabilities/skills。
 */

import { createEffect, createMemo, createSignal, For, Show, onCleanup } from "solid-js";
import { Dialog as KobalteDialog } from "@kobalte/core/dialog";
import { DialogV2, DialogBody } from "@opencode-ai/ui/v2/dialog-v2";
import { Icon } from "@opencode-ai/ui/v2/icon";
import { getCapabilities, listSkills } from "../bridge";
import type { Capabilities, SkillSummary } from "../types";

interface CommandPaletteProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** 选中命令后的回调 */
  onSelect: (command: { id: string; title: string; description?: string }) => void;
}

interface Command {
  id: string;
  title: string;
  description?: string;
  category?: string;
}

const BUILTIN_COMMANDS: Command[] = [
  { id: "session.new", title: "新建会话", description: "创建一个新会话", category: "会话" },
  { id: "session.back", title: "返回会话列表", description: "回到 Home 页", category: "会话" },
  { id: "settings.open", title: "打开设置", description: "配置模型、推理参数与工具", category: "设置" },
];

export default function CommandPalette(props: CommandPaletteProps) {
  const [query, setQuery] = createSignal("");
  const [capabilities, setCapabilities] = createSignal<Capabilities | null>(null);
  const [skills, setSkills] = createSignal<SkillSummary[]>([]);

  createEffect(() => {
    if (!props.open) return;
    void getCapabilities().then(setCapabilities).catch(() => {});
    void listSkills().then(setSkills).catch(() => {});
  });

  const commands = createMemo<Command[]>(() => {
    const q = query().trim().toLowerCase();
    const all: Command[] = [
      ...BUILTIN_COMMANDS,
      ...skills().map((s) => ({
        id: `skill.${s.name}`,
        title: `技能：${s.name}`,
        description: s.description,
        category: "技能",
      })),
    ];
    if (!q) return all;
    return all.filter(
      (c) => c.title.toLowerCase().includes(q) || (c.description ?? "").toLowerCase().includes(q),
    );
  });

  // Ctrl/Cmd+K 全局快捷键
  createEffect(() => {
    if (!props.open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") props.onOpenChange(false);
      if (e.key === "Enter") {
        const first = commands()[0];
        if (first) {
          props.onOpenChange(false);
          props.onSelect(first);
        }
      }
    };
    window.addEventListener("keydown", onKey);
    onCleanup(() => window.removeEventListener("keydown", onKey));
  });

  return (
    <KobalteDialog open={props.open} onOpenChange={props.onOpenChange}>
      <KobalteDialog.Portal>
        <DialogV2 size="normal" class="!w-[min(calc(100vw_-_24px),560px)]">
          <DialogBody class="p-0">
            <div class="flex flex-col">
              {/* 输入框 */}
              <div class="flex items-center gap-2 border-b border-v2-border-border-base px-3.5 py-3">
                <Icon name="magnifying-glass" size="small" class="text-v2-icon-icon-muted" />
                <input
                  autofocus
                  value={query()}
                  placeholder="输入命令或技能名称…"
                  class="min-w-0 flex-1 border-0 bg-transparent text-[13.5px] leading-[20px] text-v2-text-text-base outline-none placeholder:text-v2-text-text-faint"
                  onInput={(e) => setQuery(e.currentTarget.value)}
                />
                <Show when={capabilities()}>
                  <span class="shrink-0 rounded-[4px] bg-v2-background-bg-layer-02 px-1.5 py-0.5 text-[10px] text-v2-text-text-faint [font-weight:530]">
                    v{capabilities()?.version}
                  </span>
                </Show>
              </div>

              {/* 命令列表 */}
              <div class="max-h-[360px] overflow-y-auto p-1.5">
                <Show
                  when={commands().length > 0}
                  fallback={
                    <div class="px-3 py-6 text-center text-[13px] text-v2-text-text-faint [font-weight:440]">
                      无匹配命令
                    </div>
                  }
                >
                  <For each={commands()}>
                    {(cmd, i) => (
                      <button
                        type="button"
                        data-component="command-palette-row"
                        class={`
                          flex h-9 w-full items-center gap-2 rounded-[6px] px-2.5 text-left
                          transition-[background-color] duration-[100ms] ease-in-out
                          hover:bg-v2-overlay-simple-overlay-hover focus-visible:bg-v2-overlay-simple-overlay-hover focus-visible:outline-none
                        `}
                        onClick={() => {
                          props.onOpenChange(false);
                          props.onSelect(cmd);
                        }}
                      >
                        <span class="shrink-0 text-v2-icon-icon-muted">
                          <Icon name={cmd.category === "会话" ? "settings-gear" : "grid-plus"} size="small" />
                        </span>
                        <div class="min-w-0 flex-1">
                          <div class="truncate text-[13px] leading-[18px] text-v2-text-text-base [font-weight:440]">
                            {cmd.title}
                          </div>
                          <Show when={cmd.description}>
                            <div class="truncate text-[11px] leading-[14px] text-v2-text-text-faint [font-weight:440]">
                              {cmd.description}
                            </div>
                          </Show>
                        </div>
                        <Show when={cmd.category}>
                          <span class="shrink-0 text-[11px] text-v2-text-text-faint [font-weight:440]">
                            {cmd.category}
                          </span>
                        </Show>
                      </button>
                    )}
                  </For>
                </Show>
              </div>
            </div>
          </DialogBody>
        </DialogV2>
      </KobalteDialog.Portal>
    </KobalteDialog>
  );
}
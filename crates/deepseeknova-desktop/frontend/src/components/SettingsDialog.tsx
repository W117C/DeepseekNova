/**
 * SettingsDialog.tsx — 设置对话框。
 *
 * 视觉参考 opencode dialog-settings（DialogV2 + FieldV2 + SwitchV2 + TextInputV2），
 * 数据绑定 DeepseekNova 后端 settings 命令（system_prompt / reasoning_params / 速率限制）。
 */

import { createResource, createSignal, createEffect, Show, For } from "solid-js";
import { Dialog as KobalteDialog } from "@kobalte/core/dialog";
import { DialogV2, DialogHeader, DialogBody, DialogFooter, DialogTitle } from "@opencode-ai/ui/v2/dialog-v2";
import { ButtonV2 } from "@opencode-ai/ui/v2/button-v2";
import { FieldV2 } from "@opencode-ai/ui/v2/field-v2";
import { TextInputV2 } from "@opencode-ai/ui/v2/text-input-v2";
import { Switch } from "@opencode-ai/ui/v2/switch-v2";
import {
  saveSettings,
  getSystemPrompt,
  setSystemPrompt,
  getReasoningParams,
  setReasoningParams,
  listTools,
  setToolEnabled,
  type ReasoningParams,
  type ToolInfo,
} from "../bridge";

interface SettingsDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

type SectionId = "general" | "reasoning" | "tools" | "system";

const SECTIONS: { id: SectionId; label: string }[] = [
  { id: "general", label: "通用" },
  { id: "reasoning", label: "推理参数" },
  { id: "tools", label: "工具" },
  { id: "system", label: "系统提示词" },
];

export default function SettingsDialog(props: SettingsDialogProps) {
  const [section, setSection] = createSignal<SectionId>("general");
  const [saving, setSaving] = createSignal(false);
  const [saved, setSaved] = createSignal(false);

  // ── 数据加载 ──
  const [systemPrompt, { refetch: refetchPrompt }] = createResource(
    () => props.open,
    () => getSystemPrompt().then((p) => p ?? ""),
    { initialValue: "" },
  );
  const [params, { refetch: refetchParams }] = createResource(
    () => props.open,
    () => getReasoningParams(),
    {
      initialValue: {
        temperature: 0.7,
        top_p: 0.95,
        max_tokens: 8192,
        stop_sequences: [],
        fallback_model: null,
        timeout_secs: 60,
        max_retries: 2,
      } as ReasoningParams,
    },
  );
  const [tools, { refetch: refetchTools }] = createResource(
    () => props.open && section() === "tools",
    () => listTools(),
    { initialValue: [] as ToolInfo[] },
  );

  // ── 本地编辑状态 ──
  const [promptDraft, setPromptDraft] = createSignal("");
  const [temp, setTemp] = createSignal(0.7);
  const [topP, setTopP] = createSignal(0.95);
  const [maxTokens, setMaxTokens] = createSignal(8192);
  const [fallbackModel, setFallbackModel] = createSignal("");
  const [timeoutSecs, setTimeoutSecs] = createSignal(60);
  const [maxRetries, setMaxRetries] = createSignal(2);
  const [toolToggles, setToolToggles] = createSignal<Record<string, boolean>>({});

  // 打开时加载数据并填充草稿
  createEffect(() => {
    if (!props.open) return;
    void refetchPrompt();
    void refetchParams();
    void refetchTools();
  });

  createEffect(() => {
    if (!props.open) return;
    const p = params();
    const t = tools();
    if (!systemPrompt.loading && !params.loading) {
      setPromptDraft(systemPrompt() ?? "");
      setTemp(p.temperature);
      setTopP(p.top_p);
      setMaxTokens(p.max_tokens);
      setFallbackModel(p.fallback_model ?? "");
      setTimeoutSecs(p.timeout_secs);
      setMaxRetries(p.max_retries);
    }
    if (t.length > 0) {
      setToolToggles(Object.fromEntries(t.map((x) => [x.name, x.enabled])));
    }
  });

  const save = async () => {
    setSaving(true);
    try {
      await setSystemPrompt(promptDraft());
      await setReasoningParams({
        temperature: temp(),
        top_p: topP(),
        max_tokens: maxTokens(),
        stop_sequences: [],
        fallback_model: fallbackModel().trim() || null,
        timeout_secs: timeoutSecs(),
        max_retries: maxRetries(),
      });
      const toggles = toolToggles();
      await Promise.all(
        Object.entries(toggles).map(([name, enabled]) => setToolEnabled(name, enabled)),
      );
      await saveSettings({});
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
    } finally {
      setSaving(false);
    }
  };

  return (
    <KobalteDialog open={props.open} onOpenChange={props.onOpenChange}>
      <KobalteDialog.Portal>
        <DialogV2 size="large" variant="settings">
          <DialogHeader>
            <DialogTitle>设置</DialogTitle>
          </DialogHeader>
          <DialogBody class="flex min-h-0 flex-col gap-6">
          {/* 左侧分区导航（opencode settings-list 视觉） */}
          <div class="flex gap-6">
            <div class="flex w-44 shrink-0 flex-col gap-1">
              <For each={SECTIONS}>
                {(s) => (
                  <button
                    type="button"
                    class={`
                      flex h-7 min-w-0 items-center gap-2 rounded-[6px] px-2 text-left text-[13px] [font-weight:440]
                      transition-[background-color,color] duration-[120ms] ease-in-out
                      data-[selected]:bg-v2-background-bg-layer-02 data-[selected]:text-v2-text-text-base
                      text-v2-text-text-muted hover:bg-v2-background-bg-layer-01
                    `}
                    data-selected={section() === s.id ? "" : undefined}
                    onClick={() => setSection(s.id)}
                  >
                    {s.label}
                  </button>
                )}
              </For>
            </div>

            {/* 右侧内容区 */}
            <div class="min-w-0 flex-1">
              {/* ── 通用 ── */}
              <Show when={section() === "general"}>
                <div class="flex flex-col gap-5">
                  <div class="text-v2-text-text-base [font-weight:530]">执行设置</div>
                  <FieldV2 class="flex items-center justify-between gap-4">
                    <FieldV2.Label tooltip="主模型不可用时的降级模型（空为不降级）">降级模型</FieldV2.Label>
                    <div class="w-64">
                      <TextInputV2
                        value={fallbackModel()}
                        placeholder="例如 deepseek-chat"
                        onInput={(e) => setFallbackModel(e.currentTarget.value)}
                      />
                    </div>
                  </FieldV2>
                  <FieldV2 class="flex items-center justify-between gap-4">
                    <FieldV2.Label tooltip="单次请求超时（秒）">请求超时（秒）</FieldV2.Label>
                    <div class="w-32">
                      <TextInputV2
                        type="number"
                        value={timeoutSecs()}
                        numeric
                        onInput={(e) => setTimeoutSecs(Number(e.currentTarget.value))}
                      />
                    </div>
                  </FieldV2>
                  <FieldV2 class="flex items-center justify-between gap-4">
                    <FieldV2.Label tooltip="失败自动重试次数">最大重试</FieldV2.Label>
                    <div class="w-32">
                      <TextInputV2
                        type="number"
                        value={maxRetries()}
                        numeric
                        onInput={(e) => setMaxRetries(Number(e.currentTarget.value))}
                      />
                    </div>
                  </FieldV2>
                </div>
              </Show>

              {/* ── 推理参数 ── */}
              <Show when={section() === "reasoning"}>
                <div class="flex flex-col gap-5">
                  <div class="text-v2-text-text-base [font-weight:530]">采样参数</div>
                  <FieldV2 class="flex items-center justify-between gap-4">
                    <FieldV2.Label tooltip="采样温度，0.0–2.0">Temperature</FieldV2.Label>
                    <div class="w-32">
                      <TextInputV2
                        type="number"
                        step="0.1"
                        value={temp()}
                        numeric
                        onInput={(e) => setTemp(Number(e.currentTarget.value))}
                      />
                    </div>
                  </FieldV2>
                  <FieldV2 class="flex items-center justify-between gap-4">
                    <FieldV2.Label tooltip="核采样，0.0–1.0">Top P</FieldV2.Label>
                    <div class="w-32">
                      <TextInputV2
                        type="number"
                        step="0.05"
                        value={topP()}
                        numeric
                        onInput={(e) => setTopP(Number(e.currentTarget.value))}
                      />
                    </div>
                  </FieldV2>
                  <FieldV2 class="flex items-center justify-between gap-4">
                    <FieldV2.Label tooltip="单次生成最大 token 数">最大 Token</FieldV2.Label>
                    <div class="w-32">
                      <TextInputV2
                        type="number"
                        step="256"
                        value={maxTokens()}
                        numeric
                        onInput={(e) => setMaxTokens(Number(e.currentTarget.value))}
                      />
                    </div>
                  </FieldV2>
                </div>
              </Show>

              {/* ── 工具 ── */}
              <Show when={section() === "tools"}>
                <div class="flex flex-col gap-1">
                  <For each={tools()}>
                    {(t) => (
                      <div class="flex h-9 items-center justify-between rounded-[6px] px-2 hover:bg-v2-overlay-simple-overlay-hover">
                        <div class="flex min-w-0 flex-col">
                          <span class="text-[13px] text-v2-text-text-base [font-weight:440]">{t.name}</span>
                          <Show when={t.description}>
                            <span class="truncate text-[11px] text-v2-text-text-faint">{t.description}</span>
                          </Show>
                        </div>
                        <Switch
                          checked={toolToggles()[t.name] ?? t.enabled}
                          onChange={(e) =>
                            setToolToggles((m) => ({ ...m, [t.name]: e }))
                          }
                          class="shrink-0"
                        >
                          {t.name}
                        </Switch>
                      </div>
                    )}
                  </For>
                </div>
              </Show>

              {/* ── 系统提示词 ── */}
              <Show when={section() === "system"}>
                <div class="flex flex-col gap-3">
                  <div class="text-v2-text-text-base [font-weight:530]">系统提示词</div>
                  <p class="text-[12px] leading-4 text-v2-text-text-muted [font-weight:440]">
                    作为 Agent 的角色设定，叠加到每次运行的 system prompt。
                  </p>
                  <textarea
                    value={promptDraft()}
                    rows={10}
                    placeholder="例如：你是一个高效的 Rust 工程师…"
                    class="w-full resize-y rounded-[8px] border border-v2-border-border-base bg-v2-background-bg-layer-01 px-3 py-2.5 text-[13px] leading-5 text-v2-text-text-base outline-none placeholder:text-v2-text-text-faint focus:border-v2-border-border-muted"
                    onInput={(e) => setPromptDraft(e.currentTarget.value)}
                  />
                </div>
              </Show>
            </div>
          </div>
        </DialogBody>
        <DialogFooter>
          <div class="flex items-center justify-end gap-2">
            <Show when={saved()}>
              <span class="mr-auto text-[12px] text-v2-text-text-muted [font-weight:440]">已保存</span>
            </Show>
            <ButtonV2 variant="ghost" size="normal" onClick={() => props.onOpenChange(false)}>
              取消
            </ButtonV2>
            <ButtonV2 variant="neutral" size="normal" onClick={save} disabled={saving()}>
              {saving() ? "保存中…" : "保存"}
            </ButtonV2>
          </div>
        </DialogFooter>
      </DialogV2>
      </KobalteDialog.Portal>
    </KobalteDialog>
  );
}
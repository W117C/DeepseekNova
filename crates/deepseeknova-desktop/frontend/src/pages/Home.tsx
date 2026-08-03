/**
 * Home.tsx — Home 页（会话列表）。
 *
 * 视觉参考 opencode home-sessions-view：会话搜索框 + 新建会话按钮 + 会话列表。
 * 数据来自 Tauri IPC 的 listSessions（经适配层 resource 加载）。
 */

import { createResource, createSignal, For, Show } from "solid-js";
import { Icon } from "@opencode-ai/ui/v2/icon";
import { ButtonV2 } from "@opencode-ai/ui/v2/button-v2";
import { ScrollView } from "@opencode-ai/ui/scroll-view";
import { listSessions, createSession, deleteSession, type SessionInfo } from "../bridge";

interface HomeProps {
  onOpenSession: (session: SessionInfo) => void;
}

export default function Home(props: HomeProps) {
  const [sessions, { refetch }] = createResource(() => listSessions(), {
    initialValue: [] as SessionInfo[],
  });
  const [search, setSearch] = createSignal("");

  const filtered = () => {
    const q = search().trim().toLowerCase();
    const list = sessions() ?? [];
    if (!q) return list;
    return list.filter((s) => s.title.toLowerCase().includes(q));
  };

  const newSession = async () => {
    const s = await createSession();
    await refetch();
    props.onOpenSession(s);
  };

  const removeSession = async (id: string, e: MouseEvent) => {
    e.stopPropagation();
    // L3：删除前确认，避免误删
    if (!window.confirm("确定删除该会话？此操作不可撤销。")) return;
    await deleteSession(id);
    await refetch();
  };

  return (
    <div class="flex min-h-0 min-w-0 flex-1 flex-col items-start">
      <main class="min-h-0 min-w-0 flex-1 flex flex-col items-center">
        <div class="flex h-full min-h-0 w-full max-w-[1100px] flex-col gap-8 px-6 pt-6 lg:pt-12">
          {/* 搜索 + 新建会话（opencode home-sessions-view 视觉） */}
          <div class="sticky top-0 z-30 shrink-0 bg-v2-background-bg-base pb-3">
            <div class="flex items-center gap-2">
              <div class="relative min-w-0 flex-1">
                <label
                  class={`
                    relative z-20 flex h-9 w-full items-center gap-2 rounded-[6px] py-1 pl-3 pr-2
                    bg-v2-background-bg-layer-02/60 text-v2-icon-icon-muted transition-[background-color,box-shadow]
                    duration-[120ms] ease-in-out hover:bg-v2-background-bg-layer-02 focus-within:bg-v2-background-bg-layer-02
                  `}
                >
                  <Icon name="magnifying-glass" />
                  <input
                    class={`
                      relative z-20 min-w-0 flex-1 border-0 bg-transparent outline-0
                      text-v2-text-text-base [font-weight:440] placeholder:text-v2-text-text-faint
                    `}
                    value={search()}
                    placeholder="搜索会话…"
                    onInput={(e) => setSearch(e.currentTarget.value)}
                  />
                </label>
              </div>
              <ButtonV2
                data-action="home-new-session"
                variant="neutral"
                size="normal"
                icon="edit"
                onClick={newSession}
              >
                新建会话
              </ButtonV2>
            </div>
          </div>

          {/* 会话列表 */}
          <div class="min-h-0 flex-1 overflow-hidden">
            <Show
              when={(filtered()?.length ?? 0) > 0}
              fallback={
                <div class="flex h-full flex-col items-center justify-center gap-4 text-center">
                  <div class="shrink-0 text-[13px] leading-[13px] tracking-[-0.04px] text-v2-text-text-base [font-weight:530]">
                    还没有会话
                  </div>
                  <p class="mb-1 text-[13px] leading-5 tracking-[-0.04px] text-v2-text-text-muted [font-weight:440]">
                    新建一个会话，开始与 DeepseekNova 协作
                  </p>
                  <ButtonV2 variant="neutral" size="normal" icon="edit" onClick={newSession}>
                    新建会话
                  </ButtonV2>
                </div>
              }
            >
              <ScrollView class="h-full">
                <div class="flex min-w-0 flex-col gap-px pb-16 pr-3">
                  <For each={filtered()}>
                    {(s) => (
                      <div class="group/session relative flex h-10 min-w-0 items-center rounded-[6px]">
                        <button
                          type="button"
                          data-component="home-session-row"
                          class={`
                            flex h-10 min-w-0 w-full flex-1 shrink-0 cursor-default items-center gap-2 rounded-[6px] border-0
                            bg-transparent py-3 pl-3 pr-10 text-left text-v2-text-text-muted [font-weight:530]
                            transition-[background-color,color,box-shadow] duration-[120ms] ease-in-out
                            hover:bg-v2-overlay-simple-overlay-hover focus-visible:bg-v2-overlay-simple-overlay-hover focus-visible:outline-none
                          `}
                          onClick={() => props.onOpenSession(s)}
                        >
                          <span class="min-w-0 overflow-hidden text-ellipsis whitespace-nowrap text-v2-text-text-base [font-weight:530]">
                            {s.title}
                          </span>
                          <span class="ml-auto shrink-0 text-v2-text-text-faint [font-weight:440]">
                            {s.message_count} 条消息
                          </span>
                        </button>
                        <div class="hover-reveal absolute right-1.5 top-1/2 flex -translate-y-1/2 items-center gap-1 group-hover/session:opacity-100">
                          <button
                            type="button"
                            class="flex size-6 items-center justify-center rounded-[4px] text-v2-icon-icon-muted hover:bg-v2-overlay-simple-overlay-hover"
                            onClick={(e) => removeSession(s.id, e)}
                            aria-label="删除会话"
                          >
                            <Icon name="xmark-small" />
                          </button>
                        </div>
                      </div>
                    )}
                  </For>
                </div>
              </ScrollView>
            </Show>
          </div>
        </div>
      </main>
    </div>
  );
}
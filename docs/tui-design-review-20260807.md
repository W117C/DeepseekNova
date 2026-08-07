# TUI 交互设计评审报告（2026-08-07）

> 目的：通过「真实对话记录 + 实机运行验证 + 竞品调研」三路证据，找出 DeepseekNova TUI 的
> 交互设计问题，并给出对标主流工具（Claude Code / Aider / OpenCode / Gemini CLI / Codex CLI）
> 的改进方案。供领导裁决优先级后实施。

---

## 一、方法

| 证据源 | 做了什么 |
|---|---|
| 会话记录 | 分析 `~/.deepseeknova/sessions/*.jsonl`（48 个真实会话），提取用户遇到问题的对话 |
| 实机运行 | 用 pty 驱动真实 TUI（`chat --tui`）+ 真实 DeepSeek API 跑多轮对话，抓屏逐帧分析 |
| 竞品调研 | 三份并行调研报告：Claude Code/Aider、OpenCode/Gemini CLI/Codex CLI、Rust TUI 通用最佳实践 |

**结论先行**：核心消息树/折叠/工具流式显示/焦点系统已经做得不错（实机确认），真正的问题是
**状态信息过载、关键指标失真、命令反馈污染对话、以及两个功能是"文档有但实现没有"**。

---

## 二、问题清单（分级）

### P0 —— 实测确认的问题（影响日常使用）

**1. 状态栏过载 + 静默截断**（实机复现）✅ A1 已修
- 120 列终端下，状态栏 `…│ ↑4069 ↓125 Σ4194 推理 44 缓存 hit0 │ li…` 在 `lines` 处被截断，
  无省略号、无优先级丢弃，最右侧信息（滚动位置/行数）静默消失。
- 根因：`render/status.rs` 把 7 组信息（运行态/模型/ctx/成本/折叠/计数/退出警示）全塞进一行
  `Span` 数组，靠 ratatui 的 Line 直接裁切。
- 竞品对照：Codex footer 有显式"宽度不足时的丢弃顺序"（先丢教学提示 → 再丢上下文 → 最后留
  模式）；Claude Code 把状态行做成"可配置 + 两行"。

**2. ctx 计量误导**（实机复现 + 会话记录确认）
- 用户会话里 agent 曾承认：`ctx 62% (78.2k / 128k)` 的分子是**会话累计 token（只增不减，
  compaction 后也不减）**。该问题已在 fdefbd9 修复（分子改为单次请求实际 tokens）。
- **剩余问题**：分母 = 配置的 `context_window`（用户配置 = 1M），实机显示
  `ctx [░░░░░░░░░░] 0% (4.2k / 1.0M)` —— 窗口 1M 时进度条恒为 0%，完全无信息量；
  且不反映真实压力源（`budget.max_total_tokens` 才是 agent 压缩/暂停的实际门槛）。
- 竞品对照：Codex 显示"剩余百分比"且留 12k 安全区；Claude Code 按**当前回合真实 prompt
  tokens**做分子、绿/黄/红三档阈值着色。
- 本次修复（A2）：分母 = `min(context_window, budget.max_total_tokens)`，实机 `(4.2k / 900.0k)`。

**3. `@` 文件补全是完全失效的死代码**（实机复现）✅ A3 已接线
- `with_at_files` 在 TUI builder 定义，但整个仓库**没有任何调用点**（grep 仅 1 处定义）→ CLI
  从不注入工作区文件清单 → `maybe_open_completion` 因 `at_files.is_empty()` 永不触发。
- 实机：输入 `@` 无任何浮层出现。
- 但 GUIDE.md 明确写着「`@` 触发文件补全（候选清单由 CLI 注入工作区文件）」——**文档与实现脱节**。
- 本次修复：CLI 注入工作区文件清单，实机输入 `@` 弹出"文件补全"浮层。

**4. 命令/系统反馈污染对话面板**（实机复现 + 会话记录确认）
- `/help` 一次性灌 30+ 行进对话面板且不消失；Esc 退出确认也会写一条系统行。会话记录里用户
  明确抱怨过（"每执行一次就留一行反馈，确实很碍事"）。`/fold` 反馈已在早前改为 notice
  （状态栏上方临时提示）；本次（A4）把 `/help` 改为可滚动浮层，两者均不再污染对话。
- 竞品对照：Claude Code 帮助走可关闭的浮层/面板；Codex 的指令提示（`? for shortcuts`）是 footer
  临时提示，不进 transcript；OpenCode 的临时消息是 10 秒 TTL 的状态栏色块。

**5. 任务暂停（Paused）提示晦涩**（代码确认 + 会话记录确认）✅ A5 已修
- 达 max_steps / budget 时只显示 `⏸ max steps` + `可 /resume <id>`。用户会话里为此问了三次
  （"怎么回事"），agent 解释后才知道是步数/预算保护。
- 竞品对照：Claude Code 中断后保留部分回复并明确提示"可继续"；Codex 错误一律"原因 + 可执行
  恢复动作"两行结构。
- 本次修复：`已达步骤上限（N），任务未完成` / `已达预算上限：X` + `输入 /resume <id> 继续任务`。

### P1 —— 体验缺口

**6. 保存会话无标题/预览，无法区分**（实机复现）
- 侧边栏"会话"面板只显示不透明时间戳 ID（`20260807-122507`），无首句预览。用户有 48 个会话
  时几乎无法找到想恢复的那一个。
- 竞品对照：Claude Code 用小模型把首条 prompt 概括成会话标题 + 选择器显示首条消息摘要；
  OpenCode 侧栏显示 `Session: <自动标题>`。

**7. 运行中双转圈**（实机复现）
- 对话面板 agent 位置 `⠇ 正在思考… Ctrl+C 取消` + 输入区 `❯ ⠇ 等待响应… Ctrl+C 取消`
  两处同时转，冗余。
- 竞品对照：OpenCode 只在消息列表底部有一行 spinner + 阶段文案；输入区保持干净。

**8. 推理折叠摘要无内容预览**（实机复现）
- `[推理 ▸ 折叠 203 字符 · Enter 展开]` 只有字符数，看不到推理讲了什么，必须展开。
- 竞品对照：Codex 折叠为 `Reasoning` 标题 + 摘要；Claude Code 推理默认折叠为灰色摘要行。

### P2 —— 打磨

**9. 无对话时 /help 与欢迎卡叠加**（实机复现）
- 欢迎卡不消失，/help 输出追加在卡片下方，版面混乱。

**10. 斜杠命令浮层不在输入框正上方**（实机复现）
- `/` 补全浮层浮动在消息区中部，与输入框之间隔着大片空白。

**11. 退出确认（Esc）提示语与文档不一致**
- 实机显示"再按 Esc 退出（3 秒内）"，GUIDE 写"再按退出"。

---

## 三、竞品对标要点（可直接吸收）

| 维度 | 本项目现状 | 主流做法（调研结论） | 建议吸收 |
|---|---|---|---|
| **无模式/键位** | 无模式 + 焦点切换 + emacs/vim 可配 | Claude Code/Codex 默认无模式、vim 仅作用输入框 | 保持无模式 ✅ |
| **状态栏** | 一行塞 7 组信息，静默截断 | Codex 回退链（丢教学→丢上下文→留模式）；Claude Code 两行 + 可配置 statusline | 优先级丢弃 + 省略号 |
| **上下文/成本** | 累计分子/1M 分母，恒 0% | Codex `% left` 留安全区；Claude Code 当前回合真实 tokens + 三档着色；成本独立 `/usage` | 改实时口径 + 阈值变色 |
| **命令反馈** | 全进对话面板 | Codex footer 提示；OpenCode 状态栏 10s TTL 色块；Claude Code 浮层 | 系统反馈走临时提示/浮层 |
| **推理显示** | 折叠仅字符数 | Codex/Claude Code 折叠摘要 + 可展开原文 | 摘要加首句预览 |
| **会话管理** | 不透明时间戳 ID | Claude Code AI 标题 + 首句摘要；OpenCode `Session: 标题` | 首句生成标题/预览 |
| **onboarding** | 欢迎卡（有） | Claude Code 输入框预置"灰色示例 prompt"；Codex 欢迎面板给示例任务 | 输入框预置示例 + 随机 tip |
| **错误/暂停** | `⏸ max steps` 晦涩 | Codex "原因 + 恢复命令"两行结构 | 暂停提示改为可执行动作 |
| **工具调用** | 折叠摘要行（已不错） | OpenCode 左粗边框块 + 分类型参数摘要；Claude Code 合并重复调用 | 合并重复调用 |
| **首启示例** | 无 | Claude Code 从 git 历史生成示例 prompt，Tab 采纳 | 首会话预置示例 |

---

## 四、改进方案（按优先级）

> ✅ = 已于 2026-08-07 本次评审落地（含测试，`cargo test -p deepseeknova-tui -p deepseeknova-cli` 全绿、clippy/fmt 干净）。

### 第 1 批：修 P0（改动小、价值高、可直接落地）

**A1. 状态栏宽度感知 + 优先级丢弃** ✅
- 位置：`render/status.rs`、`render/message.rs:570`
- 方案：segment 改为带优先级 `(u8, Span)`，`fit_status_line(width)` 按宽度回退：
  先丢 usage 明细（↑↓Σ推理缓存hit）→ lines → turn → 折叠 → 成本 → 最后留 `运行态+模型 │ ctx`；
  仍放不下才截断加省略号。ctx 标签+进度条合并为单段，避免"进度条在、标签被丢"。
- 实机验证：120 列下不再出现 `│ li…` 截断，lines 段整体丢弃。

**A2. ctx 计量改实时口径** ✅
- 位置：`app/mod.rs`（`effective_ctx_window` 纯函数）、`commands/mod.rs`（TuiCaps.budget_window）、
  `lib.rs`（builder）、`cli/main.rs`（注入 budget）
- 方案：分子已是单次请求实际 tokens（fdefbd9 已改）；本次补分母 = `min(context_window, budget.max_total_tokens)`，
  预算才是真实压力点。实机验证：1M 窗口 + 900k 预算 → 显示 `(4.2k / 900.0k)`。

**A3. `@` 补全接线（已接线）** ✅
- 位置：`cli/main.rs` 新增 `collect_at_files()`（递归扫 cwd、跳噪声目录、上限 500 条），
  TUI 启动注入 `with_at_files`。
- 实机验证：输入 `@` 弹出"文件补全"浮层（此前完全不触发）。GUIDE 声称的功能现在真的存在。
- 顺带发现：浮层与欢迎卡/正文重叠（@ 补全与 /help 都受影响）——见 C2，需统一浮层定位策略。

**A4. 命令/系统反馈不再污染对话面板** ✅（/fold 此前已改 notice；本次改 /help）
- 位置：`commands/builtin.rs`（HelpCmd）、`app/focus.rs`（Focus::Help + HelpOverlay）、
  `app/state.rs`（handle_help_key）、`render/message.rs`（render_help_overlay）
- 方案：`/help` 改为**可滚动浮层**（Esc/q 关闭，j/k、↑/↓、PageUp/Down 滚动，含 `1-8/30 行` 分页提示），
  不再往对话面板灌 30+ 行。`/fold` 等瞬时反馈此前已走 notice（状态栏上方临时提示，TTL 过期）。
- 实机验证：/help 打开浮层、Esc 干净关闭、对话面板零污染。

**A5. Paused 提示改为"原因 + 恢复动作"** ✅
- 位置：`model/apply.rs` 新增 `friendly_pause_reason()`（`reached max steps (N)`→`已达步骤上限（N），任务未完成`；
  `budget: X`→`已达预算上限：X`），恢复提示改为 `输入 /resume <id> 继续任务，或直接输入新指令`。
- 测试：`friendly_pause_reason_maps_known_and_passes_through` 覆盖已知形态 + 未知原样保留。

### 第 2 批：修 P1（体验提升）

**B1. 保存会话显示首句标题/预览** ✅
- 位置：`store/lib.rs`（新增 `preview_first_prompt` / `list_sessions_with_preview`，只读首行取
  `input.prompt`）、`app/state.rs`（`SessionMeta{id,preview}` + trait 返回类型）、`cli/main.rs`
  （TuiSessionController）、`render/sidebar.rs`、`commands/builtin.rs`（/sessions 命令）
- 方案：会话文件首条 user prompt 截断 24 字符作标题，无首句回退 id；侧边栏 16 字符截断显示。
- 实机验证：侧边栏显示 `▸● 你好`、`· 帮我列出当前目录下的文件`、`· Reply with exact…`，
  不再是不透明时间戳 ID。

**B2. 去重双转圈** ✅
- 位置：`render/input.rs`（运行中输入区改为静态 `运行中 · Esc/Ctrl+C 取消`，不再重复 spinner）
- 实机验证：运行中只有对话面板 agent 位置有转圈动画。

**B3. 推理折叠摘要加首句预览** ✅
- 位置：`render/message.rs` `folded_summary()`
- 实机验证：`[推理 ▸ 折叠 103 字符 · 「 The user wants to list files. Let me …」· Enter 展开]`

### 第 3 批：打磨（P2）

- **C1. /help 浮层打开时隐藏欢迎卡** ✅ `build_conversation_blocks` 加 `help_overlay.is_none()` 条件
- **C2. 浮层定位统一** ✅ 新增 `input_overlay(status_area)` 把 @ 补全、/help 锚定到状态行上方
  （输入区正上方，与斜杠命令浮层一致）；@ 补全加 `Clear` 消除叠层伪影；高度随候选数收缩。
  实机验证：两浮层均紧贴输入框上方、全宽、无叠层、欢迎卡隐藏。
- **C3. Esc 提示文案与 GUIDE 统一** ✅ 统一为"再按 Esc 退出"（state.rs echo、status.rs 提示行、
  GUIDE 872 同步）。

---

## 五、建议实施顺序

1. **先做 A1+A2+A4+A5**（状态栏、ctx、命令污染、暂停提示）——4 个改动互不依赖、单文件为主、
   每个都能带纯函数单测，是"第一印象"改善最大的一批。
2. **A3**（@ 补全）需要决定"接线还是撤文案"，接线约 30 行 + 测试。
3. **B1/B2/B3** 是体验增量，可并入手头任意一轮。
4. C 批是细枝末节，随改随带。

每批改完跑 `cargo test -p deepseeknova-tui` + `make check`，新增功能附单测（AGENTS.md 约定）。

---

## 六、代码审查结果（2026-08-07，审查本次全部改动）

审查范围：A1-A5 / B1-B3 / C1-C3 涉及的 13 个文件（tui 11 + cli 1 + store 1）。
最终状态：`cargo test`（tui 156 + cli 39 + store 17）全绿、clippy/fmt 干净、实机验证通过。

### 发现并已修复

| # | 严重度 | 问题 | 修复 |
|---|---|---|---|
| 1 | **高** | `cli/main.rs` `collect_at_files` 用 `path.is_dir()` 跟随符号链接：工作区含指向自身/祖先的目录 symlink 时无限递归，挂起 CLI 启动 | 用 `entry.file_type().is_symlink()` 跳过一切 symlink（不跟随）；补 symlink 环回归测试 |
| 2 | **中** | `render/message.rs` `input_overlay` 的 `height.min(y).max(3)`：可用高度 < 3 行时 h=3 > y，浮层溢出画到状态行上 | 改为 `height.clamp(1, y.max(1))`，永不溢出 |

### 已核验无问题

- `fit_status_line` 优先级回退链：丢弃顺序 lines→usage→turn→fold→cost→ctx，核心段（运行态+模型+退出警示）永不丢；丢弃后放得下直接返回不截断
- `handle_help_key` 滚动边界：scroll 上限与渲染 clamp 不一致但渲染端正确钳制，无 panic 风险
- `friendly_pause_reason`：已知形态精确匹配、未知形态原样保留、数字校验防误判
- `render_help_overlay`：空行/空列表/短终端均有保护，无 panic 路径
- `SessionMeta` 迁移：全仓库无遗漏引用，store 旧 `list_sessions` API 保留，serve 不受影响

### 轻微瑕疵（记录，暂不修）

- `fit_status_line` 极端窄终端（宽度 < 核心段总宽）截断时把各段拼成单字符串，丢失每段独立样式（全变 DIM）；功能正确，仅视觉降级
- `store::list_sessions_with_preview` 与 TUI 侧各自排序一次，双排序冗余但无害
- 侧边栏会话预览 16 字符截断，很多会话首句同为"你好"时仍难区分（数据问题，非代码问题）

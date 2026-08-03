# DeepseekNova TUI v2 全面重设计

- 日期：2026-08-03
- 状态：实现完成（2026-08-04，`feat/side-tasks` 工作树；`make check` 全绿），待用户评审收尾
- 范围：`crates/deepseeknova-tui` 单 crate 重构，公共 API（`TuiRunner` builder 链）保持不变，外部调用方零迁移
- 决策已确认：实时增量构建 Turn 树、可配置主题 `DEEPSEEKNOVA_THEME`

## 背景与现状问题

`deepseeknova-tui` 当前为单文件 `src/lib.rs`（2883 行），ratatui + crossterm，
功能完整（流式渲染、工具预览、diff 高亮、12 个斜杠命令、builder 注入式可选能力），
但存在五个结构性问题：

| # | 问题 | 具体表现 |
| --- | --- | --- |
| 1 | 单文件职责混杂 | 事件循环、命令路由、输入编辑器、渲染、样式、工具函数堆在一个文件 |
| 2 | 命令处理双路径分叉 | `TuiRunner::handle_command`（需外部依赖）+ `AppState::execute_command`（纯本地），新增命令要改两处 |
| 3 | 扁平行模型丢失消息边界 | `UiLine { kind, text }` 按行存储，消息/回合归属无法表达，无法做消息级导航/折叠/复制 |
| 4 | 流式缓冲竞态 | `pending_text` / `pending_reasoning` 以"flush 成行"处理，`ToolCallStart` 强制 flush reasoning，推理与正文交错时乱序 |
| 5 | 渲染与状态耦合、无焦点系统 | `draw()` 直接拼 `Paragraph`，无 widget 抽象；只有一个输入焦点，无法支撑会话列表/命令面板等新交互 |

## 方案选型

| 方案 | 结论 |
| --- | --- |
| A：消息树实时增量构建 + 模块化拆分（**选定**） | 事件流直接增量更新 Turn 树，内存低、渲染即时；模块边界清晰 |
| B：事件先落结构化日志再重放建树 | 调试友好但内存翻倍（双份数据），且重放延迟影响流式体验，弃 |
| C：最小修补（仅拆文件，保持行模型） | 消息边界问题不解决，新交互无法落地，弃 |

主题方案：**语义色映射表 + 环境变量切换**，默认值保持现 Codex 色板不变，
避免硬编码颜色散落（现代码已做到，重构时保持该性质并收敛到单一 `Theme` 类型）。

## 模块架构

```
crates/deepseeknova-tui/src/
├── lib.rs              # 公共 API：TuiRunner 组装 + builder（对外签名不变）
├── app/                # 事件循环、AppState、Focus 状态机、RunSession 代际
│   ├── mod.rs
│   ├── state.rs        # AppState（会话/显示状态，不含渲染）
│   ├── focus.rs        # Focus::Conversation/Input/Sidebar/Palette/Completion/Confirm
│   └── loop.rs         # 主事件循环（输入合并、批量重绘）
├── model/              # 消息树（实时增量构建）
│   ├── mod.rs
│   ├── conversation.rs # Conversation/Turn/Message/Segment + 折叠状态
│   └── apply.rs        # RunEvent → 消息树增量更新（单一入口）
├── render/             # 渲染层（widget 化）
│   ├── mod.rs
│   ├── layout.rs       # 面板布局（含窄终端降级）
│   ├── message.rs      # 消息卡渲染（含折叠渲染）
│   ├── sidebar.rs      # 侧边栏 Tab（会话/工具/MCP/成本/技能）
│   ├── palette.rs      # Ctrl+K 命令面板
│   ├── input.rs        # 输入区渲染（md 高亮 + @补全浮层）
│   └── status.rs       # 状态行 + 提示行
├── commands/           # 命令注册表（斜杠命令与 Ctrl+K 共用 handler）
│   ├── mod.rs          # CommandRegistry：name → {desc, args_spec, handler}
│   └── builtin.rs      # 现有 12 命令迁入 + 新增 /fold /copy
├── input/              # 输入编辑器（纯逻辑，可测）
│   ├── mod.rs
│   ├── editor.rs       # 现 InputState 迁移（多行编辑、UTF-8 边界）
│   ├── md_highlight.rs # 行级 markdown 着色
│   └── at_complete.rs  # @ 文件引用补全
└── theme.rs            # Theme 结构 + 环境变量解析 + 默认 Codex 色板
```

依赖方向：`app → model/commands/input/theme`；`render → model/theme`；
`commands → app/model/theme`（handler 收 `&mut AppContext`）。无循环。

## 消息模型（实时增量构建）

### 数据结构

```rust
/// 一次提交的完整生成回合
struct Turn {
    id: u64,
    user: Message,                    // Message::User
    assistant: AssistantTurn,         // 助手侧消息序列
    status: TurnStatus,               // Running | Done | Cancelled | Paused
}

/// 助手侧：消息树（正文一段、推理按段、工具调用含结果）
struct AssistantTurn {
    segments: Vec<Segment>,
    pending_reasoning: String,        // 未提交的推理增量
    pending_text: String,             // 未提交的正文增量
}

enum Segment {
    Reasoning { text: String },                       // 默认折叠
    Text { text: String },                            // 流式正文，整段提交
    ToolCall {
        name: String,
        arguments: String,
        result: Option<String>,                       // 截断 400 字符
        status: ToolStatus,                           // Running | Ok | Failed
    },
    Verification { command: String, passed: bool, summary: String },
    System { kind: SystemKind },                      // Paused | Approval | Info | Error
}

/// 稳定消息 id：(turn_id, 段序)，折叠/导航/复制/undo 均引用它
type SegId = (u64, usize);
```

### 增量更新规则（核心：解决乱序）

`apply(RunEvent)` 是消息树**唯一**变更入口：

1. `ReasoningDelta` → 追加 `pending_reasoning`，不落段
2. `TextDelta` → 先 `flush_reasoning()`（把 pending_reasoning 落为一个 `Reasoning` 段），
   再追加 `pending_text`
3. `ToolCallStart` → `flush_reasoning()` 后落 `ToolCall { status: Running }`
4. `ToolCallEnd` / `ToolResult` → 更新对应 ToolCall 段（arguments / result / status）
5. `TurnComplete` / `Done` → `flush_all()`，`status = Done`
6. `Paused` / `ApprovalRequest` / `Verification` → 落对应 System 段

关键不变量：**推理段只能在正文或工具调用开始前整体提交**，任何时刻
`pending_reasoning` 与已落段不交错，从根上消除现有"推理被工具调用从中间拆断"的乱序。

### 折叠策略

- 折叠状态独立存储：`HashMap<SegId, bool>`（会话内记忆），不在消息树内嵌
- 默认策略（智能折叠）：推理折叠（显示摘要头 `[推理 ▸ 折叠 N 字符]`）、
  工具调用展开、成功结果截断、失败/验证失败醒目展开
- 命令：`Enter` 切换当前消息、`/fold all|none` 批量、`/fold <segid>` 精确

## 布局与焦点

```
┌ 对话消息流（主区，消息卡渲染）───┐ [侧边栏 Tab（Ctrl+\ 开合，<90列自动隐藏）]
│ 你：…                          │  1 会话列表
│   ⚙ grep("pattern") (可展开)   │  2 工具活动
│   ✓ 验证: cargo check          │  3 MCP 状态
│   [推理 ▸ 折叠 312 字符]        │  4 成本
│                                │  5 技能
├─ 状态行 model | phase | turn | tokens | $ | 滚动 ──────────────────
├─ 输入区（md 高亮 + @补全浮层）────────────────────────────────────
└─ 上下文感知提示行（随焦点显示当前键位）───────────────────────────
```

焦点状态机：`Focus::Conversation / Input / Sidebar / Palette / Completion / Confirm`。
按键分发表按焦点路由（`mod key` 匹配，收敛现有 `handle_key` 大 match）：

- `Conversation`：`j/k` 消息导航、`Enter` 折叠切换、`y` 复制（剪贴板能力探测，降级打印）
- `Input`：现有编辑键位零破坏迁移（含历史/多行/滚动）
- `Sidebar`：`Tab` / `Ctrl+1..5` 切面板、`j/k` 列表导航
- `Palette`：`Ctrl+K` 打开，模糊搜索（标题/关键词），有参数命令内联子输入
- `Esc` 逐层退出模态；`Ctrl+\` 开合侧边栏

## 命令体系

`CommandRegistry` 单一注册表，两条入口共用 handler：

```rust
struct Command {
    name: &'static str,
    desc: &'static str,
    keywords: &'static [&'static str],   // Ctrl+K 模糊搜索用
    args_spec: ArgsSpec,                  // None | FreeText | Enum(&[&str])
    handler: fn(&mut AppContext, &str) -> CommandOutcome,  // 或异步包装
}
```

- 斜杠入口：输入 `/` 触发，回车分派到注册表
- 面板入口：`Ctrl+K` 打开，模糊搜索，选择后执行
- 现有 12 命令全部迁入（`help clear new sessions resume model cost skills mcp raw undo quit`），
  新增 `fold`（折叠控制）、`copy`（复制消息）
- 迁移要点：现有 `handle_command` 中依赖 builder 注入能力的命令（`/model`、`/cost`、
  `/mcp`、`/undo`、`/skills`、`/sessions`）改为 handler 收 `AppContext`，能力以
  `Option` 字段注入，缺失时降级提示（现状行为保留）

## 输入区增强

- **markdown 行级着色**：标题/代码围栏/列表/引用着色，只影响显示不改文本
- **@ 补全**：输入 `@` 触发，工作区文件模糊匹配（`↑↓` 选择、`Enter` 插入、`Esc` 关闭）；
  扫描数据由 CLI 经**新增** builder `with_workspace_root` 注入（仅新增方法，不改动
  任何既有 builder 签名，外部调用方零迁移），TUI 不直接碰文件系统（保持可测）
- **粘贴路径转 @ 引用**：粘贴含已存在路径的文本时自动转为 `@相对路径` 形式

## 主题（DEEPSEEKNOVA_THEME）

```rust
/// 语义色映射表；默认值 = 现 Codex 色板（user/status=cyan、agent=magenta、
/// 次要=dim、成功=green、失败=red），保证零配置行为不变
struct Theme {
    user: Style, agent: Style, reasoning: Style,
    tool: Style, tool_result: Style,
    verification_ok: Style, verification_fail: Style,
    system: Style, error: Style, paused: Style,
    accent: Color, dim: Modifier, border: Style,
}
```

- 解析链：`DEEPSEEKNOVA_THEME` 环境变量（`codex` | `dark` | `light`）→ 默认 `codex`
- `codex`：现语义色（不硬编码，深/浅终端通读）
- `dark` / `light`：预定义明暗前景色板（如 light 用深色前景保证对比度）
- 未知值回退 `codex` 并打印一条 System 提示（不阻塞启动）
- 现 `style_for(LineKind)` 迁移为 `theme.style_for(kind)`，`diff_spans` 接收 `&Theme`
- 新增 builder `with_theme(Theme)` 供编程式注入（优先级高于环境变量）

## 测试策略

- 纯逻辑全单测（延续现有风格）：消息树增量构建（乱序场景专项）、折叠状态、
  命令解析/注册表、@补全、md 着色、输入编辑（现有 20+ 测试迁移，覆盖不删）
- 渲染层：`TestBackend` 冒烟（布局、焦点切换、面板开合）
- 主题：默认=现 Codex 断言不变；`dark`/`light` 断言前景色变化；未知值回退
- 完成后跑 `make check`（含 clippy -D warnings、doc）

## 明确不做（暂缓项）

- 鼠标支持（ratatui 可做，本期不做）
- 多窗口/分屏会话对比
- 主题自定义文件（本期仅环境变量三档，`with_theme` 留编程式扩展面）
- TUI 与 desktop 前端主题打通（跨端统一，另行立项）

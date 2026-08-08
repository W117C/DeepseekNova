# deepseeknova-tui

ratatui-based interactive terminal UI for deepseeknova.

Wraps a `Runner` and displays its streaming event stream as a **structured message tree**
in a split-pane TUI:

- **Conversation pane** (top) — message-tree rendering: streaming text / reasoning
  (整段提交，默认折叠为摘要头) / tool calls with truncated results / deterministic
  verification (✓/✗) / pauses / approval requests / errors; 2000-segment cap.
- **Status bar** — model, phase, turn, token usage, session cost (`$`, refreshed from
  the model router ledger), **context occupancy** (`ctx N% (used / window)`, rendered
  when the main model's `context_window` is configured; >80% yellow, >95% red),
  scroll position.
- **Sidebar** (`Ctrl+\` toggle; auto-hidden on terminals narrower than 90 columns) —
  会话 / 工具活动 / MCP / 成本 / 技能 panels, `Tab` or `1..5` to switch;
  会话面板列出磁盘保存的会话（↑↓/j/k 选择、Enter 恢复）。
- **Input pane** (bottom) — multi-line editing with a visible cursor:
  `Shift+Enter` / `Ctrl+Enter` inserts a newline, `Home`/`End` move per line,
  horizontal window and vertical scroll both follow the cursor, input history,
  **markdown 行级着色** and **`@` 文件补全**（候选由 CLI 注入）。
- **Focus system** — `Tab` cycles 输入 → 消息导航（`j`/`k` 选中、`Enter` 折叠、
  `y` 复制）→ 侧边栏.
- **Command palette** — `/` 模糊搜索全部命令（与斜杠命令共用注册表）,
  有参数的命令内联子输入.
- **Hint line** — context-aware per-focus key hints.
- **Welcome card** — 首次启动（无对话）显示圆角欢迎卡（命令/快捷键/最近会话数）；
  等待 agent 回复时对话区显示转圈动画。

### Architecture

```
src/
├── lib.rs            # 公共 API 组装（TuiRunner + builder 链，外部零迁移）
├── app/              # 事件循环（run_loop）· AppState · Focus 状态机
├── model/            # Conversation/Turn/Segment 消息树 + apply(RunEvent) 增量入口
├── render/           # layout / message / sidebar / palette / input / status
├── commands/         # Command 注册表（斜杠 + `/` 同源）+ 内建命令 + 面板
├── input/            # 编辑器纯逻辑 · markdown 高亮 · @ 补全
└── theme.rs          # 语义配色 Theme + DEEPSEEKNOVA_THEME 解析
```

会话内容（消息树）是**唯一真相源**；命令反馈走独立的 echo 通道；渲染从树生成可见行。
折叠状态独立存储（`SegId → bool`），默认智能策略：推理折叠、工具展开、成功结果截断。

### Design

Follows a Claude Code-like semantic color model (190ac01, v0.5.0): user and
agent message bodies use the terminal's default foreground color (role shown by
`❯` / `⏺` markers instead of whole-line coloring), the brand blue `#4D6BFE` is
reserved for accents (prompt marker, `⏺`, model label), secondary information
(reasoning, tool calls, system messages) is dim, verification success is green,
and failures/errors are red. Diff output is highlighted per line (`+` green,
`-` red, `@@` accent). No hardcoded colors are used, so the UI reads correctly
in both light and dark terminals.

Theme presets via `DEEPSEEKNOVA_THEME` (`deepseek` default | `dark` | `light`;
unknown values fall back to `codex` with a notice). Programmatic injection via
`with_theme(Theme)` takes priority over the env var.

### Keys

- `Enter` submit; `Shift+Enter` / `Ctrl+Enter` newline; `Esc` quit when idle /
  close modal panels; `Ctrl+C` cancel the current run
- `Tab` focus cycle; `/` command palette; `Ctrl+\` sidebar toggle
- `↑`/`↓` input history (line movement when the input is multi-line);
  `←`/`→` cursor movement; `Home`/`End` per-line (idle); `Home`/`End` scroll when running
- `j`/`k` select message (navigation focus), `Enter` toggle fold, `y` copy
- 侧边栏会话面板：`↑`/`↓`（`j`/`k`）选择保存的会话，`Enter` 恢复
- `Backspace`/`Delete` edit; `Ctrl+U` clear input; `Ctrl+W` delete word before
- `PageUp`/`PageDown` scroll the conversation pane
- `鼠标滚轮` scrolls the conversation history (auto re-follows at the bottom)

### Slash commands

`/help` `/clear` `/new` `/sessions` `/resume <id>` `/model` (effort / thinking /
switch / use) `/cost` `/skills` `/mcp` `/raw` (normal/lite/raw) `/fold`
(all/none/reset) `/copy` `/undo` (`all` / `list`) `/quit`

Optional capabilities are enabled by builders:
`with_agent_factory` (`/model` hot-switch), `with_model_router`
(`/model use` + `/cost`), `with_session_controller`
(`/new` `/sessions` `/resume` + turn persistence), `with_skills_paths`,
`with_mcp_servers` + `with_mcp_probe` (`/mcp` live status: short-timeout spawn
probe with the real command argv that marks a still-alive stdio server as
connected), `with_undo_controller` (`/undo`), `with_theme` (programmatic theme),
`with_at_files` (`@` completion candidate list from the CLI).

```rust,no_run
use deepseeknova_tui::TuiRunner;
TuiRunner::new(runner).run().await?;
```

## License

Licensed under the same terms as deepseeknova.

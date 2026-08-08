//! 词表键枚举：TUI 全部用户可见文案的稳定标识符。
//!
//! # 结构（给桌面前端 Tauri 壳 P2 的契约）
//!
//! - **键 = 稳定标识符**：`Key` 变体名即键名，跨语言稳定。新增文案必须新增
//!   键，不得内联字符串（渲染路径全部经查表）。
//! - **每语言一个值**：`en()` 为英文默认值（兜底语言），`zh()` 为中文值；
//!   `zh()` 返回 `None` 的键表示“技术性/本就英文”文案，中文模式下回退英文
//!   （fail-safe，见 [`Key::tr`]）。
//! - **结构化文案用命名占位符**：`{name}` 形式，运行时经
//!   [`crate::i18n::Tr::t_args`] 插值（如 `Reached step limit ({n})`）。
//! - **桌面端复用方式**：前端用同一套键名（镜像枚举或共享 JSON），按
//!   `lang_code`（`"en"` / `"zh"`）取词。键名与占位符名是跨端契约，
//!   P2 壳接入时不得改名。

use super::Lang;

/// 全部用户可见文案的稳定键。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    // ── 会话/角色（app::state）────────────────────────────
    /// Esc 二次确认退出提示。
    PressEscAgain,
    /// 复制时无选中消息。
    NoSelectedMessage,
    /// 剪贴板不可用时回显提示（`{text}` 为消息正文）。
    ClipboardUnavailable,
    /// 折叠模式：默认。
    FoldDefault,
    /// 折叠模式：全部折叠。
    FoldAllFolded,
    /// 折叠模式：全部展开。
    FoldAllOpen,
    /// 折叠模式：混合。
    FoldMixed,
    /// 显示模式：normal。
    DisplayModeNormal,
    /// 显示模式：lite。
    DisplayModeLite,
    /// 显示模式：raw。
    DisplayModeRaw,

    // ── 侧边栏 Tab 标签（app::focus）──────────────────────
    TabSessions,
    TabTools,
    TabMcp,
    TabCost,
    TabSkills,

    // ── 鼠标捕获（app::focus）─────────────────────────────
    MouseCaptureOn,
    MouseCaptureOff,

    // ── 命令注册表描述（commands）─────────────────────────
    CmdHelpDesc,
    CmdClearDesc,
    CmdNewDesc,
    CmdSessionsDesc,
    CmdResumeDesc,
    CmdModelDesc,
    CmdCostDesc,
    CmdScorecardDesc,
    CmdSkillsDesc,
    CmdMcpDesc,
    CmdUndoDesc,
    CmdRawDesc,
    CmdFoldDesc,
    CmdCopyDesc,
    CmdQuitDesc,
    /// /rename 命令描述。
    CmdRenameDesc,
    /// /checkpoint 命令描述。
    CmdCheckpointDesc,

    // ── /help 浮层（commands::builtin）────────────────────
    HelpKeyCmdPalette,
    HelpKeyNav,
    HelpKeyEnter,
    HelpKeyY,
    HelpKeyPage,
    HelpKeyHistory,
    HelpKeyShiftEnter,
    HelpKeyCursor,
    HelpKeyEdit,
    HelpKeyCtrlUW,
    HelpKeyCtrlC,
    HelpFooter,

    // ── 内建命令反馈（commands::builtin）──────────────────
    /// 已清空对话面板。
    NoticeCleared,
    /// 新会话已开始。
    NoticeNewSession,
    /// 新建会话失败（`{err}`）。
    NewSessionFailed,
    /// 会话管理不可用。
    SessionUnavailable,
    /// 已保存会话列表头。
    SavedSessionsHeader,
    /// 会话列表中“当前”标记（内建命令，双空格）。
    SessionCurrentMarker,
    /// 还没有已保存的会话。
    NoSavedSessions,
    /// 列出会话失败（`{err}`）。
    ListSessionsFailed,
    /// 恢复会话完成（`{target}`、`{n}`）。
    ResumeDone,
    /// 恢复会话完成（带命名 title；`{target}`、`{title}`、`{n}`）。
    ResumeDoneTitled,
    /// 恢复会话失败（`{err}`）。
    ResumeFailed,
    /// /resume 用法。
    ResumeUsage,
    /// /rename 用法。
    RenameUsage,
    /// 重命名成功（`{title}`）。
    RenameDone,
    /// 重命名失败（`{err}`）。
    RenameFailed,
    /// 会话级检查点不可用。
    CheckpointUnavailable,
    /// /checkpoint 用法。
    CheckpointUsage,
    /// 检查点已保存（`{id}`）。
    CheckpointSaved,
    /// 保存检查点失败（`{err}`）。
    CheckpointSaveFailed,
    /// 检查点列表头。
    CheckpointListHeader,
    /// 列出检查点失败（`{err}`）。
    CheckpointListFailed,
    /// 还没有会话检查点。
    NoCheckpoints,
    /// 已回退到检查点（`{id}`、`{n}`）。
    CheckpointRollbackDone,
    /// 回退检查点失败（`{err}`）。
    CheckpointRollbackFailed,
    /// /checkpoint 未知参数（`{arg}`）。
    CheckpointUnknownArg,
    /// 模型切换不可用。
    ModelSwitchUnavailable,
    /// 模型已切换（`{effort}`、`{model}`）。
    ModelSwitched,
    /// 模型切换失败（`{err}`）。
    ModelSwitchFailed,
    /// /model 帮助标题（技术性，中文模式保持英文）。
    ModelCommandsHeader,
    /// /model 帮助行：显示当前。
    ModelHelpDisplay,
    /// /model 帮助行：effort。
    ModelHelpEffort,
    /// /model 帮助行：thinking。
    ModelHelpThinking,
    /// /model 帮助行：switch。
    ModelHelpSwitch,
    /// /model 帮助行：use。
    ModelHelpUse,
    /// 当前模型行（`{effort}`、`{model}`）。
    ModelCurrent,
    /// `(default)` 占位（技术性，中文模式保持英文）。
    DefaultLabel,
    /// 当前 effort 提示（`{effort}`、`{baseline}`）。
    EffortCurrent,
    /// thinking 切换（`{from}`、`{to}`；技术性，中文模式保持英文）。
    ThinkingToggle,
    /// thinking 状态未变。
    ThinkingUnchanged,
    /// /model switch 用法。
    ModelSwitchUsage,
    /// model pointers 不可用。
    ModelPointersUnavailable,
    /// /model use 用法。
    ModelUseUsage,
    /// 未知角色。
    UnknownRole,
    /// pointer 设置（`{role}`、`{model}`；技术性，中文模式保持英文）。
    PointerSet,
    /// router 不可用。
    RouterUnavailable,
    /// 未知 /model 子命令（`{cmd}`）。
    UnknownModelSubcommand,
    /// /cost 需要 router。
    CostRouterUnavailable,
    /// 还没有用量记录。
    NoUsageRecords,
    /// 成本总计（`{total}`）。
    CostTotal,
    /// 未计量调用（`{n}`）。
    UnmeteredCalls,
    /// 未找到评分卡。
    NoScorecardFound,
    /// 评分卡头部。
    ScorecardHeader,
    /// 可用技能列表头。
    SkillsHeader,
    /// 加载技能失败（`{path}`、`{err}`）。
    SkillsLoadFailed,
    /// 未找到技能。
    NoSkillsFound,
    /// 未配置 MCP 服务器（多行）。
    McpNotConfigured,
    /// MCP 列表头。
    McpHeader,
    /// MCP 已连接（`{name}`）。
    McpConnected,
    /// MCP 未连接（`{name}`、`{reason}`）。
    McpDisconnected,
    /// 撤销不可用。
    UndoUnavailable,
    /// 没有可回滚的快照。
    NoRollbackSnapshot,
    /// 撤销失败（`{err}`）。
    UndoFailed,
    /// 已全部回滚（`{n}`）。
    RolledBackAll,
    /// 快照列表头。
    SnapshotListHeader,
    /// 没有快照。
    NoSnapshots,
    /// 列出快照失败（`{err}`）。
    ListSnapshotsFailed,
    /// /undo 未知参数（`{arg}`）。
    UndoUnknownArg,
    /// 显示模式切换提示（`{mode}`）。
    DisplayModeNotice,
    /// 已折叠全部（`{state}`）。
    FoldedAll,
    /// 已展开全部（`{state}`）。
    ExpandedAll,
    /// 已重置折叠态。
    FoldReset,
    /// /fold 用法。
    FoldUsage,
    /// /fold 未知参数（`{arg}`）。
    FoldUnknownArg,
    /// 未提供 effort 级别。
    EffortMissing,
    /// 未知 effort 级别（`{effort}`）。
    EffortUnknown,

    // ── 事件循环（app）────────────────────────────────────
    /// 键位配置已热重载。
    KeymapReloaded,
    /// 外部编辑器失败（`{err}`）。
    ExternalEditorFailed,
    /// 运行器错误前缀（`{err}`；中英一致）。
    RunnerError,
    /// 运行器未注入。
    RunnerUnavailable,
    /// 已取消。
    Cancelled,
    /// 会话落盘失败（`{err}`）。
    SessionPersistFailed,
    /// 未知命令（`{cmd}`）。
    UnknownCommand,

    // ── TUI 启动（lib.rs）─────────────────────────────────
    /// 已加载键位定制（`{path}`、`{n}`）。
    KeymapLoaded,

    // ── 消息渲染（render::message）────────────────────────
    /// 验证行（`{mark}`、`{command}`）。
    VerificationLabel,
    /// 验证行含摘要（`{mark}`、`{command}`、`{summary}`）。
    VerificationWithSummary,
    /// 推理折叠摘要（`{n}`）。
    FoldedReasoning,
    /// 推理折叠摘要含首句预览（`{n}`、`{preview}`）。
    FoldedReasoningPreview,
    /// 工具折叠摘要（`{name}`）。
    FoldedTool,
    /// 通用折叠摘要。
    FoldedGeneric,
    /// 等待首批 delta（`{frame}`、`{verb}`、`{secs}`）。
    ThinkingWait,
    /// 运行态随机动词表（`|` 分隔，按 4s 轮转；Claude Code 风格）。
    ThinkingVerbs,
    /// 欢迎区副标题。
    WelcomeSubtitle,
    /// 欢迎区 /help 提示。
    WelcomeHelp,
    /// 欢迎区快捷键提示。
    WelcomeTips,
    /// 欢迎区会话计数（`{n}`）。
    WelcomeSessionsCount,
    /// 欢迎区会话提示（未加载）。
    WelcomeSessionsHint,
    /// 欢迎区工作目录行（`{path}`）。
    WelcomeCwd,
    /// /help 浮层标题。
    HelpTitle,
    /// /help 浮层翻页器（`{start}`、`{end}`、`{total}`）。
    HelpPager,
    /// /help 浮层翻页器（短版）。
    HelpPagerShort,

    // ── 状态行（render::status）───────────────────────────
    /// ctx 占用（`{bar}`、`{pct}`、`{used}`、`{window}`；中英一致）。
    CtxUsage,
    /// 退出警示。
    QuitWarning,
    /// 焦点提示：Conversation（5 个键位 + Esc）。
    HintConversation,
    /// 焦点提示：Input。
    HintInput,
    /// 焦点提示：Sidebar。
    HintSidebar,
    /// 焦点提示：Completion。
    HintCompletion,
    /// 焦点提示：Help。
    HintHelp,
    /// 焦点提示：Confirm。
    HintConfirm,

    // ── 输入区渲染（render::input）────────────────────────
    /// 运行中输入区提示。
    InputRunning,
    /// 斜杠候选标题：参数模式。
    CommandHintTitleArg,
    /// 斜杠候选标题：命令模式。
    CommandHintTitleCmd,
    /// @ 补全浮层标题。
    CompletionTitle,

    // ── 侧边栏渲染（render::sidebar）──────────────────────
    /// 面板标题。
    PanelTitle,
    /// MCP 面板提示。
    SidebarMcpHint,
    /// 技能面板提示。
    SidebarSkillsHint,
    /// 暂无历史会话。
    NoHistorySessions,
    /// 夜次分组头（`{night}`、`{n}`）。
    NightGroupHeader,
    /// 会话列表“当前”后缀（单空格）。
    SessionCurrentSuffix,
    /// 还有更多会话（`{n}`）。
    MoreSessions,
    /// 本次会话分隔线。
    CurrentSessionDivider,
    /// 空回合（`{id}`）。
    TurnEmpty,
    /// 暂无工具调用。
    NoToolCalls,
    /// 工具活动头部（`{n}`）。
    ToolActivityHeader,
    /// 工具计数行（`{name_col}`、`{n}`、`{suffix}`）。
    ToolCallCountLine,
    /// 会话成本（`{cost}`）。
    SessionCost,
    /// 成本不可用。
    CostUnavailable,
    /// usage 简报（`{up}`、`{down}`、`{total}`；中英一致）。
    UsageBrief,
    /// /cost 明细提示。
    SidebarCostHint,
    /// 评分卡面板标题。
    ScorecardPanelTitle,
    /// 评分卡待完成提示。
    ScorecardPending,

    // ── 审批浮层（render::approval）───────────────────────
    /// 风险标签：只读。
    RiskReadonly,
    /// 风险标签：非只读。
    RiskNonReadonly,
    /// 风险标签：危险。
    RiskDanger,
    /// 风险标签前缀（`{label}`）。
    RiskLabel,
    /// 审批键位提示。
    ApprovalHint,
    /// 审批浮层标题。
    ApprovalTitle,

    // ── 权限模式与工作区信任（P1-6）────────────────────────
    /// 权限模式：plan。
    PermModePlan,
    /// 权限模式：accept_edits。
    PermModeAcceptEdits,
    /// 权限模式：auto。
    PermModeAuto,
    /// 权限模式：default（gate 未设预设，旧行为）。
    PermModeLegacy,
    /// 状态行权限模式指示（`{mode}`）。
    PermModeIndicator,
    /// 模式切换反馈（`{mode}`）。
    PermModeNotice,
    /// 模式切换不可用（未注入 PermissionGate）。
    PermModeGateUnavailable,
    /// /mode 用法。
    PermModeUsage,
    /// 命令描述：/mode。
    CmdModeDesc,
    /// 审批浮层的当前模式行（`{mode}`）。
    ApprovalModeLine,
    /// 信任确认浮层标题。
    TrustTitle,
    /// 信任确认正文（`{root}`、`{n}`）。
    TrustPromptBody,
    /// 信任确认键位提示。
    TrustHint,
    /// 已信任反馈。
    TrustAccepted,
    /// 未信任反馈。
    TrustRejected,

    // ── 消息树系统段（model::apply）───────────────────────
    /// 暂停：步骤上限（`{n}`）。
    PauseMaxSteps,
    /// 暂停：步骤上限（无参）。
    PauseMaxStepsPlain,
    /// 暂停：预算上限（`{why}`）。
    PauseBudget,
    /// 暂停：通用（`{reason}`）。
    PauseGeneric,
    /// 质量检查段（`{severity}`、`{passed}`、`{rule}`、`{evidence}`）。
    QualityFinding,
    /// 阶段迁移段（`{phase}`、`{outcome}`）。
    PhaseTransition,
    /// 门控违规段（`{gate}`、`{detail}`）。
    GateViolation,
    /// drift 段（`{family}`、`{n}`、`{detail}`）。
    DriftFinding,
    /// 授权请求段（`{title}`）。
    ApprovalRequestLine,
    /// 授权请求段含描述（`{title}`、`{desc}`）。
    ApprovalRequestLineDesc,
    /// 恢复会话提示（`{id}`）。
    ResumeHint,

    // ── 评分卡维度标签（model::scorecard 数据展示）────────
    ScorecardDimGovernance,
    ScorecardDimVerification,
    ScorecardDimReflection,
    ScorecardDimReview,
    ScorecardDimProtocol,
    ScorecardDimComposite,

    // ── 键位定制诊断（app::keybindings）───────────────────
    /// parse_error：JSON 解析失败（`{err}`）。
    KeymapParseError,
    /// parse_error：缺少 bindings 数组。
    KeymapMissingBindings,
    /// invalid_context：缺少 context 字段（`{gi}`）。
    KeymapMissingContext,
    /// invalid_context：未知 context（`{gi}`、`{ctx}`）。
    KeymapUnknownContext,
    /// invalid_context：缺少 bindings 对象（`{gi}`）。
    KeymapMissingBindingsObj,
    /// parse_error：无法解析键（`{ctx}`、`{key}`）。
    KeymapUnparseableKey,
    /// invalid_action：值须为 action 或 null（`{ctx}`、`{key}`）。
    KeymapActionOrNull,
    /// reserved：保留键不可重绑（`{ctx}`、`{key}`、`{reason}`）。
    KeymapReserved,
    /// parse_error：键格式说明（`{ctx}`、`{key}`）。
    KeymapParseKeyHelp,
    /// invalid_action：未知 action（`{ctx}`、`{action}`）。
    KeymapUnknownAction,
    /// duplicate：重复绑定（`{ctx}`、`{key}`）。
    KeymapDuplicate,
    /// 保留键原因：ctrl+c。
    ReservedCtrlC,
    /// 保留键原因：ctrl+d。
    ReservedCtrlD,
    /// 保留键原因：ctrl+m。
    ReservedCtrlM,
    /// 保留键原因：ctrl+z。
    ReservedCtrlZ,
    /// 保留键原因：ctrl+\。
    ReservedCtrlBackslash,

    // ── 主题（theme.rs）───────────────────────────────────
    /// 未知主题回退（`{theme}`）。
    ThemeUnknownFallback,
}

impl Key {
    /// 按语言取词；中文缺失的键回退英文（fail-safe）。
    pub fn tr(self, lang: Lang) -> &'static str {
        match lang {
            Lang::En => self.en(),
            Lang::Zh => self.zh().unwrap_or_else(|| self.en()),
        }
    }

    /// 英文值（默认/兜底语言，全部键必须提供）。
    pub fn en(self) -> &'static str {
        use Key::*;
        match self {
            // 会话/角色
            PressEscAgain => "Press Esc again to exit",
            NoSelectedMessage => "No message selected (use j/k to select)",
            ClipboardUnavailable => "📋 {text} (clipboard unavailable, echoed instead)",
            FoldDefault => "default",
            FoldAllFolded => "all folded",
            FoldAllOpen => "all open",
            FoldMixed => "mixed",
            DisplayModeNormal => "normal (full)",
            DisplayModeLite => "lite (hide reasoning)",
            DisplayModeRaw => "raw (typed prefix)",

            // 侧边栏 Tab
            TabSessions => "Sessions",
            TabTools => "Tools",
            TabMcp => "MCP",
            TabCost => "Cost",
            TabSkills => "Skills",

            // 鼠标捕获
            MouseCaptureOn => "Mouse capture on: wheel scrolls conversation (Ctrl+T to select text)",
            MouseCaptureOff => "Mouse capture off: select/copy with mouse (Ctrl+T restores wheel)",

            // 命令描述
            CmdHelpDesc => "Show help and all commands",
            CmdClearDesc => "Clear the conversation pane",
            CmdNewDesc => "Start a new session",
            CmdSessionsDesc => "List saved sessions",
            CmdResumeDesc => "Resume a saved session",
            CmdModelDesc => "Hot-switch model and effort",
            CmdCostDesc => "Session cost report",
            CmdScorecardDesc => "Load latest photometry scorecard (six dimensions)",
            CmdSkillsDesc => "List available skills",
            CmdMcpDesc => "List configured MCP servers (live status)",
            CmdUndoDesc => "Roll back snapshots (all/list)",
            CmdRawDesc => "Cycle display mode (normal/lite/raw)",
            CmdFoldDesc => "Fold control (all/none/reset)",
            CmdCopyDesc => "Copy the selected message",
            CmdQuitDesc => "Quit TUI",
            CmdRenameDesc => "Rename the current session",
            CmdCheckpointDesc => "Save / list / roll back session checkpoints",

            // /help 浮层
            HelpKeyCmdPalette => "  Ctrl+K         Command palette",
            HelpKeyNav => "  j/k            Conversation: navigate messages",
            HelpKeyEnter => "  Enter          Fold/expand selected message",
            HelpKeyY => "  y              Copy selected message",
            HelpKeyPage => "  PageUp/Down    Scroll back",
            HelpKeyHistory => "  ↑/↓            Input history (move cursor when multiline)",
            HelpKeyShiftEnter => "  Shift+Enter    Newline (same as Ctrl+J)",
            HelpKeyCursor => "  ←/→/Home/End  Move cursor in input (when idle)",
            HelpKeyEdit => "  Delete/Backspace  Edit input",
            HelpKeyCtrlUW => "  Ctrl+U/W       Clear input / delete previous word",
            HelpKeyCtrlC => "  Ctrl+C         Cancel current run",
            HelpFooter => "  j/k or ↑/↓ scroll · Esc or q closes",

            // 内建命令反馈
            NoticeCleared => "Conversation cleared",
            NoticeNewSession => "New session started",
            NewSessionFailed => "Failed to create session: {err}",
            SessionUnavailable => "Session management unavailable (no SessionController)",
            SavedSessionsHeader => "Saved sessions (newest first):",
            SessionCurrentMarker => "  (current)",
            NoSavedSessions => "(no saved sessions yet)",
            ListSessionsFailed => "Failed to list sessions: {err}",
            ResumeDone => "Resumed '{target}' — {n} messages (in conversation pane, scroll/fold)",
            ResumeDoneTitled => "Resumed '{target}' ('{title}') — {n} messages (in conversation pane, scroll/fold)",
            ResumeFailed => "Failed to resume session: {err}",
            ResumeUsage => "Usage: /resume <session-id> (see /sessions)",
            RenameUsage => "Usage: /rename <title>",
            RenameDone => "Session renamed to '{title}'",
            RenameFailed => "Failed to rename session: {err}",
            CheckpointUnavailable => "Session checkpoint unavailable (no CheckpointController)",
            CheckpointUsage => "Usage: /checkpoint save [label] | /checkpoint list | /checkpoint rollback [id]",
            CheckpointSaved => "Checkpoint saved: {id}",
            CheckpointSaveFailed => "Failed to save checkpoint: {err}",
            CheckpointListHeader => "Session checkpoints (newest first):",
            CheckpointListFailed => "Failed to list checkpoints: {err}",
            NoCheckpoints => "(no session checkpoints yet)",
            CheckpointRollbackDone => "Rolled back to checkpoint '{id}' ({n} messages)",
            CheckpointRollbackFailed => "Failed to roll back checkpoint: {err}",
            CheckpointUnknownArg => "Unknown /checkpoint argument: {arg} (usage: /checkpoint save [label] | list | rollback [id])",
            ModelSwitchUnavailable => "Model switch unavailable (no agent factory)",
            ModelSwitched => "Model switched: effort={effort} model={model}",
            ModelSwitchFailed => "Failed to switch model: {err}",
            ModelCommandsHeader => "Model commands:",
            ModelHelpDisplay => "  /model                  Show current model and help",
            ModelHelpEffort => "  /model effort <level>   Set reasoning effort: disabled|high|max",
            ModelHelpThinking => "  /model thinking         Toggle thinking on/off",
            ModelHelpSwitch => "  /model switch <name>    Switch to a model",
            ModelHelpUse => "  /model use <role> <name> Set role pointer: main|task|compact|quick",
            ModelCurrent => "Current: effort={effort} model={model}",
            DefaultLabel => "(default)",
            EffortCurrent => "Current reasoning effort: {effort} (baseline: {baseline}); usage: /model effort disabled|high|max",
            ThinkingToggle => "thinking {from} → {to}",
            ThinkingUnchanged => "thinking state unchanged",
            ModelSwitchUsage => "Usage: /model switch <provider-model-name>",
            ModelPointersUnavailable => "model pointers unavailable (no router)",
            ModelUseUsage => "Usage: /model use <main|task|compact|quick> <model-name>",
            UnknownRole => "Unknown role (main|task|compact|quick)",
            PointerSet => "pointer {role} → {model}",
            RouterUnavailable => "router unavailable",
            UnknownModelSubcommand => "Unknown /model subcommand: {cmd} (see /model help)",
            CostRouterUnavailable => "router unavailable (/cost needs ModelRouter)",
            NoUsageRecords => "No usage records yet",
            CostTotal => "Total: ${total}",
            UnmeteredCalls => "({n} unmetered calls)",
            NoScorecardFound => "No photometry data found (no scorecard JSON in .deepseeknova/metrics)",
            ScorecardHeader => "Photometry · Scorecard (latest run)",
            SkillsHeader => "Available skills:",
            SkillsLoadFailed => "Failed to load skills from {path}: {err}",
            NoSkillsFound => "(no skills found — put .md files in .deepseeknova/skills/)",
            McpNotConfigured => "No MCP servers configured\nConfigure them in the mcp_servers array at the top of deepseeknova.toml and restart",
            McpHeader => "Configured MCP servers (live status):",
            McpConnected => "  • {name} — ✓ connected",
            McpDisconnected => "  • {name} — ✗ disconnected ({reason})",
            UndoUnavailable => "Undo unavailable (no UndoController)",
            NoRollbackSnapshot => "No snapshot to roll back",
            UndoFailed => "Undo failed: {err}",
            RolledBackAll => "Rolled back {n} snapshots",
            SnapshotListHeader => "Snapshot list:",
            NoSnapshots => "(no snapshots)",
            ListSnapshotsFailed => "Failed to list snapshots: {err}",
            UndoUnknownArg => "Unknown argument: {arg} (usage: /undo | /undo all | /undo list)",
            DisplayModeNotice => "Display mode: {mode}",
            FoldedAll => "All messages folded (current: {state})",
            ExpandedAll => "All messages expanded (current: {state})",
            FoldReset => "Fold state reset (smart default)",
            FoldUsage => "Usage: /fold all | none | reset",
            FoldUnknownArg => "Unknown argument: {arg} (all|none|reset)",
            EffortMissing => "No effort level provided",
            EffortUnknown => "Unknown effort level: '{effort}'",

            // 事件循环
            KeymapReloaded => "Keybindings hot-reloaded",
            ExternalEditorFailed => "External editor failed: {err}",
            RunnerError => "❌ {err}",
            RunnerUnavailable => "❌ runner unavailable (not injected)",
            Cancelled => "Cancelled (Ctrl+C / Esc)",
            SessionPersistFailed => "Failed to persist session: {err}",
            UnknownCommand => "Unknown command: /{cmd} (see /help)",

            // 启动
            KeymapLoaded => "Loaded keymap {path} ({n} overrides)",

            // 消息渲染
            VerificationLabel => "{mark} Verify: {command}",
            VerificationWithSummary => "{mark} Verify: {command} — {summary}",
            FoldedReasoning => "[reasoning ▸ folded {n} chars · Enter to expand]",
            FoldedReasoningPreview => "[reasoning ▸ folded {n} chars · \"{preview}\" · Enter to expand]",
            FoldedTool => "[tool ▸ {name} folded · Enter to expand]",
            FoldedGeneric => "[folded · Enter to expand]",
            ThinkingWait => "{frame} {verb}… ({secs}s · Ctrl+C to cancel)",
            ThinkingVerbs => "Thinking|Pondering|Reasoning|Mulling|Deliberating|Crunching",
            WelcomeSubtitle => "AI Agent terminal · sessions auto-persist",
            WelcomeHelp => "Type /help to see all commands",
            WelcomeTips => "Tab switches focus · Ctrl+\\ sidebar · mouse wheel scrolls history",
            WelcomeSessionsCount => "Saved sessions: {n} (restore in sidebar/Sessions)",
            WelcomeSessionsHint => "Saved sessions: see sidebar/Sessions",
            WelcomeCwd => "cwd: {path}",
            HelpTitle => "  Help · Keybindings",
            HelpPager => " · {start}-{end}/{total} lines · j/k scroll · Esc close",
            HelpPagerShort => " · Esc close",

            // 状态行
            CtxUsage => " │ ctx [{bar}] {pct}% ({used} / {window})",
            QuitWarning => " │ ⚠ Press Esc again to exit",
            HintConversation => "{nav} navigate · {fold} fold · {copy} copy · {page} page · {top} top/bottom · Esc back to input",
            HintInput => "/ commands · Ctrl+P perm mode · Ctrl+T mouse · Ctrl+U clear · Ctrl+W delete word · Shift+Enter newline · Esc cancel / Esc Esc exit",
            HintSidebar => "{nav} select session · Enter resume · {tab} switch panel · Esc close",
            HintCompletion => "↑↓ select · Enter insert · Esc close",
            HintHelp => "j/k or ↑/↓ scroll · PageUp/Down page · Esc/q close",
            HintConfirm => "y confirm · n/Esc cancel",

            // 输入区
            InputRunning => "Running · Esc/Ctrl+C to cancel",
            CommandHintTitleArg => "arguments · ↑↓ select · Enter run · Tab complete · Esc close",
            CommandHintTitleCmd => "commands · ↑↓ select · Enter run · Tab complete · Esc close",
            CompletionTitle => "@ file completion",

            // 侧边栏
            PanelTitle => "Panel",
            SidebarMcpHint => " Run /mcp to see server status",
            SidebarSkillsHint => " Run /skills to see available skills",
            NoHistorySessions => " (no saved sessions yet)",
            NightGroupHeader => " ▾ {night} · {n}",
            SessionCurrentSuffix => " (current)",
            MoreSessions => "  …{n} more (see /sessions for all)",
            CurrentSessionDivider => " ── current session ──",
            TurnEmpty => "#{id} (empty)",
            NoToolCalls => " (no tool calls yet)",
            ToolActivityHeader => " Tool activity · {n} tools",
            ToolCallCountLine => " {name_col} {n} calls  [{suffix}]",
            SessionCost => " Session cost: ${cost}",
            CostUnavailable => " Cost unavailable (no router)",
            UsageBrief => " ↑{up} ↓{down} Σ{total}",
            SidebarCostHint => " Run /cost to see details",
            ScorecardPanelTitle => " Photometry · Scorecard",
            ScorecardPending => " Photometry pending (run /scorecard)",

            // 审批浮层
            RiskReadonly => "read-only",
            RiskNonReadonly => "non-read-only",
            RiskDanger => "dangerous",
            RiskLabel => " Risk: {label}",
            ApprovalHint => "y allow · n deny · Esc deny",
            ApprovalTitle => "🔒 Request authorization",

            // 权限模式与工作区信任
            PermModePlan => "plan",
            PermModeAcceptEdits => "accept_edits",
            PermModeAuto => "auto",
            PermModeLegacy => "default",
            PermModeIndicator => " │ perm {mode}",
            PermModeNotice => "Permission mode: {mode}",
            PermModeGateUnavailable => "Permission mode switching unavailable (no gate)",
            PermModeUsage => "Usage: /mode [plan|accept_edits|auto|cycle]",
            CmdModeDesc => "Switch permission mode preset (plan/accept_edits/auto)",
            ApprovalModeLine => " Current permission mode: {mode}",
            TrustTitle => "🔒 Trust workspace?",
            TrustPromptBody => "This project's config carries permission rules ({n}).\nTrust this workspace so project allow rules take effect?\nWorkspace: {root}\n(untrusted: project allow rules degrade to ask)",
            TrustHint => "y trust · n untrusted · Esc untrusted",
            TrustAccepted => "Workspace trusted — project allow rules active",
            TrustRejected => "Workspace untrusted — project allow rules degraded to ask",

            // 系统段
            PauseMaxSteps => "Reached step limit ({n}), task incomplete",
            PauseMaxStepsPlain => "Reached step limit, task incomplete",
            PauseBudget => "Reached budget limit: {why}",
            PauseGeneric => "Task paused: {reason}",
            QualityFinding => "🔎 Quality check [{severity} {passed}] {rule}: {evidence}",
            PhaseTransition => "🔁 Phase transition: {phase} ({outcome})",
            GateViolation => "🚧 Gate violation: {gate} — {detail}",
            DriftFinding => "🧭 drift: {family} failed {n} times consecutively — {detail}",
            ApprovalRequestLine => "🔒 Request authorization: {title}",
            ApprovalRequestLineDesc => "🔒 Request authorization: {title} — {desc}",
            ResumeHint => "Enter /resume {id} to continue, or send a new instruction",

            // 评分卡维度
            ScorecardDimGovernance => "Governance",
            ScorecardDimVerification => "Verification",
            ScorecardDimReflection => "Reflection",
            ScorecardDimReview => "Review",
            ScorecardDimProtocol => "Protocol",
            ScorecardDimComposite => "Composite",

            // 键位诊断
            KeymapParseError => "parse_error: JSON parse failed — {err} (check quotes and commas)",
            KeymapMissingBindings => "parse_error: missing \"bindings\" array (see module docs for an example)",
            KeymapMissingContext => "invalid_context[#{gi}]: missing \"context\" string field",
            KeymapUnknownContext => "invalid_context[#{gi}]: unknown context '{ctx}' (available: Input/Conversation/Sidebar/Completion)",
            KeymapMissingBindingsObj => "invalid_context[#{gi}]: missing \"bindings\" object",
            KeymapUnparseableKey => "parse_error[{ctx}]: cannot parse key '{key}'",
            KeymapActionOrNull => "invalid_action[{ctx}]: value for key '{key}' must be an action name or null",
            KeymapReserved => "reserved[{ctx}]: key '{key}' cannot be rebound — {reason}",
            KeymapParseKeyHelp => "parse_error[{ctx}]: cannot parse key '{key}' (supported: ctrl/alt/shift/super + key name or single character)",
            KeymapUnknownAction => "invalid_action[{ctx}]: unknown action '{action}' (available: chat:submit / conv:scrollTop / modal:dismiss etc, see /help)",
            KeymapDuplicate => "duplicate[{ctx}]: key '{key}' bound twice (later takes effect)",
            ReservedCtrlC => "terminal interrupt signal, reserved by the OS",
            ReservedCtrlD => "terminal EOF (exits shell on empty line)",
            ReservedCtrlM => "equivalent to Enter in the terminal",
            ReservedCtrlZ => "SIGTSTP suspends the process",
            ReservedCtrlBackslash => "SIGQUIT terminate signal",

            // 主题
            ThemeUnknownFallback => "Unknown theme '{theme}' (deepseek|dark|light), falling back to deepseek",
        }
    }

    /// 中文值；返回 `None` 表示该文案技术性/本就英文，中文模式回退英文。
    pub fn zh(self) -> Option<&'static str> {
        use Key::*;
        Some(match self {
            // 会话/角色
            PressEscAgain => "再按 Esc 退出",
            NoSelectedMessage => "没有选中的消息（先 j/k 选中）",
            ClipboardUnavailable => "📋 {text}（剪贴板不可用，已回显文本）",
            FoldDefault => "默认",
            FoldAllFolded => "全折叠",
            FoldAllOpen => "全展开",
            FoldMixed => "混合",
            DisplayModeNormal => "normal（全量）",
            DisplayModeLite => "lite（隐藏推理）",
            DisplayModeRaw => "raw（带类型前缀）",

            // 侧边栏 Tab
            TabSessions => "会话",
            TabTools => "工具活动",
            TabMcp => "MCP",
            TabCost => "成本",
            TabSkills => "技能",

            // 鼠标捕获
            MouseCaptureOn => "鼠标捕获已开启：滚轮滚动对话历史（Ctrl+T 切换为选中文本）",
            MouseCaptureOff => "鼠标捕获已关闭：可用鼠标选中/复制文本（Ctrl+T 恢复滚轮）",

            // 命令描述
            CmdHelpDesc => "显示帮助与全部命令",
            CmdClearDesc => "清空对话面板",
            CmdNewDesc => "开始新会话",
            CmdSessionsDesc => "列出已保存会话",
            CmdResumeDesc => "恢复指定会话",
            CmdModelDesc => "模型与 effort 热切换",
            CmdCostDesc => "会话成本报表",
            CmdScorecardDesc => "读取最新测光评分卡（六维光度表）",
            CmdSkillsDesc => "列出可用技能",
            CmdMcpDesc => "列出已配置 MCP 服务器（实时状态）",
            CmdUndoDesc => "回滚快照（all/list）",
            CmdRawDesc => "切换显示模式（normal/lite/raw）",
            CmdFoldDesc => "折叠控制（all/none/reset）",
            CmdCopyDesc => "复制当前选中消息",
            CmdQuitDesc => "退出 TUI",
            CmdRenameDesc => "重命名当前会话",
            CmdCheckpointDesc => "会话检查点（保存/列表/回退）",

            // /help 浮层
            HelpKeyCmdPalette => "  Ctrl+K         命令面板",
            HelpKeyNav => "  j/k            Conversation 焦点下消息导航",
            HelpKeyEnter => "  Enter          折叠/展开选中消息",
            HelpKeyY => "  y              复制选中消息",
            HelpKeyPage => "  PageUp/Down    滚动回看",
            HelpKeyHistory => "  ↑/↓            输入历史（多行时移动光标）",
            HelpKeyShiftEnter => "  Shift+Enter    换行（Ctrl+J 同）",
            HelpKeyCursor => "  ←/→/Home/End  输入内移动光标（空闲时）",
            HelpKeyEdit => "  Delete/Backspace 编辑输入",
            HelpKeyCtrlUW => "  Ctrl+U/W       清空输入 / 删前一词",
            HelpKeyCtrlC => "  Ctrl+C         取消当前运行",
            HelpFooter => "  j/k 或 ↑/↓ 滚动 · Esc 或 q 关闭",

            // 内建命令反馈
            NoticeCleared => "已清空对话面板",
            NoticeNewSession => "新会话已开始",
            NewSessionFailed => "新建会话失败: {err}",
            SessionUnavailable => "会话管理不可用（未提供 SessionController）",
            SavedSessionsHeader => "已保存会话（最新优先）:",
            SessionCurrentMarker => "  (当前)",
            NoSavedSessions => "（还没有已保存的会话）",
            ListSessionsFailed => "列出会话失败: {err}",
            ResumeDone => "已恢复 '{target}' — {n} 条消息（进入对话面板，可滚动/折叠）",
            ResumeDoneTitled => "已恢复 '{target}'（'{title}'）— {n} 条消息（进入对话面板，可滚动/折叠）",
            ResumeFailed => "恢复会话失败: {err}",
            ResumeUsage => "用法: /resume <session-id>（见 /sessions）",
            RenameUsage => "用法: /rename <title>",
            RenameDone => "已将会话重命名为 '{title}'",
            RenameFailed => "重命名会话失败: {err}",
            CheckpointUnavailable => "会话检查点不可用（未注入 CheckpointController）",
            CheckpointUsage => "用法: /checkpoint save [标签] | /checkpoint list | /checkpoint rollback [id]",
            CheckpointSaved => "检查点已保存: {id}",
            CheckpointSaveFailed => "保存检查点失败: {err}",
            CheckpointListHeader => "会话检查点（最新优先）:",
            CheckpointListFailed => "列出检查点失败: {err}",
            NoCheckpoints => "（还没有会话检查点）",
            CheckpointRollbackDone => "已回退到检查点 '{id}'（{n} 条消息）",
            CheckpointRollbackFailed => "回退检查点失败: {err}",
            CheckpointUnknownArg => "未知 /checkpoint 参数: {arg}（用法: /checkpoint save [标签] | list | rollback [id]）",
            ModelSwitchUnavailable => "模型切换不可用（未提供 agent 工厂）",
            ModelSwitched => "模型已切换: effort={effort} model={model}",
            ModelSwitchFailed => "模型切换失败: {err}",
            ModelHelpDisplay => "  /model                  显示当前模型与帮助",
            ModelHelpEffort => "  /model effort <level>   设置 reasoning effort: disabled|high|max",
            ModelHelpThinking => "  /model thinking         切换 thinking 开/关",
            ModelHelpSwitch => "  /model switch <name>    切换到指定模型",
            ModelHelpUse => "  /model use <role> <name> 设置角色指针: main|task|compact|quick",
            ModelCurrent => "当前: effort={effort} model={model}",
            EffortCurrent => "当前 reasoning effort: {effort} (基线: {baseline}); 用法: /model effort disabled|high|max",
            ThinkingUnchanged => "thinking 状态未变",
            ModelSwitchUsage => "用法: /model switch <provider-model-name>",
            ModelPointersUnavailable => "model pointers 不可用（未提供 router）",
            ModelUseUsage => "用法: /model use <main|task|compact|quick> <model-name>",
            UnknownRole => "未知角色（main|task|compact|quick）",
            RouterUnavailable => "router 不可用",
            UnknownModelSubcommand => "未知 /model 子命令: {cmd}（/model help 查看）",
            CostRouterUnavailable => "router 不可用（/cost 需要 ModelRouter）",
            NoUsageRecords => "还没有用量记录",
            CostTotal => "总计: ${total}",
            UnmeteredCalls => "（未计量调用: {n}）",
            NoScorecardFound => "未找到测光数据（.deepseeknova/metrics 无评分卡 JSON）",
            ScorecardHeader => "测光·评分卡（最近一次 run）",
            SkillsHeader => "可用技能:",
            SkillsLoadFailed => "加载技能失败 {path}: {err}",
            NoSkillsFound => "（未找到技能，可创建 .md 文件放到 .deepseeknova/skills/）",
            McpNotConfigured => "未配置 MCP 服务器\n在 deepseeknova.toml 顶层 mcp_servers 数组配置后重启生效",
            McpHeader => "已配置 MCP 服务器（实时状态）:",
            McpConnected => "  • {name} — ✓ 已连接",
            McpDisconnected => "  • {name} — ✗ 未连接（{reason}）",
            UndoUnavailable => "撤销不可用（未提供 UndoController）",
            NoRollbackSnapshot => "没有可回滚的快照",
            UndoFailed => "撤销失败: {err}",
            RolledBackAll => "已全部回滚 {n} 个快照",
            SnapshotListHeader => "快照列表:",
            NoSnapshots => "（没有快照）",
            ListSnapshotsFailed => "列出快照失败: {err}",
            UndoUnknownArg => "未知参数: {arg}（用法: /undo | /undo all | /undo list）",
            DisplayModeNotice => "显示模式: {mode}",
            FoldedAll => "已折叠全部消息（当前: {state}）",
            ExpandedAll => "已展开全部消息（当前: {state}）",
            FoldReset => "已重置折叠态（回智能默认）",
            FoldUsage => "用法: /fold all | none | reset",
            FoldUnknownArg => "未知参数: {arg}（all|none|reset）",
            EffortMissing => "未提供 effort 级别",
            EffortUnknown => "未知 effort 级别: '{effort}'",

            // 事件循环
            KeymapReloaded => "键位配置已热重载",
            ExternalEditorFailed => "外部编辑器失败: {err}",
            RunnerUnavailable => "❌ runner 不可用（未注入）",
            Cancelled => "已取消（Ctrl+C / Esc）",
            SessionPersistFailed => "会话落盘失败: {err}",
            UnknownCommand => "未知命令: /{cmd}（/help 查看）",

            // 启动
            KeymapLoaded => "已加载键位定制 {path}（{n} 条覆盖）",

            // 消息渲染
            VerificationLabel => "{mark} 验证: {command}",
            VerificationWithSummary => "{mark} 验证: {command} — {summary}",
            FoldedReasoning => "[推理 ▸ 折叠 {n} 字符 · Enter 展开]",
            FoldedReasoningPreview => "[推理 ▸ 折叠 {n} 字符 · 「{preview}」· Enter 展开]",
            FoldedTool => "[工具 ▸ {name} 已折叠 · Enter 展开]",
            FoldedGeneric => "[已折叠 · Enter 展开]",
            ThinkingWait => "{frame} {verb}…（{secs}s · Ctrl+C 取消）",
            ThinkingVerbs => "思考|推敲|推理|琢磨|酝酿|盘算",
            WelcomeSubtitle => "AI Agent 终端 · 会话自动持久化",
            WelcomeHelp => "输入 /help 查看全部命令",
            WelcomeTips => "Tab 切换焦点 · Ctrl+\\ 侧边栏 · 鼠标滚轮滚动历史",
            WelcomeSessionsCount => "最近保存会话: {n} 个（侧边栏/会话 面板恢复）",
            WelcomeSessionsHint => "最近保存会话: 侧边栏/会话 面板查看",
            WelcomeCwd => "工作目录: {path}",
            HelpTitle => " 帮助 · 快捷键",
            HelpPager => " · {start}-{end}/{total} 行 · j/k 滚动 · Esc 关闭",
            HelpPagerShort => " · Esc 关闭",

            // 状态行
            QuitWarning => " │ ⚠ 再按 Esc 退出",
            HintConversation => "{nav} 导航 · {fold} 折叠 · {copy} 复制 · {page} 翻页 · {top} 首尾 · Esc 回输入",
            HintInput => "/ 命令 · Ctrl+P 权限模式 · Ctrl+T 鼠标 · Ctrl+U 清行 · Ctrl+W 删词 · Shift+Enter 换行 · Esc 取消/再按 Esc 退出",
            HintSidebar => "{nav} 选择会话 · Enter 恢复 · {tab} 切面板 · Esc 关闭",
            HintCompletion => "↑↓ 选择 · Enter 插入 · Esc 关闭补全",
            HintHelp => "j/k 或 ↑/↓ 滚动 · PageUp/Down 翻页 · Esc/q 关闭",
            HintConfirm => "y 确认 · n/Esc 取消",

            // 输入区
            InputRunning => "运行中 · Esc/Ctrl+C 取消",
            CommandHintTitleArg => "参数 · ↑↓ 选择 · Enter 执行 · Tab 补全 · Esc 关闭",
            CommandHintTitleCmd => "命令 · ↑↓ 选择 · Enter 执行 · Tab 补全 · Esc 关闭",
            CompletionTitle => "@ 文件补全",

            // 侧边栏
            PanelTitle => "面板",
            SidebarMcpHint => " 运行 /mcp 查看服务器状态",
            SidebarSkillsHint => " 运行 /skills 查看可用技能",
            NoHistorySessions => " （暂无历史会话）",
            NightGroupHeader => " ▾ {night} 夜 · {n}",
            SessionCurrentSuffix => " (当前)",
            MoreSessions => "  …还有 {n} 个（/sessions 查看全部）",
            CurrentSessionDivider => " ── 本次会话 ──",
            TurnEmpty => "#{id}（空）",
            NoToolCalls => " （暂无工具调用）",
            ToolActivityHeader => " 工具活动 · {n} 种工具",
            ToolCallCountLine => " {name_col} {n} 次  [{suffix}]",
            SessionCost => " 会话成本: ${cost}",
            CostUnavailable => " 成本不可用（无 router）",
            SidebarCostHint => " 运行 /cost 查看明细",
            ScorecardPanelTitle => " 测光·评分卡",
            ScorecardPending => " 测光待完成（运行 /scorecard）",

            // 审批浮层
            RiskReadonly => "只读",
            RiskNonReadonly => "非只读",
            RiskDanger => "危险",
            RiskLabel => " 风险：{label}",
            ApprovalHint => "y 允许 · n 拒绝 · Esc 拒绝",
            ApprovalTitle => "🔒 请求授权",

            // 权限模式与工作区信任
            PermModePlan => "plan",
            PermModeAcceptEdits => "accept_edits",
            PermModeAuto => "auto",
            PermModeLegacy => "default",
            PermModeIndicator => " │ 权限 {mode}",
            PermModeNotice => "权限模式: {mode}",
            PermModeGateUnavailable => "权限模式切换不可用（未注入 PermissionGate）",
            PermModeUsage => "用法: /mode [plan|accept_edits|auto|cycle]",
            CmdModeDesc => "切换权限模式预设（plan/accept_edits/auto）",
            ApprovalModeLine => " 当前权限模式：{mode}",
            TrustTitle => "🔒 信任该工作区？",
            TrustPromptBody => "该项目配置带有权限规则（{n} 条）。\n信任该工作区以让项目 allow 规则生效？\n工作区: {root}\n（不信任则项目 allow 规则降级为 ask）",
            TrustHint => "y 信任 · n 不信任 · Esc 不信任",
            TrustAccepted => "已信任该工作区 — 项目 allow 规则生效",
            TrustRejected => "未信任该工作区 — 项目 allow 规则降级为 ask",

            // 系统段
            PauseMaxSteps => "已达步骤上限（{n}），任务未完成",
            PauseMaxStepsPlain => "已达步骤上限，任务未完成",
            PauseBudget => "已达预算上限：{why}",
            PauseGeneric => "任务暂停：{reason}",
            QualityFinding => "🔎 质量检查 [{severity} {passed}] {rule}: {evidence}",
            PhaseTransition => "🔁 阶段迁移: {phase} ({outcome})",
            GateViolation => "🚧 门控违规: {gate} — {detail}",
            DriftFinding => "🧭 drift: {family} 连续失败 {n} 次 — {detail}",
            ApprovalRequestLine => "🔒 请求授权: {title}",
            ApprovalRequestLineDesc => "🔒 请求授权: {title} — {desc}",
            ResumeHint => "输入 /resume {id} 继续任务，或直接输入新指令",

            // 评分卡维度
            ScorecardDimGovernance => "治理",
            ScorecardDimVerification => "验证",
            ScorecardDimReflection => "反思",
            ScorecardDimReview => "审查",
            ScorecardDimProtocol => "协议",
            ScorecardDimComposite => "综合",

            // 键位诊断
            KeymapParseError => "parse_error: JSON 解析失败 — {err}（检查引号与逗号）",
            KeymapMissingBindings => "parse_error: 缺少 \"bindings\" 数组（示例见模块文档）",
            KeymapMissingContext => "invalid_context[#{gi}]: 缺少 \"context\" 字符串字段",
            KeymapUnknownContext => "invalid_context[#{gi}]: 未知 context '{ctx}'（可用: Input/Conversation/Sidebar/Completion）",
            KeymapMissingBindingsObj => "invalid_context[#{gi}]: 缺少 \"bindings\" 对象",
            KeymapUnparseableKey => "parse_error[{ctx}]: 无法解析键 '{key}'",
            KeymapActionOrNull => "invalid_action[{ctx}]: 键 '{key}' 的值必须是 action 名或 null",
            KeymapReserved => "reserved[{ctx}]: 键 '{key}' 不可重绑 — {reason}",
            KeymapParseKeyHelp => "parse_error[{ctx}]: 无法解析键 '{key}'（支持 ctrl/alt/shift/super + 键名或单字符）",
            KeymapUnknownAction => "invalid_action[{ctx}]: 未知 action '{action}'（可用 chat:submit / conv:scrollTop / modal:dismiss 等，见 /help）",
            KeymapDuplicate => "duplicate[{ctx}]: 键 '{key}' 重复绑定（后者生效）",
            ReservedCtrlC => "终端中断信号，被操作系统保留",
            ReservedCtrlD => "终端 EOF（空行时退出 shell）",
            ReservedCtrlM => "与 Enter 在终端中等价",
            ReservedCtrlZ => "SIGTSTP 挂起进程",
            ReservedCtrlBackslash => "SIGQUIT 终止信号",

            // 主题
            ThemeUnknownFallback => "未知主题 '{theme}'（deepseek|dark|light），已回退 deepseek",

            // ── 技术性/本就英文：中文模式回退英文 ──────────
            ModelCommandsHeader
            | DefaultLabel
            | ThinkingToggle
            | PointerSet
            | CtxUsage
            | UsageBrief
            | RunnerError => return None,
        })
    }
}

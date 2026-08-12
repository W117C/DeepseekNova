//! 上下文键盘系统（Claude Code 设计迁移）：action 注册表 + context 分层。
//!
//! 设计要点（对照 `逆向/concepts/claude-code-tui.md`）：
//! - **action**：语义化动作（`chat:submit`、`conv:scrollTop`），与具体键位解耦；
//! - **context**：焦点即上下文（Input/Conversation/Sidebar/Completion），
//!   同一键在不同 context 可绑定不同 action（如 Esc = 回输入 / 关面板 / 取消）；
//! - **动态键位显示**：UI 提示从绑定表查询而非硬编码——未来 keybindings.json
//!   用户改键后提示自动更新（Rw(action, context, fallback) 的轻量版）。
//!
//! 本模块是数据层：不持有 AppState 可变引用，只做「键 → action」查询与
//! 「action → 键位显示」查询。执行仍由 focus.rs 的 handler 完成。

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Ctrl+Shift 组合修饰符常量。
///
/// bitflags 的 `|` 运算符非 const，不能直接用于编译期绑定表，故用
/// 常量 `union` 组合（与 `Binding::new` 的 const 语义兼容）。
const CTRL_SHIFT: KeyModifiers = KeyModifiers::CONTROL.union(KeyModifiers::SHIFT);

/// 语义化动作。命名 `域:动作`，与 Claude Code 的 `chat:submit` 风格一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    // ── 全局 ─────────────────────────────────────────────
    /// 退出程序（Esc Esc 二次确认）。
    AppQuit,
    /// 取消当前生成。
    AppCancel,
    /// 切换侧边栏。
    AppToggleSidebar,
    /// 循环切换权限模式预设（plan → accept_edits → auto）。
    PermModeCycle,
    /// 打开命令面板（Ctrl+P；模糊搜索 + 最近使用排序，复用 CommandRegistry）。
    OpenCommandPalette,
    /// 切换 Tasks 面板（Ctrl+G；展示进行中的工具/子代理任务，grok 对齐）。
    ToggleTasks,
    /// 打开 /help 帮助浮层（F1）。
    AppHelp,
    /// 清屏重绘（Ctrl+L）。
    AppRedraw,
    /// 切换全屏模式：隐藏/恢复状态行与提示行（Ctrl+Shift+F）。
    ToggleFullscreen,
    /// 强制全量重绘（Ctrl+Shift+R）。
    Redraw,
    // ── 输入（Chat context）──────────────────────────────
    /// 提交输入。
    ChatSubmit,
    /// 换行（多行输入）。
    ChatNewline,
    /// 清空输入。
    ChatClearInput,
    /// 删除光标前一个词。
    ChatDeleteWord,
    /// 历史上翻/下翻。
    ChatHistoryPrev,
    ChatHistoryNext,
    /// 光标移动。
    ChatMoveLeft,
    ChatMoveRight,
    ChatMoveLineUp,
    ChatMoveLineDown,
    ChatHome,
    ChatEnd,
    /// Home 键：光标到当前行首（Ctrl+A 是缓冲区开头）。
    ChatHomeLine,
    /// End 键：光标到当前行尾（Ctrl+E 是缓冲区末尾）。
    ChatEndLine,
    /// 焦点循环：输入 → 对话导航。
    ChatFocusConversation,
    // ── 对话导航（Conversation context，vim 血统）────────
    /// 选中上/下一条消息。
    ConvSelectPrev,
    ConvSelectNext,
    /// 切换折叠。
    ConvToggleFold,
    /// 复制选中消息。
    ConvCopy,
    /// 滚动：整页/首尾/贴底。
    ConvScrollPageUp,
    ConvScrollPageDown,
    /// 半页滚动（vim `Ctrl+U` / `Ctrl+D`）。
    ConvScrollHalfUp,
    ConvScrollHalfDown,
    ConvScrollTop,
    ConvScrollBottom,
    /// 回输入焦点。
    ConvFocusInput,
    // ── 侧边栏（Sidebar context）─────────────────────────
    SidebarNextTab,
    SidebarPrevTab,
    SidebarClose,
    /// 侧边栏会话列表选择上/下（Sessions 面板）。
    SidebarSelectPrev,
    SidebarSelectNext,
    /// 侧边栏选中会话并恢复（Sessions 面板 Enter）。
    SidebarResumeSelected,
    /// 侧边栏加宽一列（`[`，Sidebar 焦点生效）。
    SidebarWiden,
    /// 侧边栏收窄一列（`]`，Sidebar 焦点生效）。
    SidebarNarrow,
    // ── 模态（Completion context）────────────────────────
    /// 关闭模态并回输入。
    ModalDismiss,
    /// 模态内选择上/下。
    ModalSelectPrev,
    ModalSelectNext,
    /// 确认选择。
    ModalAccept,
    /// 回退（删除查询字符 / 删除输入字符）。
    ModalBackspace,
    /// 输入字符（查询/候选过滤）。
    ModalTypeChar,
    /// 参数子输入提交。
    ModalArgSubmit,
    /// 取消参数子输入。
    ModalArgCancel,
    // ── grok 对齐：对话内搜索（Conversation context，Ctrl+F）──
    /// 打开对话内搜索条（Ctrl+F；空查询关闭）。
    ConvSearchOpen,
    /// 搜索：输入查询字符。
    ConvSearchType,
    /// 下一个命中（Enter/n）。
    ConvSearchNext,
    /// 上一个命中（Shift+Enter/N）。
    ConvSearchPrev,
    /// 关闭搜索（Esc）。
    ConvSearchClose,
    /// 回退查询字符（Backspace）。
    ConvSearchBackspace,
    // ── grok 对齐：历史搜索（Chat context，Ctrl+R）──────────
    /// 打开历史搜索（Ctrl+R；空查询回退输入框）。
    ChatHistorySearchOpen,
    /// 历史搜索：输入查询字符。
    ChatHistorySearchType,
    /// 上一个匹配（↑/Ctrl+R）。
    ChatHistorySearchPrev,
    /// 下一个匹配（↓）。
    ChatHistorySearchNext,
    /// 采纳选中项到输入框（Enter）。
    ChatHistorySearchAccept,
    /// 关闭历史搜索（Esc）。
    ChatHistorySearchClose,
    // ── grok 对齐：rewind（Esc Esc 空 prompt）──────────────
    /// 打开 rewind 浮层（空 prompt 二次 Esc）。
    RewindOpen,
    /// rewind 选择上/下（k/j、↑/↓）。
    RewindSelectPrev,
    RewindSelectNext,
    /// 确认回退到选中回合（Enter）。
    RewindAccept,
    /// 关闭 rewind（Esc）。
    RewindClose,
    // ── grok 对齐：vim 双键与 turn 导航 ─────────────────────
    /// vim 双键首键（`g`/`z` 开头；等待第二键）。
    ConvVimLead,
    /// vim 双键动作（`gg` 顶部、`H`/`M`/`L` 屏位、`zz`/`zt`/`zb` 视位）。
    ConvVimExec,
    /// turn 导航：上一回合（h）。
    ConvTurnPrev,
    /// turn 导航：下一回合（l）。
    ConvTurnNext,
    /// 单回合/全回合视图切换（`v`）。
    ConvToggleTurnView,
    /// response 锚点：上一个 response 顶部（K）。
    ConvAnchorPrev,
    /// response 锚点：下一个 response 顶部（J）。
    ConvAnchorNext,
    // ── grok 对齐：多行模式 ─────────────────────────────────
    /// 多行模式开关（提示行指示；与 ChatNewline 共存）。
    ChatToggleMultiline,
    // ── grok 对齐：统一模态（`?` 快捷键速查）─────────────────
    /// 打开快捷键速查表（`?`）。
    OpenShortcutsHelp,
    /// 关闭当前模态（Esc）。
    ModalClose,
}

impl Action {
    /// action 的稳定名字（keybindings.json 用，`域:动作` 风格）。
    pub fn name(self) -> &'static str {
        use Action::*;
        match self {
            AppQuit => "app:quit",
            AppCancel => "app:cancel",
            AppToggleSidebar => "app:toggleSidebar",
            PermModeCycle => "perm:cycleMode",
            OpenCommandPalette => "app:commandPalette",
            ToggleTasks => "app:toggleTasks",
            AppHelp => "app:help",
            AppRedraw => "app:redraw",
            ToggleFullscreen => "app:toggleFullscreen",
            Redraw => "app:forceRedraw",
            ChatSubmit => "chat:submit",
            ChatNewline => "chat:newline",
            ChatClearInput => "chat:clearInput",
            ChatDeleteWord => "chat:deleteWord",
            ChatHistoryPrev => "chat:historyPrev",
            ChatHistoryNext => "chat:historyNext",
            ChatMoveLeft => "chat:moveLeft",
            ChatMoveRight => "chat:moveRight",
            ChatMoveLineUp => "chat:moveLineUp",
            ChatMoveLineDown => "chat:moveLineDown",
            ChatHome => "chat:home",
            ChatEnd => "chat:end",
            ChatHomeLine => "chat:homeLine",
            ChatEndLine => "chat:endLine",
            ChatFocusConversation => "chat:focusConversation",
            ConvSelectPrev => "conv:selectPrev",
            ConvSelectNext => "conv:selectNext",
            ConvToggleFold => "conv:toggleFold",
            ConvCopy => "conv:copy",
            ConvScrollPageUp => "conv:scrollPageUp",
            ConvScrollPageDown => "conv:scrollPageDown",
            ConvScrollHalfUp => "conv:scrollHalfUp",
            ConvScrollHalfDown => "conv:scrollHalfDown",
            ConvScrollTop => "conv:scrollTop",
            ConvScrollBottom => "conv:scrollBottom",
            ConvFocusInput => "conv:focusInput",
            SidebarNextTab => "sidebar:nextTab",
            SidebarPrevTab => "sidebar:prevTab",
            SidebarClose => "sidebar:close",
            SidebarSelectPrev => "sidebar:selectPrev",
            SidebarSelectNext => "sidebar:selectNext",
            SidebarResumeSelected => "sidebar:resumeSelected",
            SidebarWiden => "sidebar:widen",
            SidebarNarrow => "sidebar:narrow",
            ModalDismiss => "modal:dismiss",
            ModalSelectPrev => "modal:selectPrev",
            ModalSelectNext => "modal:selectNext",
            ModalAccept => "modal:accept",
            ModalBackspace => "modal:backspace",
            ModalTypeChar => "modal:typeChar",
            ModalArgSubmit => "modal:argSubmit",
            ModalArgCancel => "modal:argCancel",
            ConvSearchOpen => "conv:searchOpen",
            ConvSearchType => "conv:searchType",
            ConvSearchNext => "conv:searchNext",
            ConvSearchPrev => "conv:searchPrev",
            ConvSearchClose => "conv:searchClose",
            ConvSearchBackspace => "conv:searchBackspace",
            ChatHistorySearchOpen => "chat:historySearchOpen",
            ChatHistorySearchType => "chat:historySearchType",
            ChatHistorySearchPrev => "chat:historySearchPrev",
            ChatHistorySearchNext => "chat:historySearchNext",
            ChatHistorySearchAccept => "chat:historySearchAccept",
            ChatHistorySearchClose => "chat:historySearchClose",
            RewindOpen => "conv:rewindOpen",
            RewindSelectPrev => "conv:rewindSelectPrev",
            RewindSelectNext => "conv:rewindSelectNext",
            RewindAccept => "conv:rewindAccept",
            RewindClose => "conv:rewindClose",
            ConvVimLead => "conv:vimLead",
            ConvVimExec => "conv:vimExec",
            ConvTurnPrev => "conv:turnPrev",
            ConvTurnNext => "conv:turnNext",
            ConvToggleTurnView => "conv:toggleTurnView",
            ConvAnchorPrev => "conv:anchorPrev",
            ConvAnchorNext => "conv:anchorNext",
            ChatToggleMultiline => "chat:toggleMultiline",
            OpenShortcutsHelp => "app:shortcutsHelp",
            ModalClose => "modal:close",
        }
    }

    /// 名字 → action（keybindings.json 解析）；未知返回 None。
    pub fn from_name(name: &str) -> Option<Action> {
        ALL_ACTIONS.iter().copied().find(|a| a.name() == name)
    }
}

/// 全部 action（校验与遍历用）。
pub const ALL_ACTIONS: &[Action] = &[
    Action::AppQuit,
    Action::AppCancel,
    Action::AppToggleSidebar,
    Action::PermModeCycle,
    Action::OpenCommandPalette,
    Action::ToggleTasks,
    Action::AppHelp,
    Action::AppRedraw,
    Action::ToggleFullscreen,
    Action::Redraw,
    Action::ChatSubmit,
    Action::ChatNewline,
    Action::ChatClearInput,
    Action::ChatDeleteWord,
    Action::ChatHistoryPrev,
    Action::ChatHistoryNext,
    Action::ChatMoveLeft,
    Action::ChatMoveRight,
    Action::ChatMoveLineUp,
    Action::ChatMoveLineDown,
    Action::ChatHome,
    Action::ChatEnd,
    Action::ChatHomeLine,
    Action::ChatEndLine,
    Action::ChatFocusConversation,
    Action::ConvSelectPrev,
    Action::ConvSelectNext,
    Action::ConvToggleFold,
    Action::ConvCopy,
    Action::ConvScrollPageUp,
    Action::ConvScrollPageDown,
    Action::ConvScrollHalfUp,
    Action::ConvScrollHalfDown,
    Action::ConvScrollTop,
    Action::ConvScrollBottom,
    Action::ConvFocusInput,
    Action::SidebarNextTab,
    Action::SidebarPrevTab,
    Action::SidebarClose,
    Action::SidebarSelectPrev,
    Action::SidebarSelectNext,
    Action::SidebarResumeSelected,
    Action::SidebarWiden,
    Action::SidebarNarrow,
    Action::ModalDismiss,
    Action::ModalSelectPrev,
    Action::ModalSelectNext,
    Action::ModalAccept,
    Action::ModalBackspace,
    Action::ModalTypeChar,
    Action::ModalArgSubmit,
    Action::ModalArgCancel,
    Action::ConvSearchOpen,
    Action::ConvSearchType,
    Action::ConvSearchNext,
    Action::ConvSearchPrev,
    Action::ConvSearchClose,
    Action::ConvSearchBackspace,
    Action::ChatHistorySearchOpen,
    Action::ChatHistorySearchType,
    Action::ChatHistorySearchPrev,
    Action::ChatHistorySearchNext,
    Action::ChatHistorySearchAccept,
    Action::ChatHistorySearchClose,
    Action::RewindOpen,
    Action::RewindSelectPrev,
    Action::RewindSelectNext,
    Action::RewindAccept,
    Action::RewindClose,
    Action::ConvVimLead,
    Action::ConvVimExec,
    Action::ConvTurnPrev,
    Action::ConvTurnNext,
    Action::ConvToggleTurnView,
    Action::ConvAnchorPrev,
    Action::ConvAnchorNext,
    Action::ChatToggleMultiline,
    Action::OpenShortcutsHelp,
    Action::ModalClose,
];

/// 动作所属的显示上下文（与 `Focus` 一一对应）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionContext {
    Input,
    Conversation,
    Sidebar,
    Completion,
}

impl ActionContext {
    pub fn name(self) -> &'static str {
        match self {
            ActionContext::Input => "Input",
            ActionContext::Conversation => "Conversation",
            ActionContext::Sidebar => "Sidebar",
            ActionContext::Completion => "Completion",
        }
    }

    pub fn from_name(name: &str) -> Option<ActionContext> {
        match name {
            "Input" => Some(ActionContext::Input),
            "Conversation" => Some(ActionContext::Conversation),
            "Sidebar" => Some(ActionContext::Sidebar),
            "Completion" => Some(ActionContext::Completion),
            _ => None,
        }
    }
}

/// 键位（含修饰符，可扩展为 chord 序列——本期仅单键，双键序列由
/// `AppState` 的 pending chord 状态机实现，见 focus.rs）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Binding {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl Binding {
    /// 常量构造（绑定表是编译期数据）。
    pub const fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }

    /// 解析 `ctrl+k` / `shift+enter` / `esc` / `g` 形式键串（keybindings.json）。
    /// 修饰符：ctrl / alt / shift / super（macOS cmd）；meta 归一化为 alt。
    pub fn parse(spec: &str) -> Option<Binding> {
        let spec = spec.trim();
        if spec.is_empty() {
            return None;
        }
        let mut modifiers = KeyModifiers::NONE;
        let mut key = spec;
        for part in spec.split('+') {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => modifiers |= KeyModifiers::CONTROL,
                "alt" | "meta" | "option" => modifiers |= KeyModifiers::ALT,
                "shift" => modifiers |= KeyModifiers::SHIFT,
                "super" | "cmd" | "command" | "win" => modifiers |= KeyModifiers::SUPER,
                _ => key = part,
            }
        }
        let code = match key.to_ascii_lowercase().as_str() {
            "esc" | "escape" => KeyCode::Esc,
            "enter" | "return" => KeyCode::Enter,
            "tab" => KeyCode::Tab,
            "backtab" => KeyCode::BackTab,
            "up" | "↑" => KeyCode::Up,
            "down" | "↓" => KeyCode::Down,
            "left" | "←" => KeyCode::Left,
            "right" | "→" => KeyCode::Right,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" => KeyCode::PageUp,
            "pagedown" => KeyCode::PageDown,
            "backspace" | "del" => KeyCode::Backspace,
            "delete" => KeyCode::Delete,
            "insert" => KeyCode::Insert,
            "space" => KeyCode::Char(' '),
            "\\" => KeyCode::Char('\\'),
            s if s.chars().count() == 1 => {
                // count==1 保证 next() 不为 None
                KeyCode::Char(s.chars().next().unwrap_or('\0'))
            }
            _ => return None,
        };
        Some(Binding::new(code, modifiers))
    }

    /// 从 crossterm 按键事件构造（运行时查询用）。
    pub fn from_key_event(key: &KeyEvent) -> Binding {
        Binding::new(key.code, key.modifiers)
    }

    /// 键位显示文本（平台无关；macOS 语义显示由上层决定）。
    pub fn display(&self) -> String {
        let mods = self.modifiers;
        let mut parts = Vec::new();
        if mods.contains(KeyModifiers::CONTROL) {
            parts.push("Ctrl");
        }
        if mods.contains(KeyModifiers::ALT) {
            parts.push("Alt");
        }
        if mods.contains(KeyModifiers::SHIFT) {
            parts.push("Shift");
        }
        let key = match self.code {
            KeyCode::Enter => "Enter".to_string(),
            KeyCode::Esc => "Esc".to_string(),
            KeyCode::BackTab => "Shift+Tab".to_string(),
            KeyCode::Tab => "Tab".to_string(),
            KeyCode::Up => "↑".to_string(),
            KeyCode::Down => "↓".to_string(),
            KeyCode::Left => "←".to_string(),
            KeyCode::Right => "→".to_string(),
            KeyCode::Home => "Home".to_string(),
            KeyCode::End => "End".to_string(),
            KeyCode::PageUp => "PageUp".to_string(),
            KeyCode::PageDown => "PageDown".to_string(),
            KeyCode::Backspace => "Backspace".to_string(),
            KeyCode::Char(c) => c.to_uppercase().collect::<String>(),
            _ => format!("{:?}", self.code),
        };
        if parts.is_empty() {
            key
        } else {
            parts.join("+") + "+" + &key
        }
    }
}

/// context 内按键 → action 的绑定表（编译期数据，参考 Claude Code
/// 附录 A 的默认键位表）。命令面板为纯 `/` 触发（无 Ctrl+K 绑定）。
pub const BINDINGS: &[(ActionContext, Binding, Action)] = &[
    // ── Global（任意焦点生效，优先于 context 绑定）──────
    (
        ActionContext::Input,
        Binding::new(KeyCode::Char('4'), KeyModifiers::CONTROL),
        Action::AppToggleSidebar,
    ),
    (
        ActionContext::Input,
        Binding::new(KeyCode::Char('\\'), KeyModifiers::CONTROL),
        Action::AppToggleSidebar,
    ),
    (
        ActionContext::Input,
        Binding::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        Action::OpenCommandPalette,
    ),
    (
        ActionContext::Input,
        Binding::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
        Action::ToggleTasks,
    ),
    (
        // grok 对齐：Ctrl+B = 后台任务面板（与 Ctrl+G 同入口；任意焦点生效）。
        ActionContext::Input,
        Binding::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
        Action::ToggleTasks,
    ),
    (
        ActionContext::Input,
        Binding::new(KeyCode::Char('p'), CTRL_SHIFT),
        Action::PermModeCycle,
    ),
    (
        ActionContext::Input,
        Binding::new(KeyCode::F(1), KeyModifiers::NONE),
        Action::AppHelp,
    ),
    (
        ActionContext::Input,
        Binding::new(KeyCode::Char('l'), KeyModifiers::CONTROL),
        Action::AppRedraw,
    ),
    (
        ActionContext::Input,
        Binding::new(KeyCode::Char('F'), CTRL_SHIFT),
        Action::ToggleFullscreen,
    ),
    (
        ActionContext::Input,
        Binding::new(KeyCode::Char('R'), CTRL_SHIFT),
        Action::Redraw,
    ),
    // ── Chat（输入聚焦）──────────────────────────────────
    (
        ActionContext::Input,
        Binding::new(KeyCode::Enter, KeyModifiers::NONE),
        Action::ChatSubmit,
    ),
    (
        ActionContext::Input,
        Binding::new(KeyCode::Enter, KeyModifiers::SHIFT),
        Action::ChatNewline,
    ),
    (
        ActionContext::Input,
        Binding::new(KeyCode::Enter, KeyModifiers::CONTROL),
        Action::ChatNewline,
    ),
    (
        ActionContext::Input,
        Binding::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
        Action::ChatHome,
    ),
    (
        ActionContext::Input,
        Binding::new(KeyCode::Char('e'), KeyModifiers::CONTROL),
        Action::ChatEnd,
    ),
    (
        ActionContext::Input,
        Binding::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
        Action::ChatClearInput,
    ),
    (
        ActionContext::Input,
        Binding::new(KeyCode::Char('w'), KeyModifiers::CONTROL),
        Action::ChatDeleteWord,
    ),
    (
        ActionContext::Input,
        Binding::new(KeyCode::Up, KeyModifiers::NONE),
        Action::ChatHistoryPrev,
    ),
    (
        ActionContext::Input,
        Binding::new(KeyCode::Down, KeyModifiers::NONE),
        Action::ChatHistoryNext,
    ),
    (
        ActionContext::Input,
        Binding::new(KeyCode::Left, KeyModifiers::NONE),
        Action::ChatMoveLeft,
    ),
    (
        ActionContext::Input,
        Binding::new(KeyCode::Right, KeyModifiers::NONE),
        Action::ChatMoveRight,
    ),
    (
        ActionContext::Input,
        Binding::new(KeyCode::Home, KeyModifiers::NONE),
        Action::ChatHomeLine,
    ),
    (
        ActionContext::Input,
        Binding::new(KeyCode::End, KeyModifiers::NONE),
        Action::ChatEndLine,
    ),
    (
        ActionContext::Input,
        Binding::new(KeyCode::Tab, KeyModifiers::NONE),
        Action::ChatFocusConversation,
    ),
    // ── Conversation（对话导航）──────────────────────────
    (
        ActionContext::Conversation,
        Binding::new(KeyCode::Char('j'), KeyModifiers::NONE),
        Action::ConvSelectNext,
    ),
    (
        ActionContext::Conversation,
        Binding::new(KeyCode::Down, KeyModifiers::NONE),
        Action::ConvSelectNext,
    ),
    (
        ActionContext::Conversation,
        Binding::new(KeyCode::Char('k'), KeyModifiers::NONE),
        Action::ConvSelectPrev,
    ),
    (
        ActionContext::Conversation,
        Binding::new(KeyCode::Up, KeyModifiers::NONE),
        Action::ConvSelectPrev,
    ),
    (
        ActionContext::Conversation,
        Binding::new(KeyCode::Enter, KeyModifiers::NONE),
        Action::ConvToggleFold,
    ),
    (
        ActionContext::Conversation,
        Binding::new(KeyCode::Char('y'), KeyModifiers::NONE),
        Action::ConvCopy,
    ),
    (
        ActionContext::Conversation,
        Binding::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
        Action::ConvScrollPageUp,
    ),
    (
        ActionContext::Conversation,
        Binding::new(KeyCode::Char('f'), KeyModifiers::CONTROL),
        Action::ConvSearchOpen,
    ),
    (
        ActionContext::Conversation,
        Binding::new(KeyCode::PageUp, KeyModifiers::NONE),
        Action::ConvScrollPageUp,
    ),
    (
        ActionContext::Conversation,
        Binding::new(KeyCode::PageDown, KeyModifiers::NONE),
        Action::ConvScrollPageDown,
    ),
    (
        ActionContext::Conversation,
        Binding::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
        Action::ConvScrollHalfUp,
    ),
    (
        ActionContext::Conversation,
        Binding::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
        Action::ConvScrollHalfDown,
    ),
    (
        ActionContext::Conversation,
        Binding::new(KeyCode::Char('g'), KeyModifiers::NONE),
        Action::ConvVimLead,
    ),
    (
        ActionContext::Conversation,
        Binding::new(KeyCode::Char('G'), KeyModifiers::NONE),
        Action::ConvScrollBottom,
    ),
    (
        ActionContext::Conversation,
        Binding::new(KeyCode::Char('h'), KeyModifiers::NONE),
        Action::ConvTurnPrev,
    ),
    (
        ActionContext::Conversation,
        Binding::new(KeyCode::Char('l'), KeyModifiers::NONE),
        Action::ConvTurnNext,
    ),
    (
        ActionContext::Conversation,
        Binding::new(KeyCode::Char('v'), KeyModifiers::NONE),
        Action::ConvToggleTurnView,
    ),
    (
        ActionContext::Conversation,
        Binding::new(KeyCode::Char('K'), KeyModifiers::NONE),
        Action::ConvAnchorPrev,
    ),
    (
        ActionContext::Conversation,
        Binding::new(KeyCode::Char('J'), KeyModifiers::NONE),
        Action::ConvAnchorNext,
    ),
    (
        ActionContext::Conversation,
        Binding::new(KeyCode::Char('?'), KeyModifiers::NONE),
        Action::OpenShortcutsHelp,
    ),
    (
        ActionContext::Conversation,
        Binding::new(KeyCode::Esc, KeyModifiers::NONE),
        Action::ConvFocusInput,
    ),
    (
        ActionContext::Conversation,
        Binding::new(KeyCode::Char('i'), KeyModifiers::NONE),
        Action::ConvFocusInput,
    ),
    // ── Chat（输入聚焦）：Ctrl+R 历史搜索 ────────────────
    (
        ActionContext::Input,
        Binding::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
        Action::ChatHistorySearchOpen,
    ),
    // ── Chat（输入聚焦）：多行模式切换 ────────────────────
    // grok 的 Ctrl+M 在终端层面等价 Enter，不可用；取 Alt+M 作为
    // 多行模式开关（Enter 提交 / Alt+M 切换换行语义）。
    (
        ActionContext::Input,
        Binding::new(KeyCode::Char('m'), KeyModifiers::ALT),
        Action::ChatToggleMultiline,
    ),
    // ── Sidebar ──────────────────────────────────────────
    (
        ActionContext::Sidebar,
        Binding::new(KeyCode::Tab, KeyModifiers::NONE),
        Action::SidebarNextTab,
    ),
    (
        ActionContext::Sidebar,
        Binding::new(KeyCode::BackTab, KeyModifiers::NONE),
        Action::SidebarPrevTab,
    ),
    (
        ActionContext::Sidebar,
        Binding::new(KeyCode::Esc, KeyModifiers::NONE),
        Action::SidebarClose,
    ),
    (
        ActionContext::Sidebar,
        Binding::new(KeyCode::Backspace, KeyModifiers::NONE),
        Action::SidebarClose,
    ),
    (
        ActionContext::Sidebar,
        Binding::new(KeyCode::Up, KeyModifiers::NONE),
        Action::SidebarSelectPrev,
    ),
    (
        ActionContext::Sidebar,
        Binding::new(KeyCode::Char('k'), KeyModifiers::NONE),
        Action::SidebarSelectPrev,
    ),
    (
        ActionContext::Sidebar,
        Binding::new(KeyCode::Down, KeyModifiers::NONE),
        Action::SidebarSelectNext,
    ),
    (
        ActionContext::Sidebar,
        Binding::new(KeyCode::Char('j'), KeyModifiers::NONE),
        Action::SidebarSelectNext,
    ),
    (
        ActionContext::Sidebar,
        Binding::new(KeyCode::Enter, KeyModifiers::NONE),
        Action::SidebarResumeSelected,
    ),
    (
        ActionContext::Sidebar,
        Binding::new(KeyCode::Char('['), KeyModifiers::NONE),
        Action::SidebarWiden,
    ),
    (
        ActionContext::Sidebar,
        Binding::new(KeyCode::Char(']'), KeyModifiers::NONE),
        Action::SidebarNarrow,
    ),
    // ── Completion ───────────────────────────────────────
    (
        ActionContext::Completion,
        Binding::new(KeyCode::Esc, KeyModifiers::NONE),
        Action::ModalDismiss,
    ),
    (
        ActionContext::Completion,
        Binding::new(KeyCode::Up, KeyModifiers::NONE),
        Action::ModalSelectPrev,
    ),
    (
        ActionContext::Completion,
        Binding::new(KeyCode::Down, KeyModifiers::NONE),
        Action::ModalSelectNext,
    ),
    (
        ActionContext::Completion,
        Binding::new(KeyCode::Enter, KeyModifiers::NONE),
        Action::ModalAccept,
    ),
];

/// 按键在指定 context 下命中的 action；未绑定返回 None。
/// 查找顺序：精确匹配 → 无修饰符匹配（终端有时吞掉修饰符）。
pub fn lookup(context: ActionContext, key: &KeyEvent) -> Option<Action> {
    BINDINGS
        .iter()
        .find(|(ctx, binding, _)| {
            *ctx == context && binding.code == key.code && binding.modifiers == key.modifiers
        })
        .map(|(_, _, action)| *action)
}

/// 用户覆盖层感知的按键解析：先查 keybindings.json 覆盖（含解绑），
/// 未覆盖回落默认绑定表。
pub fn resolve_action(
    keymap: &crate::app::keybindings::Keymap,
    context: ActionContext,
    key: &KeyEvent,
) -> Option<Action> {
    let binding = Binding::from_key_event(key);
    match keymap.lookup(context, binding) {
        Some(Some(action)) => Some(action),
        Some(None) => None, // 用户解绑
        None => lookup(context, key),
    }
}

/// 动作在指定 context 下的键位显示文本（动态提示查询入口）。
/// 未绑定返回 None——调用方给出 fallback 文本（与 Claude Code
/// `Rw(action, context, fallbackChord)` 同构）。
pub fn chord_for(context: ActionContext, action: Action) -> Option<String> {
    BINDINGS
        .iter()
        .find(|(ctx, _, act)| *ctx == context && *act == action)
        .map(|(_, binding, _)| binding.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: crossterm::event::KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    #[test]
    fn lookup_resolves_chat_submit() {
        assert_eq!(
            lookup(
                ActionContext::Input,
                &key(KeyCode::Enter, KeyModifiers::NONE)
            ),
            Some(Action::ChatSubmit)
        );
        assert_eq!(
            lookup(
                ActionContext::Input,
                &key(KeyCode::Enter, KeyModifiers::SHIFT)
            ),
            Some(Action::ChatNewline),
            "shift+enter = 换行"
        );
    }

    #[test]
    fn lookup_is_context_scoped() {
        // 同一按键在不同 context 命中不同 action。
        assert_eq!(
            lookup(
                ActionContext::Conversation,
                &key(KeyCode::Enter, KeyModifiers::NONE)
            ),
            Some(Action::ConvToggleFold)
        );
        assert_eq!(
            lookup(ActionContext::Input, &key(KeyCode::Esc, KeyModifiers::NONE)),
            None,
            "Esc 在 Input 未绑定（退出走 confirm_quit 逻辑）"
        );
    }

    #[test]
    fn lookup_missing_returns_none() {
        assert_eq!(
            lookup(
                ActionContext::Sidebar,
                &key(KeyCode::Char('z'), KeyModifiers::NONE)
            ),
            None
        );
    }

    #[test]
    fn plain_question_mark_not_bound_in_input() {
        // grok 对齐修复：裸 `?` 在 Input 上下文不再绑定命令面板，否则输入框
        // 无法键入 `?`（shell 命令/URL/疑问句），每次按键都弹面板（审查#1）。
        assert_eq!(
            lookup(
                ActionContext::Input,
                &key(KeyCode::Char('?'), KeyModifiers::NONE)
            ),
            None,
            "裸 `?` 在 Input 上下文不得命中任何 action"
        );
        // Conversation 上下文的 `?` → OpenShortcutsHelp 保留。
        assert_eq!(
            lookup(
                ActionContext::Conversation,
                &key(KeyCode::Char('?'), KeyModifiers::NONE)
            ),
            Some(Action::OpenShortcutsHelp)
        );
    }

    #[test]
    fn chord_display_shows_platform_neutral_text() {
        assert_eq!(
            chord_for(ActionContext::Conversation, Action::ConvScrollPageUp),
            Some("Ctrl+B".to_string())
        );
        assert_eq!(
            chord_for(ActionContext::Input, Action::ChatSubmit),
            Some("Enter".to_string())
        );
        assert_eq!(
            chord_for(ActionContext::Sidebar, Action::SidebarClose),
            Some("Esc".to_string())
        );
        assert_eq!(chord_for(ActionContext::Input, Action::AppQuit), None);
    }

    #[test]
    fn binding_display_formats_modifiers() {
        let b = Binding::new(KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(b.display(), "Ctrl+K");
        let b = Binding::new(KeyCode::Char('4'), KeyModifiers::CONTROL);
        assert_eq!(b.display(), "Ctrl+4");
        let b = Binding::new(KeyCode::BackTab, KeyModifiers::NONE);
        assert_eq!(b.display(), "Shift+Tab");
        let b = Binding::new(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(b.display(), "↑");
    }

    #[test]
    fn new_bindings_resolve_sidebar_width_and_fullscreen() {
        // 侧边栏 `[`/`]` 只在 Sidebar 焦点生效，不劫持输入区键入。
        assert_eq!(
            lookup(
                ActionContext::Sidebar,
                &key(KeyCode::Char('['), KeyModifiers::NONE)
            ),
            Some(Action::SidebarWiden)
        );
        assert_eq!(
            lookup(
                ActionContext::Sidebar,
                &key(KeyCode::Char(']'), KeyModifiers::NONE)
            ),
            Some(Action::SidebarNarrow)
        );
        assert_eq!(
            lookup(
                ActionContext::Input,
                &key(KeyCode::Char('['), KeyModifiers::NONE)
            ),
            None,
            "输入区 `[` 仍是自由插入字符"
        );
        // 全局：Ctrl+Shift+F 全屏、Ctrl+Shift+R 重绘（经 Input 上下文注册）。
        assert_eq!(
            lookup(
                ActionContext::Input,
                &key(
                    KeyCode::Char('F'),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT
                )
            ),
            Some(Action::ToggleFullscreen)
        );
        assert_eq!(
            lookup(
                ActionContext::Input,
                &key(
                    KeyCode::Char('R'),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT
                )
            ),
            Some(Action::Redraw)
        );
    }

    #[test]
    fn new_action_names_round_trip() {
        // keybindings.json 可经 name/from_name 重绑新增 action。
        assert_eq!(
            Action::from_name("sidebar:widen"),
            Some(Action::SidebarWiden)
        );
        assert_eq!(
            Action::from_name("sidebar:narrow"),
            Some(Action::SidebarNarrow)
        );
        assert_eq!(
            Action::from_name("app:toggleFullscreen"),
            Some(Action::ToggleFullscreen)
        );
        assert_eq!(Action::from_name("app:forceRedraw"), Some(Action::Redraw));
        assert_eq!(Action::SidebarWiden.name(), "sidebar:widen");
        assert_eq!(Action::ToggleFullscreen.name(), "app:toggleFullscreen");
    }
}

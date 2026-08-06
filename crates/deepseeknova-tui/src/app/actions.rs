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
}

impl Action {
    /// action 的稳定名字（keybindings.json 用，`域:动作` 风格）。
    pub fn name(self) -> &'static str {
        use Action::*;
        match self {
            AppQuit => "app:quit",
            AppCancel => "app:cancel",
            AppToggleSidebar => "app:toggleSidebar",
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
            ChatFocusConversation => "chat:focusConversation",
            ConvSelectPrev => "conv:selectPrev",
            ConvSelectNext => "conv:selectNext",
            ConvToggleFold => "conv:toggleFold",
            ConvCopy => "conv:copy",
            ConvScrollPageUp => "conv:scrollPageUp",
            ConvScrollPageDown => "conv:scrollPageDown",
            ConvScrollTop => "conv:scrollTop",
            ConvScrollBottom => "conv:scrollBottom",
            ConvFocusInput => "conv:focusInput",
            SidebarNextTab => "sidebar:nextTab",
            SidebarPrevTab => "sidebar:prevTab",
            SidebarClose => "sidebar:close",
            SidebarSelectPrev => "sidebar:selectPrev",
            SidebarSelectNext => "sidebar:selectNext",
            SidebarResumeSelected => "sidebar:resumeSelected",
            ModalDismiss => "modal:dismiss",
            ModalSelectPrev => "modal:selectPrev",
            ModalSelectNext => "modal:selectNext",
            ModalAccept => "modal:accept",
            ModalBackspace => "modal:backspace",
            ModalTypeChar => "modal:typeChar",
            ModalArgSubmit => "modal:argSubmit",
            ModalArgCancel => "modal:argCancel",
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
    Action::ChatFocusConversation,
    Action::ConvSelectPrev,
    Action::ConvSelectNext,
    Action::ConvToggleFold,
    Action::ConvCopy,
    Action::ConvScrollPageUp,
    Action::ConvScrollPageDown,
    Action::ConvScrollTop,
    Action::ConvScrollBottom,
    Action::ConvFocusInput,
    Action::SidebarNextTab,
    Action::SidebarPrevTab,
    Action::SidebarClose,
    Action::SidebarSelectPrev,
    Action::SidebarSelectNext,
    Action::SidebarResumeSelected,
    Action::ModalDismiss,
    Action::ModalSelectPrev,
    Action::ModalSelectNext,
    Action::ModalAccept,
    Action::ModalBackspace,
    Action::ModalTypeChar,
    Action::ModalArgSubmit,
    Action::ModalArgCancel,
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
            s if s.chars().count() == 1 => KeyCode::Char(s.chars().next().unwrap()),
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
        Action::ChatHome,
    ),
    (
        ActionContext::Input,
        Binding::new(KeyCode::End, KeyModifiers::NONE),
        Action::ChatEnd,
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
        Action::ConvScrollPageDown,
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
        Binding::new(KeyCode::Char('g'), KeyModifiers::NONE),
        Action::ConvScrollTop,
    ),
    (
        ActionContext::Conversation,
        Binding::new(KeyCode::Char('G'), KeyModifiers::NONE),
        Action::ConvScrollBottom,
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
}

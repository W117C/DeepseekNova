//! 焦点状态机：按键按当前焦点分发表路由。
//!
//! 焦点归属：`Input`（编辑器）与 `Conversation`（消息导航）是主焦点；
//! `Sidebar`/`Palette`/`Completion` 为模态焦点；`Confirm` 保留给破坏性操作。

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::state::{AppState, KeyAction};

/// 当前焦点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    /// 对话消息导航（j/k、Enter 折叠、y 复制）。
    Conversation,
    /// 输入编辑器（默认）。
    #[default]
    Input,
    /// 侧边栏（Tab/Ctrl+1..5 切面板、j/k 列表）。
    Sidebar,
    /// Ctrl+K 命令面板。
    Palette,
    /// @ 文件补全浮层。
    Completion,
    /// 破坏性操作确认（spec 预留位；当前无挂起操作，保持可达性以避
    /// 免后续接线时破坏焦点分发表）。
    #[allow(dead_code)]
    Confirm,
}

/// 侧边栏面板。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SidebarTab {
    #[default]
    Sessions,
    Tools,
    Mcp,
    Cost,
    Skills,
}

impl SidebarTab {
    pub const ALL: [SidebarTab; 5] = [
        SidebarTab::Sessions,
        SidebarTab::Tools,
        SidebarTab::Mcp,
        SidebarTab::Cost,
        SidebarTab::Skills,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SidebarTab::Sessions => "会话",
            SidebarTab::Tools => "工具活动",
            SidebarTab::Mcp => "MCP",
            SidebarTab::Cost => "成本",
            SidebarTab::Skills => "技能",
        }
    }

    pub fn prev(self) -> Self {
        let idx = Self::ALL.iter().position(|t| *t == self).unwrap_or(0);
        Self::ALL[(idx + Self::ALL.len() - 1) % Self::ALL.len()]
    }

    pub fn next(self) -> Self {
        let idx = Self::ALL.iter().position(|t| *t == self).unwrap_or(0);
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }
}

/// Ctrl+K 命令面板状态。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PaletteState {
    pub query: String,
    pub selected: usize,
    /// 已选中命令后进入的参数子输入（有参数的命令）。
    pub arg_input: Option<String>,
}

/// @ 补全浮层状态。
#[derive(Debug, Clone, PartialEq)]
pub struct CompletionState {
    /// 补全起始位置（插入点字节下标）。
    pub start: usize,
    /// 光标处插入点（字节下标）。
    pub end: usize,
    /// 当前候选列表。
    pub candidates: Vec<String>,
    pub selected: usize,
}

impl AppState {
    /// 按键分派：按焦点路由。返回是否退出/提交/取消。
    pub fn handle_key(&mut self, key: &KeyEvent) -> KeyAction {
        match self.focus {
            Focus::Conversation => self.handle_conversation_key(key),
            Focus::Input => {
                let action = self.handle_editor_key(key);
                // 编辑后尝试打开 @ 补全浮层（仅在有候选文件源时生效）。
                self.maybe_open_completion();
                action
            }
            Focus::Sidebar => self.handle_sidebar_key(key),
            Focus::Palette => crate::commands::palette::handle_key(self, key),
            Focus::Completion => crate::input::at_complete::handle_key(self, key),
            Focus::Confirm => self.handle_confirm_key(key),
        }
    }

    /// 光标前是 `@` 词且候选非空时打开补全浮层（文件清单由 CLI 注入，缺省不触发）。
    fn maybe_open_completion(&mut self) {
        if self.completion.is_some() || self.at_files.is_empty() {
            return;
        }
        let Some((start, end, prefix)) =
            crate::input::at_complete::AtCompleter::word_at(&self.input.text, self.input.cursor)
        else {
            return;
        };
        let candidates =
            crate::input::at_complete::AtCompleter::new(self.at_files.clone()).candidates(&prefix);
        if candidates.is_empty() {
            return;
        }
        self.completion = Some(CompletionState {
            start,
            end,
            candidates,
            selected: 0,
        });
        self.focus = Focus::Completion;
    }

    /// Conversation 焦点：消息导航 / 折叠 / 复制。
    fn handle_conversation_key(&mut self, key: &KeyEvent) -> KeyAction {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.select_next();
                KeyAction::None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.select_prev();
                KeyAction::None
            }
            KeyCode::Enter => {
                if let Some(seg) = self.selected {
                    self.toggle_fold(seg);
                }
                KeyAction::None
            }
            KeyCode::Char('y') => {
                self.copy_selected();
                KeyAction::None
            }
            KeyCode::Tab => {
                // 焦点循环：Conversation → Sidebar（可见时）→ Input。
                self.focus = if self.sidebar_visible {
                    Focus::Sidebar
                } else {
                    Focus::Input
                };
                KeyAction::None
            }
            KeyCode::Esc | KeyCode::Char('i') => {
                self.focus = Focus::Input;
                KeyAction::None
            }
            KeyCode::Char('q') => KeyAction::Quit,
            _ => KeyAction::None,
        }
    }

    /// Sidebar 焦点：面板切换 / 列表导航 / 关闭。
    fn handle_sidebar_key(&mut self, key: &KeyEvent) -> KeyAction {
        match key.code {
            KeyCode::Tab => {
                self.sidebar_tab = self.sidebar_tab.next();
                KeyAction::None
            }
            KeyCode::BackTab => {
                self.sidebar_tab = self.sidebar_tab.prev();
                KeyAction::None
            }
            KeyCode::Char('1') => self.set_sidebar_tab(SidebarTab::Sessions),
            KeyCode::Char('2') => self.set_sidebar_tab(SidebarTab::Tools),
            KeyCode::Char('3') => self.set_sidebar_tab(SidebarTab::Mcp),
            KeyCode::Char('4') => self.set_sidebar_tab(SidebarTab::Cost),
            KeyCode::Char('5') => self.set_sidebar_tab(SidebarTab::Skills),
            KeyCode::Esc | KeyCode::Backspace => {
                self.sidebar_visible = false;
                self.focus = Focus::Input;
                KeyAction::None
            }
            KeyCode::Char('q') => KeyAction::Quit,
            _ => KeyAction::None,
        }
    }

    fn set_sidebar_tab(&mut self, tab: SidebarTab) -> KeyAction {
        self.sidebar_tab = tab;
        self.focus = Focus::Input;
        KeyAction::None
    }

    /// Confirm 焦点：y/n 确认（当前无挂起的破坏性操作，直接退出模态）。
    fn handle_confirm_key(&mut self, key: &KeyEvent) -> KeyAction {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('n') | KeyCode::Esc => {
                self.focus = Focus::Input;
                KeyAction::None
            }
            _ => KeyAction::None,
        }
    }

    /// 全局模态热键：Ctrl+K 命令面板、Ctrl+\ 侧边栏开合（任意焦点生效）。
    pub fn handle_modal_shortcuts(&mut self, key: &KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('k') => {
                    self.palette = Some(PaletteState::default());
                    self.focus = Focus::Palette;
                    return true;
                }
                // Ctrl+\ (0x1C)：crossterm unix 下解析为 Char('4')+CONTROL；
                // 兼容显式 `\` 形态（部分终端 modifyOtherKeys）。
                KeyCode::Char('4') | KeyCode::Char('\\') => {
                    self.sidebar_visible = !self.sidebar_visible;
                    return true;
                }
                _ => {}
            }
        }
        false
    }
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
    fn sidebar_tab_cycles() {
        assert_eq!(SidebarTab::Mcp.next(), SidebarTab::Cost);
        assert_eq!(SidebarTab::Sessions.prev(), SidebarTab::Skills);
        assert_eq!(SidebarTab::Skills.next(), SidebarTab::Sessions);
        assert_eq!(SidebarTab::ALL.len(), 5);
    }

    #[test]
    fn conversation_keys_move_select_and_toggle_fold() {
        let mut app = AppState::default();
        // 构造两个段：借助 conversation API
        let id = app.conversation.begin_turn("q".into());
        app.apply_run_event(deepseeknova_core::runner::RunEvent::ReasoningDelta {
            text: "r".into(),
            signature: None,
        });
        app.apply_run_event(deepseeknova_core::runner::RunEvent::TextDelta("a".into()));
        app.apply_run_event(deepseeknova_core::runner::RunEvent::Done(
            crate::model::conversation::done_output(""),
        ));
        app.focus = Focus::Conversation;
        assert_eq!(app.selected, None);

        app.handle_key(&key(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(app.selected, Some((id, 0)), "选中第一段");
        app.handle_key(&key(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(app.selected, Some((id, 1)));
        app.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.fold.contains_key(&(id, 1)), "Enter 切换折叠态");
        app.handle_key(&key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.focus, Focus::Input);
    }

    #[test]
    fn sidebar_keys_switch_tab_and_close() {
        let mut app = AppState {
            focus: Focus::Sidebar,
            ..Default::default()
        };
        app.handle_key(&key(KeyCode::Char('3'), KeyModifiers::NONE));
        assert_eq!(app.sidebar_tab, SidebarTab::Mcp);
        assert_eq!(app.focus, Focus::Input, "选中面板后回输入");
        app.focus = Focus::Sidebar;
        app.handle_key(&key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.sidebar_tab, SidebarTab::Cost, "Mcp.next() = Cost");
        app.focus = Focus::Sidebar;
        app.handle_key(&key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.sidebar_visible);
        assert_eq!(app.focus, Focus::Input);
    }

    #[test]
    fn modal_shortcuts_open_palette_and_toggle_sidebar() {
        let mut app = AppState::default();
        assert!(app.handle_modal_shortcuts(&key(KeyCode::Char('k'), KeyModifiers::CONTROL)));
        assert_eq!(app.focus, Focus::Palette);
        assert!(!app.sidebar_visible);
        // Ctrl+\ 在 crossterm unix 下解析为 Char('4')+CONTROL。
        assert!(app.handle_modal_shortcuts(&key(KeyCode::Char('4'), KeyModifiers::CONTROL)));
        assert!(app.sidebar_visible);
        assert!(app.handle_modal_shortcuts(&key(KeyCode::Char('4'), KeyModifiers::CONTROL)));
        assert!(!app.sidebar_visible);
        // 兼容显式 `\` 形态。
        assert!(app.handle_modal_shortcuts(&key(KeyCode::Char('\\'), KeyModifiers::CONTROL)));
        assert!(app.sidebar_visible);
    }

    #[test]
    fn confirm_exits_modal() {
        let mut app = AppState {
            focus: Focus::Confirm,
            ..Default::default()
        };
        app.handle_key(&key(KeyCode::Char('y'), KeyModifiers::NONE));
        assert_eq!(app.focus, Focus::Input);
    }
}

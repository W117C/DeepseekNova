//! 焦点状态机：按键按当前焦点分发表路由。
//!
//! 焦点归属：`Input`（编辑器）与 `Conversation`（消息导航）是主焦点；
//! `Sidebar`/`Completion` 为模态焦点；`Confirm` 保留给破坏性操作。
//! 斜杠命令候选（`command_hint`）是非模态的——焦点保持 Input，
//! 候选就地展开在输入区上方（Claude Code 风格）。

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::actions::{Action, ActionContext};
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
        // 审批浮层优先：挂起请求时 y 允许 / n|Esc 拒绝，其余键忽略
        //（Claude Code Confirmation context 语义）。
        if self.pending_approval.is_some() {
            return match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    if let Some(req) = self.pending_approval.take() {
                        let _ = req.reply.send(true);
                    }
                    KeyAction::None
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    if let Some(req) = self.pending_approval.take() {
                        let _ = req.reply.send(false);
                    }
                    KeyAction::None
                }
                _ => KeyAction::None,
            };
        }
        // 任意非 Esc 键复位退出确认。
        if key.code != KeyCode::Esc {
            self.disarm_quit();
        }
        // ctrl+x 双键序列（低频高危动作专用，Claude Code 同款设计）：
        // 首键 ctrl+x → 3 秒窗等待第二键；第二键 ctrl+e → 外部编辑器。
        if self.focus == Focus::Input {
            if let Some(started) = self.chord_pending {
                self.chord_pending = None;
                if started.elapsed() < std::time::Duration::from_secs(3)
                    && key.code == KeyCode::Char('e')
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    return KeyAction::ExternalEditor;
                }
            } else if key.code == KeyCode::Char('x')
                && key.modifiers.contains(KeyModifiers::CONTROL)
            {
                self.chord_pending = Some(std::time::Instant::now());
                return KeyAction::None;
            }
        }
        // 斜杠命令候选优先于焦点分派：浮层打开时 ↑↓/Enter/Tab/Esc 由它消费，
        // 即使焦点意外不在 Input（如刚切过侧边栏），按键也不会“无响应”。
        if let Some(action) = self.handle_command_hint_key(key) {
            return action;
        }
        match self.focus {
            Focus::Conversation => self.handle_conversation_key(key),
            Focus::Input => {
                let action = self.handle_editor_key(key);
                // @ 补全浮层在按键后检查打开；斜杠候选的刷新只在
                // handle_editor_key 内部文本变更路径上发生——此处不再
                // 无条件重建，否则选择/关闭后候选状态会被重置（selected
                // 恒回 0、Esc 关不掉）。
                self.maybe_open_completion();
                action
            }
            Focus::Sidebar => self.handle_sidebar_key(key),
            Focus::Completion => crate::input::at_complete::handle_key(self, key),
            Focus::Confirm => self.handle_confirm_key(key),
        }
    }

    /// 光标前是 `@` 词且候选非空时打开补全浮层（文件清单由 CLI 注入，缺省不触发）。
    /// 斜杠候选打开期间不抢焦点（选择键 j/k 等不得误触发 @ 补全）。
    fn maybe_open_completion(&mut self) {
        if self.command_hint.is_some() || self.completion.is_some() || self.at_files.is_empty() {
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

    /// Conversation 焦点：action 注册表驱动（消息导航 / 折叠 / 复制 /
    /// vim 滚动）。未绑定键忽略。
    fn handle_conversation_key(&mut self, key: &KeyEvent) -> KeyAction {
        let Some(action) =
            crate::app::actions::resolve_action(&self.keymap, ActionContext::Conversation, key)
        else {
            // 数字/字母快捷键（Tab 循环焦点、q 退出）为会话级，非注册表。
            return match key.code {
                KeyCode::Tab => {
                    self.focus = if self.sidebar_visible {
                        Focus::Sidebar
                    } else {
                        Focus::Input
                    };
                    KeyAction::None
                }
                KeyCode::Char('q') => self.confirm_quit(),
                _ => KeyAction::None,
            };
        };
        match action {
            Action::ConvSelectNext => {
                self.select_next();
                KeyAction::None
            }
            Action::ConvSelectPrev => {
                self.select_prev();
                KeyAction::None
            }
            Action::ConvToggleFold => {
                if let Some(seg) = self.selected {
                    self.toggle_fold(seg);
                }
                KeyAction::None
            }
            Action::ConvCopy => {
                self.copy_selected();
                KeyAction::None
            }
            Action::ConvScrollPageUp => {
                // 向上翻页 = 看更早记录（offset 减小）。
                self.scroll_offset = self.scroll_offset.saturating_sub(20);
                self.auto_scroll = false;
                KeyAction::None
            }
            Action::ConvScrollPageDown => {
                self.scroll_offset = self.scroll_offset.saturating_add(20);
                KeyAction::None
            }
            Action::ConvScrollTop => {
                self.scroll_offset = 0;
                self.auto_scroll = false;
                KeyAction::None
            }
            Action::ConvScrollBottom => {
                self.auto_scroll = true;
                KeyAction::None
            }
            Action::ConvFocusInput => {
                self.focus = Focus::Input;
                KeyAction::None
            }
            _ => KeyAction::None,
        }
    }

    /// Sidebar 焦点：action 注册表驱动（面板切换 / 关闭）。
    fn handle_sidebar_key(&mut self, key: &KeyEvent) -> KeyAction {
        let Some(action) =
            crate::app::actions::resolve_action(&self.keymap, ActionContext::Sidebar, key)
        else {
            // 数字 1..5 直接切面板（会话级快捷键）。
            return match key.code {
                KeyCode::Char('1') => self.set_sidebar_tab(SidebarTab::Sessions),
                KeyCode::Char('2') => self.set_sidebar_tab(SidebarTab::Tools),
                KeyCode::Char('3') => self.set_sidebar_tab(SidebarTab::Mcp),
                KeyCode::Char('4') => self.set_sidebar_tab(SidebarTab::Cost),
                KeyCode::Char('5') => self.set_sidebar_tab(SidebarTab::Skills),
                KeyCode::Char('q') => self.confirm_quit(),
                _ => KeyAction::None,
            };
        };
        match action {
            Action::SidebarNextTab => {
                self.sidebar_tab = self.sidebar_tab.next();
                KeyAction::None
            }
            Action::SidebarPrevTab => {
                self.sidebar_tab = self.sidebar_tab.prev();
                KeyAction::None
            }
            Action::SidebarClose => {
                self.sidebar_visible = false;
                self.focus = Focus::Input;
                KeyAction::None
            }
            Action::SidebarSelectPrev => {
                if self.sidebar_tab == SidebarTab::Sessions && !self.saved_sessions.is_empty() {
                    self.saved_session_selected =
                        (self.saved_session_selected + self.saved_sessions.len() - 1)
                            % self.saved_sessions.len();
                }
                KeyAction::None
            }
            Action::SidebarSelectNext => {
                if self.sidebar_tab == SidebarTab::Sessions && !self.saved_sessions.is_empty() {
                    self.saved_session_selected =
                        (self.saved_session_selected + 1) % self.saved_sessions.len();
                }
                KeyAction::None
            }
            Action::SidebarResumeSelected => {
                if self.sidebar_tab == SidebarTab::Sessions {
                    if let Some(id) = self
                        .saved_sessions
                        .get(self.saved_session_selected)
                        .cloned()
                    {
                        // 事件循环用真实 caps 消费 pending_command，与 Ctrl+K 同路。
                        self.pending_command = Some(("resume".to_string(), id));
                        self.focus = Focus::Input;
                    }
                }
                KeyAction::None
            }
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

    /// 全局模态热键：Ctrl+\ 侧边栏开合（任意焦点生效）。
    /// 命令面板为纯 `/` 触发（就地候选，非模态），无 Ctrl+K。
    pub fn handle_modal_shortcuts(&mut self, key: &KeyEvent) -> bool {
        let Some(action) =
            crate::app::actions::resolve_action(&self.keymap, ActionContext::Input, key)
        else {
            return false;
        };
        match action {
            Action::AppToggleSidebar => {
                self.sidebar_visible = !self.sidebar_visible;
                true
            }
            _ => false,
        }
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
    fn sidebar_selects_and_resumes_saved_session() {
        let mut app = AppState {
            focus: Focus::Sidebar,
            sidebar_tab: SidebarTab::Sessions,
            saved_sessions: vec!["chat-a".to_string(), "chat-b".to_string()],
            ..Default::default()
        };
        app.handle_key(&key(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.saved_session_selected, 1);
        app.handle_key(&key(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.saved_session_selected, 0);
        app.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            app.pending_command,
            Some(("resume".to_string(), "chat-a".to_string())),
            "Enter 把选中会话交给事件循环恢复"
        );
        assert_eq!(app.focus, Focus::Input, "恢复后回输入焦点");
    }

    #[test]
    fn command_hint_keys_win_over_focus_dispatch() {
        // 回归：候选浮层打开时，即使焦点在 Conversation，↑↓/Enter 也必须
        // 操作浮层而不是被焦点路由吞掉（“提示写了按键但按了没反应”）。
        let mut app = AppState::default();
        app.input.set_text("/s".to_string());
        app.command_hint = Some(crate::commands::CommandHintState {
            candidates: crate::commands::CommandRegistry::search("s"),
            selected: 0,
            arg_options: None,
        });
        app.focus = Focus::Conversation;
        app.handle_key(&key(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(
            app.command_hint.as_ref().unwrap().selected,
            1,
            "↓ 移动候选选中项"
        );
        let action = app.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(action, KeyAction::Submit(_)), "Enter 执行候选");
        assert!(app.input.text.is_empty(), "执行后输入框清空");
        assert!(app.command_hint.is_none(), "执行后浮层关闭");
    }

    #[test]
    fn modal_shortcuts_toggle_sidebar_only() {
        // 命令面板为纯 `/` 触发：Ctrl+K 不再打开任何模态。
        let mut app = AppState::default();
        assert!(!app.handle_modal_shortcuts(&key(KeyCode::Char('k'), KeyModifiers::CONTROL)));
        assert_eq!(app.focus, Focus::Input, "Ctrl+K 无模态可开");
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

    #[test]
    fn esc_requires_second_press_to_quit() {
        // Claude Code "Esc Esc" 防误触：首次 Esc 置位并提示，3 秒内再按才退出。
        let mut app = AppState::default();
        assert_eq!(
            app.handle_key(&key(KeyCode::Esc, KeyModifiers::NONE)),
            KeyAction::None,
            "首次 Esc 不退出"
        );
        assert!(app.quit_armed, "首次 Esc 置位");
        assert_eq!(
            app.handle_key(&key(KeyCode::Esc, KeyModifiers::NONE)),
            KeyAction::Quit,
            "3 秒内再按 Esc 退出"
        );
    }

    #[test]
    fn non_esc_key_disarms_quit() {
        let mut app = AppState::default();
        app.handle_key(&key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.quit_armed);
        app.handle_key(&key(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(!app.quit_armed, "任意非 Esc 键复位");
    }

    #[test]
    fn esc_cancels_running_turn() {
        // 生成中按 Esc = 取消（不再直接退出）。
        let mut app = AppState {
            running: true,
            ..Default::default()
        };
        assert_eq!(
            app.handle_key(&key(KeyCode::Esc, KeyModifiers::NONE)),
            KeyAction::Cancel
        );
    }

    #[test]
    fn conversation_vim_scroll_keys() {
        let mut app = AppState {
            focus: Focus::Conversation,
            scroll_offset: 50,
            ..Default::default()
        };
        app.handle_key(&key(KeyCode::Char('b'), KeyModifiers::CONTROL));
        assert_eq!(app.scroll_offset, 30, "ctrl+b 上翻一页（看更早记录）");
        app.handle_key(&key(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert_eq!(app.scroll_offset, 50, "ctrl+f 下翻一页（看更新内容）");
        app.handle_key(&key(KeyCode::Char('g'), KeyModifiers::NONE));
        assert_eq!(app.scroll_offset, 0, "g → 顶部");
        app.handle_key(&key(KeyCode::Char('G'), KeyModifiers::NONE));
        assert!(app.auto_scroll, "G → 贴底");
    }

    #[test]
    fn ctrl_x_chord_triggers_external_editor() {
        // ctrl+x ctrl+e 双键序列 → ExternalEditor（低频动作双键设计）。
        let mut app = AppState::default();
        assert_eq!(
            app.handle_key(&key(KeyCode::Char('x'), KeyModifiers::CONTROL)),
            KeyAction::None,
            "首键 ctrl+x 不触发"
        );
        assert!(app.chord_pending.is_some());
        assert_eq!(
            app.handle_key(&key(KeyCode::Char('e'), KeyModifiers::CONTROL)),
            KeyAction::ExternalEditor,
            "3 秒内 ctrl+e 触发外部编辑器"
        );
        // 非 ctrl+e 第二键不触发。
        let mut app = AppState::default();
        app.handle_key(&key(KeyCode::Char('x'), KeyModifiers::CONTROL));
        assert_eq!(
            app.handle_key(&key(KeyCode::Char('k'), KeyModifiers::CONTROL)),
            KeyAction::None
        );
        assert!(app.chord_pending.is_none(), "第二键后 chord 复位");
    }
}

//! AppState：会话与显示状态（不含渲染与命令逻辑）。
//!
//! - 会话内容唯一真相源在 [`Conversation`]，命令反馈走 `echo` 通道；
//! - 渲染（`draw`）与按键分派（`handle_key`）分别由 `render/` 与 `app/focus` 提供；
//! - 编辑器按键处理（`handle_editor_key`）保留旧版全部编辑语义。

use std::collections::HashMap;

use async_trait::async_trait;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use deepseeknova_core::chunk::Usage;
use deepseeknova_core::runner::RunEvent;

use super::focus::{CompletionState, Focus, PaletteState, SidebarTab};
use crate::input::editor::InputState;
use crate::model::apply::ConversationApply;
use crate::model::conversation::{Conversation, SegId};

/// 回显通道的行上限（命令反馈滚动，防无界增长）。
const MAX_ECHO: usize = 500;

/// 对话面板显示模式（`/raw` 循环切换）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DisplayMode {
    #[default]
    Normal,
    Lite,
    Raw,
}

pub fn display_mode_label(mode: DisplayMode) -> &'static str {
    match mode {
        DisplayMode::Normal => "normal（全量）",
        DisplayMode::Lite => "lite（隐藏推理）",
        DisplayMode::Raw => "raw（带类型前缀）",
    }
}

/// 回显行（命令反馈）。
#[derive(Debug, Clone)]
pub struct UiLine {
    pub kind: crate::model::conversation::LineKind,
    pub text: String,
}

/// 回车后的处理结果。
#[derive(Debug, PartialEq)]
pub enum KeyAction {
    Quit,
    Submit(String),
    Cancel,
    None,
}

/// 会话管理控制器（由 CLI 用 ChatPersistence 实现，TUI 不依赖 CLI 类型）。
#[async_trait]
pub trait SessionController: Send + Sync {
    async fn new_session(&self) -> anyhow::Result<()>;
    async fn list_sessions(&self) -> anyhow::Result<Vec<String>>;
    async fn current_session(&self) -> Option<String>;
    async fn resume(&self, id: &str) -> anyhow::Result<Vec<ResumedLine>>;
    async fn record_turn(
        &self,
        prompt: &str,
        output_text: &str,
        model: Option<String>,
    ) -> anyhow::Result<()>;
}

/// 恢复会话中的一条消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumedLine {
    pub role: ResumedRole,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumedRole {
    User,
    Assistant,
    System,
}

/// 撤销控制器（由 CLI 用 CheckpointManager 实现）。
#[async_trait]
pub trait UndoController: Send + Sync {
    async fn list(&self) -> anyhow::Result<Vec<String>>;
    async fn rollback_one(&self) -> anyhow::Result<Option<String>>;
    async fn rollback_all(&self) -> anyhow::Result<usize>;
}

/// `/mcp` 展示的一个已启用 MCP server。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerInfo {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
}

/// 连接探测结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpStatus {
    Connected,
    Disconnected(String),
}

/// `/mcp` 实时连接探测器。
#[async_trait]
pub trait McpProbe: Send + Sync {
    async fn probe(&self, servers: &[McpServerInfo]) -> Vec<McpStatus>;
}

/// 会话与显示状态。
#[derive(Default)]
pub struct AppState {
    /// 消息树（会话内容唯一真相源）。
    pub conversation: Conversation,
    /// 命令反馈回显行。
    pub echo: Vec<UiLine>,
    /// 当前主题（with_theme / DEEPSEEKNOVA_THEME 解析结果）。
    pub theme: crate::theme::Theme,
    pub input: InputState,
    pub history: Vec<String>,
    pub history_idx: Option<usize>,
    pub running: bool,
    pub turn: usize,
    pub usage: Option<Usage>,
    /// 会话累计成本（美元），由 router ledger 每帧刷新。
    pub total_cost_usd: Option<f64>,
    /// 会话累计上下文占用 `(used_tokens, window_tokens)`，由事件循环每帧
    /// 从 router ledger + `TuiCaps.context_window` 刷新；无 router 或
    /// 未配置 window 时为 None（状态行不显示占用率）。
    pub context_usage: Option<(u64, u64)>,
    pub scroll_offset: usize,
    pub auto_scroll: bool,
    pub model_label: String,
    pub display_mode: DisplayMode,
    /// 最近一次提交的 prompt（回合落盘用）。
    pub last_prompt: Option<String>,
    /// 当前焦点。
    pub focus: Focus,
    /// 消息导航选中的段。
    pub selected: Option<SegId>,
    /// 折叠态（独立存储，不嵌消息树）。
    pub fold: HashMap<SegId, bool>,
    pub sidebar_visible: bool,
    pub sidebar_tab: SidebarTab,
    pub palette: Option<PaletteState>,
    pub completion: Option<CompletionState>,
    /// @ 补全候选文件清单（由 CLI 注入；为空则不触发补全）。
    pub at_files: Vec<String>,
    /// Ctrl+K 面板选中命令后待执行请求（事件循环用真实 caps 消费）。
    pub pending_command: Option<(String, String)>,
}

impl AppState {
    /// 回显一行（命令反馈）。
    pub fn echo_line(&mut self, kind: crate::model::conversation::LineKind, text: &str) {
        self.echo.push(UiLine {
            kind,
            text: text.to_string(),
        });
        if self.echo.len() > MAX_ECHO {
            self.echo.drain(0..self.echo.len() - MAX_ECHO);
        }
    }

    /// 单一入口消费 RunEvent：委托消息树 + 单独消费 Usage。
    pub fn apply_run_event(&mut self, ev: RunEvent) {
        if let RunEvent::Usage(usage) = &ev {
            self.usage = Some(usage.clone());
        }
        self.conversation.apply(ev);
    }

    /// 清空对话面板（/clear、/new、/resume 共用）。
    pub fn clear_display(&mut self) {
        self.conversation = Conversation::default();
        self.echo.clear();
        self.scroll_offset = 0;
        self.auto_scroll = true;
        self.selected = None;
        self.fold.clear();
    }

    /// 折叠判断：显式设置优先，默认按智能策略（推理折叠，其余展开）。
    pub fn is_folded(&self, seg: SegId, kind: crate::model::conversation::LineKind) -> bool {
        match self.fold.get(&seg) {
            Some(f) => *f,
            None => kind == crate::model::conversation::LineKind::Reasoning,
        }
    }

    /// 切换折叠态；默认值也物化为显式设置（便于重置）。
    pub fn toggle_fold(&mut self, seg: SegId) {
        let folded = self.fold.get(&seg).copied().unwrap_or(false);
        self.fold.insert(seg, !folded);
    }

    /// 批量折叠/展开全部段（`/fold all|none`）。
    pub fn fold_all(&mut self, folded: bool) {
        self.fold.clear();
        for (seg, _) in self.conversation.iter_segments() {
            self.fold.insert(seg, folded);
        }
    }

    /// 重置折叠态（`/fold reset`：清空显式设置，回智能默认）。
    pub fn fold_reset(&mut self) {
        self.fold.clear();
    }

    /// 选中下一段；越界回第一段。
    pub fn select_next(&mut self) {
        let segs: Vec<SegId> = self
            .conversation
            .iter_segments()
            .map(|(id, _)| id)
            .collect();
        if segs.is_empty() {
            self.selected = None;
            return;
        }
        let next = match self.selected {
            Some(cur) => {
                let idx = segs.iter().position(|s| *s == cur).unwrap_or(usize::MAX);
                segs[(idx + 1).min(segs.len() - 1)]
            }
            None => segs[0],
        };
        self.selected = Some(next);
    }

    /// 选中上一段；越界回最后一段。
    pub fn select_prev(&mut self) {
        let segs: Vec<SegId> = self
            .conversation
            .iter_segments()
            .map(|(id, _)| id)
            .collect();
        if segs.is_empty() {
            self.selected = None;
            return;
        }
        let prev = match self.selected {
            Some(cur) => {
                let idx = segs
                    .iter()
                    .position(|s| *s == cur)
                    .map(|i| i.saturating_sub(1))
                    .unwrap_or(0);
                segs[idx]
            }
            None => *segs.last().unwrap(),
        };
        self.selected = Some(prev);
    }

    /// 复制当前选中消息。剪贴板能力探测：不可用时降级为回显文本（不报错）。
    pub fn copy_selected(&mut self) {
        let Some(seg) = self.selected.and_then(|id| {
            self.conversation
                .iter_segments()
                .find(|(sid, _)| *sid == id)
                .map(|(_, s)| s.clone())
        }) else {
            self.echo_line(
                crate::model::conversation::LineKind::System,
                "没有选中的消息（先 j/k 选中）",
            );
            return;
        };
        let text = crate::render::message::segment_plain_text(&seg);
        // 剪贴板能力探测：本期降级为回显（见 spec「明确不做」与 plan Task 12 回退），
        // 文案明确提示「未复制到剪贴板」，避免用户误以为已复制。
        self.echo_line(
            crate::model::conversation::LineKind::System,
            &format!("📋 {text}（剪贴板不可用，已回显文本）"),
        );
    }

    /// 编辑器按键（Input 焦点）：保留旧版全部编辑语义。
    pub fn handle_editor_key(&mut self, key: &KeyEvent) -> KeyAction {
        match key.code {
            KeyCode::Esc => {
                if self.running {
                    KeyAction::None
                } else {
                    KeyAction::Quit
                }
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.running {
                    KeyAction::Cancel
                } else {
                    KeyAction::None
                }
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if !self.running {
                    self.input.clear();
                }
                KeyAction::None
            }
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if !self.running {
                    self.input.delete_word_before();
                }
                KeyAction::None
            }
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if !self.running {
                    self.input.home();
                }
                KeyAction::None
            }
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if !self.running {
                    self.input.end();
                }
                KeyAction::None
            }
            KeyCode::Enter => {
                if self.running {
                    return KeyAction::None;
                }
                if key.modifiers.contains(KeyModifiers::SHIFT)
                    || key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    self.input.insert_char('\n');
                    return KeyAction::None;
                }
                let prompt = std::mem::take(&mut self.input);
                let prompt = prompt.text.trim().to_string();
                if prompt.is_empty() {
                    return KeyAction::None;
                }
                if prompt.starts_with('/') {
                    return KeyAction::Submit(prompt);
                }
                self.history.push(prompt.clone());
                self.history_idx = None;
                KeyAction::Submit(prompt)
            }
            KeyCode::Char(c) => {
                if !self.running
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                {
                    self.input.insert_char(c);
                }
                KeyAction::None
            }
            KeyCode::Backspace => {
                if !self.running {
                    self.input.backspace();
                }
                KeyAction::None
            }
            KeyCode::Left => {
                if !self.running {
                    self.input.move_left();
                }
                KeyAction::None
            }
            KeyCode::Right => {
                if !self.running {
                    self.input.move_right();
                }
                KeyAction::None
            }
            KeyCode::Delete => {
                if !self.running {
                    self.input.delete();
                }
                KeyAction::None
            }
            KeyCode::Up => {
                if !self.running {
                    if self.input.text.contains('\n') {
                        self.input.move_line_up();
                    } else {
                        self.history_prev();
                    }
                }
                KeyAction::None
            }
            KeyCode::Down => {
                if !self.running {
                    if self.input.text.contains('\n') {
                        self.input.move_line_down();
                    } else {
                        self.history_next();
                    }
                }
                KeyAction::None
            }
            KeyCode::PageUp => {
                self.scroll_offset = self.scroll_offset.saturating_add(20);
                self.auto_scroll = false;
                KeyAction::None
            }
            KeyCode::PageDown => {
                self.scroll_offset = self.scroll_offset.saturating_sub(20);
                KeyAction::None
            }
            KeyCode::Home => {
                if self.running {
                    self.scroll_offset = 0;
                    self.auto_scroll = false;
                } else {
                    self.input.home_line();
                }
                KeyAction::None
            }
            KeyCode::End => {
                if self.running {
                    self.auto_scroll = true;
                } else {
                    self.input.end_line();
                }
                KeyAction::None
            }
            KeyCode::Tab => {
                // 焦点循环入口：Input（空闲）→ Conversation（消息导航）。
                if !self.running {
                    self.focus = Focus::Conversation;
                }
                KeyAction::None
            }
            _ => KeyAction::None,
        }
    }

    fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let idx = match self.history_idx {
            Some(i) if i > 0 => i - 1,
            Some(_) => 0,
            None => self.history.len() - 1,
        };
        self.history_idx = Some(idx);
        self.input.set_text(self.history[idx].clone());
    }

    fn history_next(&mut self) {
        match self.history_idx {
            Some(i) if i + 1 < self.history.len() => {
                self.history_idx = Some(i + 1);
                self.input.set_text(self.history[i + 1].clone());
            }
            Some(_) => {
                self.history_idx = None;
                self.input.clear();
            }
            None => {}
        }
    }

    /// 滚动钳制：自动跟随贴底，手动滚动不越界。
    pub fn clamp_scroll(&mut self, viewport: usize) {
        let len = self.render_line_count();
        let max = len.saturating_sub(viewport);
        if self.auto_scroll {
            self.scroll_offset = max;
        } else {
            self.scroll_offset = self.scroll_offset.min(max);
        }
    }

    /// 渲染行总数（树 + pending + echo 的近似行数，滚动百分比用）。
    pub fn render_line_count(&self) -> usize {
        self.conversation.segment_count() + self.echo.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::conversation::{done_output, LineKind};
    use crossterm::event::KeyEventKind;
    use deepseeknova_core::runner::RunEvent;

    #[test]
    fn echo_line_caps_at_max() {
        let mut app = AppState::default();
        for i in 0..(MAX_ECHO + 10) {
            app.echo_line(LineKind::System, &format!("x{i}"));
        }
        assert_eq!(app.echo.len(), MAX_ECHO);
    }

    #[test]
    fn apply_event_routes_usage_and_tree() {
        let mut app = AppState::default();
        app.conversation.begin_turn("q".into());
        app.apply_run_event(RunEvent::Usage(Usage {
            prompt_tokens: 1,
            completion_tokens: 2,
            total_tokens: 3,
            reasoning_tokens: 0,
            cache_hit_tokens: 0,
            cache_miss_tokens: 0,
        }));
        app.apply_run_event(RunEvent::TextDelta("hi".into()));
        app.apply_run_event(RunEvent::Done(done_output("")));
        assert!(app.usage.is_some());
        assert_eq!(app.conversation.segment_count(), 1);
    }

    #[test]
    fn fold_default_reasoning_folded_others_open() {
        let mut app = AppState::default();
        let id = app.conversation.begin_turn("q".into());
        app.apply_run_event(RunEvent::ReasoningDelta {
            text: "r".into(),
            signature: None,
        });
        app.apply_run_event(RunEvent::TextDelta("a".into()));
        app.apply_run_event(RunEvent::Done(done_output("")));
        assert!(app.is_folded((id, 0), LineKind::Reasoning));
        assert!(!app.is_folded((id, 1), LineKind::Agent));
        app.toggle_fold((id, 1));
        assert!(app.is_folded((id, 1), LineKind::Agent));
        app.fold_all(false);
        assert!(!app.is_folded((id, 0), LineKind::Reasoning));
        app.fold_reset();
        assert!(app.is_folded((id, 0), LineKind::Reasoning));
    }

    #[test]
    fn selection_moves_through_segments() {
        let mut app = AppState::default();
        let id = app.conversation.begin_turn("q".into());
        app.apply_run_event(RunEvent::TextDelta("a".into()));
        app.apply_run_event(RunEvent::ToolCallStart {
            id: "1".into(),
            name: "g".into(),
        });
        app.apply_run_event(RunEvent::Done(done_output("")));
        app.select_next();
        assert_eq!(app.selected, Some((id, 0)));
        app.select_next();
        assert_eq!(app.selected, Some((id, 1)));
        app.select_next();
        assert_eq!(app.selected, Some((id, 1)), "越界回末段");
        app.select_prev();
        assert_eq!(app.selected, Some((id, 0)));
        app.select_prev();
        assert_eq!(app.selected, Some((id, 0)), "到首不再回绕（回首段）");
    }

    #[test]
    fn editor_keys_edit_and_submit() {
        let mut app = AppState::default();
        let key = |code: KeyCode| KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        };
        app.handle_editor_key(&key(KeyCode::Char('a')));
        app.handle_editor_key(&key(KeyCode::Char('b')));
        assert_eq!(app.input.text, "ab");
        match app.handle_editor_key(&key(KeyCode::Enter)) {
            KeyAction::Submit(p) => assert_eq!(p, "ab"),
            _ => panic!("should submit"),
        }
        assert_eq!(app.history, vec!["ab"], "普通输入入历史");
        // 命令走 Submit 且不入历史
        app.handle_editor_key(&key(KeyCode::Char('/')));
        app.handle_editor_key(&key(KeyCode::Char('q')));
        match app.handle_editor_key(&key(KeyCode::Enter)) {
            KeyAction::Submit(p) => assert_eq!(p, "/q"),
            _ => panic!("should submit command"),
        }
        assert_eq!(app.history, vec!["ab"], "命令不入历史");
    }

    #[test]
    fn clear_display_resets_state() {
        let mut app = AppState::default();
        let id = app.conversation.begin_turn("q".into());
        app.apply_run_event(RunEvent::TextDelta("x".into()));
        app.apply_run_event(RunEvent::Done(done_output("")));
        app.echo_line(LineKind::System, "hi");
        app.fold.insert((id, 0), true);
        app.selected = Some((id, 0));
        app.clear_display();
        assert_eq!(app.conversation.segment_count(), 0);
        assert!(app.echo.is_empty());
        assert!(app.fold.is_empty());
        assert!(app.selected.is_none());
        assert!(app.auto_scroll);
    }

    #[test]
    fn clipboard_unavailable_degrades_to_echo() {
        let mut app = AppState::default();
        let id = app.conversation.begin_turn("q".into());
        app.apply_run_event(RunEvent::TextDelta("hello".into()));
        app.apply_run_event(RunEvent::Done(done_output("")));
        app.selected = Some((id, 0));
        app.copy_selected();
        assert!(app.echo.iter().any(|l| l.text.contains("hello")));
        assert!(
            app.echo.iter().any(|l| l.text.contains("剪贴板不可用")),
            "降级文案明确提示未复制"
        );
    }

    // 迁移占位：原 lib.rs 的输入/滚动测试在 input/editor 模块，见 Task 4。
}

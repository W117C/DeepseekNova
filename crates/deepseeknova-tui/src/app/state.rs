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

use super::focus::{CompletionState, Focus, SidebarTab};
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
    /// ctrl+x ctrl+e：外部编辑器（事件循环挂起终端执行 $EDITOR）。
    ExternalEditor,
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
    /// 本轮运行开始时刻（等待 spinner 帧推导用）；提交时置位，
    /// Done/Cancel 时清除。None 时渲染静态帧。
    pub run_started_at: Option<std::time::Instant>,
    pub turn: usize,
    pub usage: Option<Usage>,
    /// 会话累计成本（美元），由 router ledger 每帧刷新。
    pub total_cost_usd: Option<f64>,
    /// 会话累计上下文占用 `(used_tokens, window_tokens)`，由事件循环每帧
    /// 从 router ledger + `TuiCaps.context_window` 刷新；无 router 或
    /// 未配置 window 时为 None（状态行不显示占用率）。
    pub context_usage: Option<(u64, u64)>,
    pub scroll_offset: usize,
    /// 上一帧估算的对话区物理行数（wrap 后），滚动钳制/百分比口径。
    pub rendered_lines: usize,
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
    pub completion: Option<CompletionState>,
    /// 斜杠命令行内候选（输入 `/` 开头时触发，Claude Code 风格）。
    pub command_hint: Option<crate::commands::CommandHintState>,
    /// 磁盘保存的会话 id 列表（最新优先；事件循环经 SessionController 刷新）。
    pub saved_sessions: Vec<String>,
    /// 当前会话 id（SessionController 上报；侧边栏“当前”标记用）。
    pub current_session: Option<String>,
    /// 是否已从磁盘加载过会话列表（首次加载前渲染“加载中”占位）。
    pub sessions_loaded: bool,
    /// 侧边栏会话列表当前选中项。
    pub saved_session_selected: usize,
    /// @ 补全候选文件清单（由 CLI 注入；为空则不触发补全）。
    pub at_files: Vec<String>,
    /// Ctrl+K 面板选中命令后待执行请求（事件循环用真实 caps 消费）。
    pub pending_command: Option<(String, String)>,
    /// ctrl+x 双键序列状态：首键时间戳（3 秒窗），等待第二键。
    pub chord_pending: Option<std::time::Instant>,
    /// 用户键位定制层（keybindings.json，热重载）。
    pub keymap: crate::app::keybindings::Keymap,
    /// keybindings.json 路径（热重载轮询用）。
    pub keymap_path: std::path::PathBuf,
    /// keybindings.json 上次加载的 mtime。
    pub keymap_mtime: Option<std::time::SystemTime>,
    /// 待审批请求（agent 阻塞等待 y/n；无挂起请求为 None）。
    pub pending_approval: Option<crate::approval::ApprovalRequest>,
    /// Esc 二次确认退出：首次 Esc 置位（提示再按一次退出），
    /// 3 秒无后续按键或任意非 Esc 键复位；与 Claude Code
    /// "Esc Esc / press Esc again to exit" 防误触设计一致。
    pub quit_armed: bool,
    /// `quit_armed` 的置位时间戳（纳秒，monotonic）。
    /// `pub(crate)`：结构体在其他模块以 `..Default::default()` 构造。
    pub(crate) quit_armed_at: Option<std::time::Instant>,
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

    /// 斜杠命令行内候选刷新：输入以 `/` 开头时展示模糊匹配候选
    /// （纯 `/` 触发，Claude Code 风格）；已输入参数（含空格）时
    /// 切换到参数模式：展示该命令的枚举/用法候选（如 `/fold ` →
    /// all|none|reset），Enter 选中执行。无匹配时关闭（Enter 回退
    /// 原样提交，由命令分发报未知命令）。
    pub fn refresh_command_hint(&mut self) {
        let text = self.input.text.trim_start();
        if let Some(cmd) = text.strip_prefix('/') {
            // 已输入参数（含空格）→ 参数模式：仅当命令唯一命中时展示。
            if cmd.contains(char::is_whitespace) {
                let name = cmd.split_whitespace().next().unwrap_or("");
                let args = cmd.split_whitespace().skip(1).collect::<Vec<_>>().join(" ");
                let hint = crate::commands::CommandRegistry::find(name).and_then(|c| {
                    // 仅支持显式 args_hint 的命令进入参数模式。
                    let options = c.args_hint?;
                    let matched: Vec<&'static str> = options
                        .iter()
                        .copied()
                        .filter(|o| o.starts_with(&args) || args.is_empty() || args.contains(o))
                        .collect();
                    if matched.is_empty() {
                        None
                    } else {
                        Some(crate::commands::CommandHintState {
                            candidates: vec![c],
                            selected: 0,
                            arg_options: Some(matched),
                        })
                    }
                });
                self.command_hint = hint;
                return;
            }
            let candidates = crate::commands::CommandRegistry::search(cmd);
            if candidates.is_empty() {
                self.command_hint = None;
                return;
            }
            self.command_hint = Some(crate::commands::CommandHintState {
                candidates,
                selected: 0,
                arg_options: None,
            });
        } else {
            self.command_hint = None;
        }
    }

    /// Esc 二次确认退出：返回是否真正退出（Claude Code "Esc Esc" 防误触）。
    /// - 未置位 → 置位并回显提示（再按一次 Esc 退出），3 秒超时自动复位；
    /// - 已置位（3 秒内再按 Esc）→ 退出。
    pub fn confirm_quit(&mut self) -> KeyAction {
        if self.quit_armed
            && self
                .quit_armed_at
                .map(|t| t.elapsed() < std::time::Duration::from_secs(3))
                .unwrap_or(false)
        {
            KeyAction::Quit
        } else {
            self.quit_armed = true;
            self.quit_armed_at = Some(std::time::Instant::now());
            self.echo_line(
                crate::model::conversation::LineKind::System,
                "再按 Esc 退出（3 秒内）",
            );
            KeyAction::None
        }
    }

    /// 非 Esc 键复位退出确认（防误触连锁）。
    pub fn disarm_quit(&mut self) {
        self.quit_armed = false;
        self.quit_armed_at = None;
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

    /// 折叠判断：显式设置优先，默认按智能策略（推理、工具调用折叠，
    /// 其余展开）。工具调用默认折叠让 agent 输出保持整洁——参数与结果
    /// 可能很长，需要时 Enter 展开查看。
    pub fn is_folded(&self, seg: SegId, kind: crate::model::conversation::LineKind) -> bool {
        match self.fold.get(&seg) {
            Some(f) => *f,
            None => {
                kind == crate::model::conversation::LineKind::Reasoning
                    || kind == crate::model::conversation::LineKind::Tool
            }
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

    /// 斜杠命令候选浮层按键（`/` 触发时优先于焦点分派）。
    ///
    /// 返回 `Some` = 按键已被浮层消费；`None` = 非浮层键，继续走编辑/焦点逻辑。
    /// 浮层打开时即使焦点意外不在 Input（如刚切过侧边栏），↑↓/Enter/Tab/Esc
    /// 也由这里处理——避免“提示写了按键但按了没反应”。
    pub fn handle_command_hint_key(&mut self, key: &KeyEvent) -> Option<KeyAction> {
        self.command_hint.as_ref()?;
        // 循环选择长度：参数模式按 arg_options 数，命令模式按候选数
        //（此前统一用 candidates.len()，参数模式恒为 1 → 选中永远
        // 停在 0，↑↓ 看起来没有交互）。
        let select_len = |h: &crate::commands::CommandHintState| match &h.arg_options {
            Some(opts) => opts.len(),
            None => h.candidates.len(),
        };
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(hint) = &mut self.command_hint {
                    let n = select_len(hint);
                    if n > 0 {
                        hint.selected = (hint.selected + n - 1) % n;
                    }
                }
                Some(KeyAction::None)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(hint) = &mut self.command_hint {
                    let n = select_len(hint);
                    if n > 0 {
                        hint.selected = (hint.selected + 1) % n;
                    }
                }
                Some(KeyAction::None)
            }
            KeyCode::Enter => {
                // 参数模式（`/fold ` 后）：选中枚举/用法候选直接执行完整命令。
                // 命令名模式：已输入的部分参数（`/model sw` 的 `sw`）保留拼接，
                // 未输入参数则空——命令自己处理缺参提示。
                let prefix = self.input.text.trim_start();
                let hint = self.command_hint.take();
                if let Some(h) = &hint {
                    // 参数模式：候选即选中项，命令名取 h.candidates[0]。
                    if let (Some(opts), Some(picked)) =
                        (&h.arg_options, h.candidates.get(h.selected).map(|c| c.name))
                    {
                        if let Some(opt) = opts.get(h.selected).copied() {
                            self.input.clear();
                            return Some(KeyAction::Submit(format!("/{picked} {opt}")));
                        }
                    }
                    let picked = h.candidates.get(h.selected).map(|c| c.name);
                    if let Some(name) = picked {
                        let typed_args = prefix
                            .strip_prefix('/')
                            .map(|rest| {
                                rest.split_once(char::is_whitespace)
                                    .map(|(_, args)| args.trim().to_string())
                                    .unwrap_or_default()
                            })
                            .unwrap_or_default();
                        let full = if typed_args.is_empty() {
                            format!("/{name}")
                        } else {
                            format!("/{name} {typed_args}")
                        };
                        // 执行后清空输入框：命令一旦提交，残留文本会让后续输入
                        // 拼成 `/s/sessions` 之类的无效串（实测复现的“按键没反应”）。
                        self.input.clear();
                        return Some(KeyAction::Submit(full));
                    }
                }
                Some(KeyAction::None)
            }
            KeyCode::Tab => {
                let picked = self
                    .command_hint
                    .as_ref()
                    .and_then(|h| h.candidates.get(h.selected).map(|c| c.name));
                if let Some(name) = picked {
                    // 仅填入命令名 + 空格，让用户继续输入参数再回车。
                    self.input.set_text(format!("/{name} "));
                }
                self.command_hint = None;
                Some(KeyAction::None)
            }
            KeyCode::Esc => {
                self.command_hint = None;
                Some(KeyAction::None)
            }
            _ => None,
        }
    }

    /// 把恢复的会话行灌入消息树（`/resume` 共用）：用户行开新回合，
    /// 助手行落 Text 段，系统行落 System 段；全部标记 Done。
    pub fn restore_conversation(&mut self, lines: Vec<ResumedLine>) {
        self.clear_display();
        for line in lines {
            match line.role {
                ResumedRole::User => {
                    self.conversation.begin_turn(line.text);
                }
                ResumedRole::Assistant => {
                    if let Some(turn) = self.conversation.current_mut() {
                        turn.assistant.flush_all();
                        turn.assistant
                            .segments
                            .push(crate::model::conversation::Segment::Text { text: line.text });
                    }
                }
                ResumedRole::System => {
                    if !line.text.trim().is_empty() {
                        self.conversation
                            .push_system(crate::model::conversation::SystemKind::Info, line.text);
                    }
                }
            }
        }
        if let Some(turn) = self.conversation.current_mut() {
            turn.assistant.flush_all();
            turn.status = crate::model::conversation::TurnStatus::Done;
        }
        self.turn = self.conversation.turn_count();
        // 恢复后会话 id 已切换，让事件循环尽快重拉侧边栏列表。
        self.sessions_loaded = false;
    }

    /// 编辑器按键（Input 焦点）：保留旧版全部编辑语义。
    pub fn handle_editor_key(&mut self, key: &KeyEvent) -> KeyAction {
        if let Some(action) = self.handle_command_hint_key(key) {
            return action;
        }
        match key.code {
            KeyCode::Esc => {
                if self.running {
                    // 生成中按 Esc：取消当前生成（Claude Code Chat context 行为）。
                    KeyAction::Cancel
                } else {
                    // 空闲按 Esc：二次确认退出（防误触）。
                    self.confirm_quit()
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
                    self.refresh_command_hint();
                }
                KeyAction::None
            }
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if !self.running {
                    self.input.delete_word_before();
                    self.refresh_command_hint();
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
                    self.refresh_command_hint();
                }
                KeyAction::None
            }
            KeyCode::Backspace => {
                if !self.running {
                    self.input.backspace();
                    self.refresh_command_hint();
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
                    self.refresh_command_hint();
                }
                KeyAction::None
            }
            KeyCode::Up => {
                if !self.running {
                    if self.command_hint.is_some() {
                        // 候选选择在上面已处理；这里兜底。
                    } else if self.input.text.contains('\n') {
                        self.input.move_line_up();
                    } else {
                        self.history_prev();
                        self.refresh_command_hint();
                    }
                }
                KeyAction::None
            }
            KeyCode::Down => {
                if !self.running {
                    if self.command_hint.is_some() {
                        // 候选选择在上面已处理；这里兜底。
                    } else if self.input.text.contains('\n') {
                        self.input.move_line_down();
                    } else {
                        self.history_next();
                        self.refresh_command_hint();
                    }
                }
                KeyAction::None
            }
            KeyCode::PageUp => {
                // 向上翻页 = 看更早记录（offset 减小）。
                self.scroll_offset = self.scroll_offset.saturating_sub(20);
                self.auto_scroll = false;
                KeyAction::None
            }
            KeyCode::PageDown => {
                self.scroll_offset = self.scroll_offset.saturating_add(20);
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
        let len = self.rendered_lines;
        let max = len.saturating_sub(viewport);
        if self.auto_scroll {
            self.scroll_offset = max;
        } else {
            self.scroll_offset = self.scroll_offset.min(max);
            // 手动滚到最底部后恢复“跟随新消息”（滚轮向下/PageDown 到底即贴底）。
            if self.scroll_offset >= max {
                self.auto_scroll = true;
            }
        }
    }

    /// 渲染行总数（上一帧估算的 wrap 物理行数，滚动百分比用）。
    pub fn render_line_count(&self) -> usize {
        self.rendered_lines
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
        // 命令走 Submit 且不入历史；`/q` 命中 quit 候选 → Enter 执行完整命令
        //（Claude Code 候选优先行为）。
        app.handle_editor_key(&key(KeyCode::Char('/')));
        app.handle_editor_key(&key(KeyCode::Char('q')));
        assert!(app.command_hint.is_some(), "/q 触发命令候选");
        match app.handle_editor_key(&key(KeyCode::Enter)) {
            KeyAction::Submit(p) => assert_eq!(p, "/quit", "候选优先：/q → /quit"),
            _ => panic!("should submit command"),
        }
        assert_eq!(app.history, vec!["ab"], "命令不入历史");
        // 无候选的命令（未知）原样提交，由命令分发报错。
        app.input.clear();
        app.handle_editor_key(&key(KeyCode::Char('/')));
        for c in "wat".chars() {
            app.handle_editor_key(&key(KeyCode::Char(c)));
        }
        assert!(app.command_hint.is_none(), "无匹配候选时 hint 关闭");
        match app.handle_editor_key(&key(KeyCode::Enter)) {
            KeyAction::Submit(p) => assert_eq!(p, "/wat", "无候选原样提交"),
            _ => panic!("should submit"),
        }
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

    #[test]
    fn command_hint_selection_survives_full_key_cycle() {
        // 回归：handle_key 全链路下，↑↓ 选择后选中项不得被后续刷新重置。
        // 此前 Focus::Input 分支在 handle_editor_key 之后无条件重建 hint，
        // selected 恒回 0——浮层高亮永远停在第一项，按键看似无响应。
        let mut app = AppState::default();
        let key = |code: KeyCode| KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        };
        app.handle_key(&key(KeyCode::Char('/')));
        app.handle_key(&key(KeyCode::Char('s')));
        assert!(app.command_hint.is_some(), "/s 触发候选");
        assert_eq!(app.command_hint.as_ref().unwrap().selected, 0);
        app.handle_key(&key(KeyCode::Down));
        assert_eq!(
            app.command_hint.as_ref().unwrap().selected,
            1,
            "↓ 后选中第 2 项（/s 命中 sessions/resume/skills 3 项，不得被重置）"
        );
        app.handle_key(&key(KeyCode::Down));
        assert_eq!(app.command_hint.as_ref().unwrap().selected, 2);
        app.handle_key(&key(KeyCode::Up));
        assert_eq!(app.command_hint.as_ref().unwrap().selected, 1, "↑ 回退一项");
        // Esc 关闭后不得被重建。
        app.handle_key(&key(KeyCode::Esc));
        assert!(app.command_hint.is_none(), "Esc 关闭且不被重建");
    }

    #[test]
    fn ctrl_u_clears_input_and_closes_hint() {
        // 回归：Ctrl+U 清空输入后 hint 必须关闭——此前清空路径不刷新，
        // 残留的候选浮层继续显示（文本已空却仍有候选）。
        let mut app = AppState::default();
        let key = |code: KeyCode, modifiers: KeyModifiers| KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        };
        app.handle_key(&key(KeyCode::Char('/'), KeyModifiers::NONE));
        assert!(app.command_hint.is_some());
        app.handle_key(&key(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert!(app.input.text.is_empty());
        assert!(app.command_hint.is_none(), "Ctrl+U 清空后 hint 关闭");
        // 恢复输入 `/q` 后候选重新出现。
        app.handle_key(&key(KeyCode::Char('/'), KeyModifiers::NONE));
        app.handle_key(&key(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(app.command_hint.is_some(), "重新输入 /q 候选恢复");
    }

    #[test]
    fn delete_slash_closes_hint() {
        // 回归：光标移到行首后 Delete 删除 `/` → hint 关闭（文本不再以 / 开头）。
        // 前向删除（Delete）在光标末尾是无操作，删除 `/` 前必须先 Home。
        let mut app = AppState::default();
        let key = |code: KeyCode| KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        };
        app.handle_key(&key(KeyCode::Char('/')));
        app.handle_key(&key(KeyCode::Char('q')));
        assert!(app.command_hint.is_some());
        app.handle_key(&key(KeyCode::Home));
        app.handle_key(&key(KeyCode::Delete));
        assert_eq!(app.input.text, "q", "Delete 删除行首的 /，留下 q");
        assert!(app.command_hint.is_none(), "删掉 / 后 hint 关闭");
    }

    #[test]
    fn manual_scroll_to_bottom_reenables_following() {
        // 滚轮/PageDown 滚到最底后恢复 auto_scroll：新消息自动贴底可见。
        let mut app = AppState {
            auto_scroll: false,
            scroll_offset: 100,
            rendered_lines: 100,
            ..Default::default()
        };
        app.clamp_scroll(25);
        assert_eq!(app.scroll_offset, 75, "钳制到视口底部");
        assert!(app.auto_scroll, "手动滚到底后恢复跟随");
    }

    #[test]
    fn command_hint_enter_executes_and_clears_input() {
        // 回归：命令候选 Enter 后输入框必须清空——残留文本会让后续输入
        // 拼成 `/s/sessions` 这类无效串，表现为“按了键但没反应/未知命令”。
        let mut app = AppState::default();
        let key = |code: KeyCode| KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        };
        app.handle_key(&key(KeyCode::Char('/')));
        app.handle_key(&key(KeyCode::Char('s')));
        assert!(app.command_hint.is_some(), "/s 触发候选");
        match app.handle_key(&key(KeyCode::Enter)) {
            KeyAction::Submit(p) => assert!(p.starts_with('/'), "执行选中候选: {p}"),
            _ => panic!("Enter 应执行命令"),
        }
        assert!(app.input.text.is_empty(), "执行后输入框清空");
        assert!(app.command_hint.is_none(), "执行后候选关闭");
        // 紧接着正常输入新命令，不再拼接旧文本。
        app.handle_key(&key(KeyCode::Char('/')));
        app.handle_key(&key(KeyCode::Char('s')));
        assert_eq!(app.input.text, "/s", "新输入从空框开始");
    }

    #[test]
    fn restore_conversation_builds_done_turns() {
        let mut app = AppState::default();
        app.restore_conversation(vec![
            ResumedLine {
                role: ResumedRole::User,
                text: "第一个问题".into(),
            },
            ResumedLine {
                role: ResumedRole::Assistant,
                text: "第一个回答".into(),
            },
            ResumedLine {
                role: ResumedRole::System,
                text: "info".into(),
            },
            ResumedLine {
                role: ResumedRole::User,
                text: "第二个问题".into(),
            },
            ResumedLine {
                role: ResumedRole::Assistant,
                text: "第二个回答".into(),
            },
        ]);
        assert_eq!(app.conversation.turn_count(), 2);
        assert_eq!(app.conversation.user_text_of(1), Some("第一个问题"));
        assert_eq!(app.conversation.user_text_of(2), Some("第二个问题"));
        let texts: Vec<&str> = app
            .conversation
            .iter_segments()
            .map(|(_, s)| match s {
                crate::model::conversation::Segment::Text { text } => text.as_str(),
                crate::model::conversation::Segment::System { .. } => "sys",
                _ => "",
            })
            .collect();
        assert_eq!(texts, vec!["第一个回答", "sys", "第二个回答"]);
        assert_eq!(app.turn, 2, "状态行回合数与恢复内容一致");
        assert!(!app.sessions_loaded, "恢复后请求重拉会话列表");
    }

    // 迁移占位：原 lib.rs 的输入/滚动测试在 input/editor 模块，见 Task 4。
}

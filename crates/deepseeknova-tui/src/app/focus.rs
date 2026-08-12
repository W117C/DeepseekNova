//! 焦点状态机：按键按当前焦点分发表路由。
//!
//! 焦点归属：`Input`（编辑器）与 `Conversation`（消息导航）是主焦点；
//! `Sidebar`/`Completion` 为模态焦点；`Confirm` 保留给破坏性操作。
//! 斜杠命令候选（`command_hint`）是非模态的——焦点保持 Input，
//! 候选就地展开在输入区上方（Claude Code 风格）。

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Line;

use crate::i18n::Key;
use crate::model::conversation::{LineKind, SegId, Segment};

use super::actions::{Action, ActionContext};
use super::state::{AppState, DisplayMode, KeyAction, RewindState};

/// 当前焦点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    /// 对话消息导航（j/k、Enter 折叠、y 复制）。
    Conversation,
    /// 输入编辑器（默认）。
    #[default]
    Input,
    /// 侧边栏（Tab/1..5 切面板、j/k 列表）。
    Sidebar,
    /// @ 文件补全浮层。
    Completion,
    /// /help 帮助浮层（Esc/q 关闭，j/k 或 ↑/↓ 滚动）。
    Help,
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

    /// 面板标签的词表键（渲染经 `Tr::t` 取当前语言值）。
    pub fn label(self) -> Key {
        match self {
            SidebarTab::Sessions => Key::TabSessions,
            SidebarTab::Tools => Key::TabTools,
            SidebarTab::Mcp => Key::TabMcp,
            SidebarTab::Cost => Key::TabCost,
            SidebarTab::Skills => Key::TabSkills,
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

/// /help 帮助浮层：全量帮助文本 + 滚动位置。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HelpOverlay {
    /// 帮助行（构建时一次性生成，Esc 关闭后清空）。
    pub lines: Vec<String>,
    /// 当前滚动偏移（可见区首行）。
    pub scroll: usize,
}

/// 鼠标命中映射用的对话区面板宽度近似值（列）。
///
/// AppState 侧拿不到 draw 时的真实面板宽度，点击→段映射按此宽度折行估算，
/// 是尽力而为：短段/未折行内容精确，超长行可能与真实渲染行号有偏差。
const CLICK_HIT_WIDTH: usize = 100;

/// grok 对齐：vim 视位滚动（H/M/L、zz/zt/zb）用的对话区视口高度近似值（行）。
///
/// AppState 侧拿不到 draw 时的真实面板高度，视位定位按此近似高度估算，
/// 与 [`CLICK_HIT_WIDTH`] 同属尽力而为。
const VIM_VIEWPORT_ROWS: usize = 24;

/// vim 视位滚动目标：把选中段贴视口顶部/居中/贴底（H/M/L 与 zz/zt/zb 共用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VimPlacement {
    /// 段贴视口顶部（`H` / `zt`）。
    Top,
    /// 段居中视口（`M` / `zz`）。
    Center,
    /// 段贴视口底部（`L` / `zb`）。
    Bottom,
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
        // 信任确认浮层：y 信任 / n|Esc 不信任（未确认按 untrusted 处理）。
        // 结果经 trust_decision 由事件循环用真实 TrustController 消费。
        if self.trust_prompt.is_some() {
            return match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    self.trust_decision = Some(true);
                    self.trust_prompt = None;
                    KeyAction::None
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.trust_decision = Some(false);
                    self.trust_prompt = None;
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
        // Ctrl+P 命令面板：模态浮层，优先于焦点分派（任意焦点打开即接管）。
        if let Some(action) = self.handle_command_palette_key(key) {
            return action;
        }
        // /help 浮层优先：Esc/q 关闭，j/k、↑/↓ 滚动。
        if self.help_overlay.is_some() {
            return self.handle_help_key(key);
        }
        // grok 对齐：rewind / 对话内搜索 / 历史搜索——模态优先于焦点分派
        //（与 command_palette 同构：激活即接管全部按键）。
        if self.rewind.is_some() {
            return self.handle_rewind_key(key);
        }
        if let Some(action) = self.handle_search_key(key) {
            return action;
        }
        if let Some(action) = self.handle_history_search_key(key) {
            return action;
        }
        // grok 对齐：统一模态（`?` 快捷键速查等）优先于焦点分派。
        // j/k/↑/↓ 滚动 modal_scroll，Esc/q 关闭，其余键忽略。
        if self.active_modal.is_some() {
            return match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    self.modal_scroll = self.modal_scroll.saturating_add(1);
                    KeyAction::None
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.modal_scroll = self.modal_scroll.saturating_sub(1);
                    KeyAction::None
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.active_modal = None;
                    KeyAction::None
                }
                _ => KeyAction::None,
            };
        }
        match self.focus {
            Focus::Conversation => self.handle_conversation_key(key),
            Focus::Input => {
                // grok 对齐：空 prompt 二次 Esc → 打开 rewind（有可回退回合时）；
                // 首次 Esc 仍走 confirm_quit 置位（显示 PressEscAgain），
                // 会话为空时保持既有 Esc Esc 退出语义（不破坏原测试）。
                if key.code == KeyCode::Esc
                    && !self.running
                    && self.input.text.is_empty()
                    && self.quit_armed
                    && self
                        .quit_armed_at
                        .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(3))
                    && !self.ordered_user_turn_ids().is_empty()
                {
                    self.open_rewind();
                    self.disarm_quit();
                    return KeyAction::None;
                }
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
            Focus::Help => KeyAction::None, // handle_help_key 已在浮层分支消费
            Focus::Confirm => self.handle_confirm_key(key),
        }
    }

    /// Ctrl+P 命令面板按键：模态浮层，任意焦点打开即接管。
    ///
    /// 返回 `Some` = 按键已被面板消费；`None` = 非面板键，继续走焦点逻辑。
    /// 字符输入过滤候选、Backspace 回退、↑↓/j/k 选择、Enter 执行（走
    /// `pending_command` 由事件循环用真实 caps 消费）、Esc 关闭。
    fn handle_command_palette_key(&mut self, key: &KeyEvent) -> Option<KeyAction> {
        self.command_palette.as_ref()?;
        let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                self.command_palette = None;
                Some(KeyAction::None)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(pal) = &mut self.command_palette {
                    let n = pal.candidates.len();
                    if n > 0 {
                        pal.selected = (pal.selected + n - 1) % n;
                    }
                }
                Some(KeyAction::None)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(pal) = &mut self.command_palette {
                    let n = pal.candidates.len();
                    if n > 0 {
                        pal.selected = (pal.selected + 1) % n;
                    }
                }
                Some(KeyAction::None)
            }
            KeyCode::Enter => {
                let picked = self
                    .command_palette
                    .as_ref()
                    .and_then(|p| p.candidates.get(p.selected).map(|c| c.name));
                self.command_palette = None;
                if let Some(name) = picked {
                    // 记录最近使用（事件循环执行成功后仍保留，简单起见先记录）。
                    self.record_recent_command(name);
                    self.pending_command = Some((name.to_string(), String::new()));
                }
                Some(KeyAction::None)
            }
            KeyCode::Backspace => {
                if let Some(pal) = &mut self.command_palette {
                    let app_snapshot = self.recent_commands.clone();
                    pal.backspace_snapshot(&app_snapshot);
                }
                Some(KeyAction::None)
            }
            KeyCode::Char(c) if !is_ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                if let Some(pal) = &mut self.command_palette {
                    let app_snapshot = self.recent_commands.clone();
                    pal.type_char_snapshot(&app_snapshot, c);
                }
                Some(KeyAction::None)
            }
            _ => Some(KeyAction::None),
        }
    }

    /// 记录一条最近使用的命令（去重置顶，上限 20）。
    pub(crate) fn record_recent_command(&mut self, name: &str) {
        self.recent_commands.retain(|n| n != name);
        self.recent_commands.insert(0, name.to_string());
        self.recent_commands.truncate(20);
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
    /// vim 滚动）。未绑定键忽略。执行收敛到 [`crate::app::dispatch`]
    /// 单一分发点（对齐 grok 的 Action→Effect 架构）。
    ///
    /// grok 对齐：`g`/`z` 双键序列与 `H`/`M`/`L` 单键在注册表解析前
    /// 就地处理——`g`/`z` 首键置位 `vim_chord`（3 秒窗等待第二键），
    /// `gg`/`zz`/`zt`/`zb` 完成视位滚动；`H`/`M`/`L` 直接按视口高度
    /// 估算定位选中段。
    fn handle_conversation_key(&mut self, key: &KeyEvent) -> KeyAction {
        // 已挂起双键序列：3 秒内第二键完成动作，超时/非法键清除并回落。
        if let Some((lead, started)) = self.vim_chord {
            self.vim_chord = None;
            if started.elapsed() < std::time::Duration::from_secs(3) && key.modifiers.is_empty() {
                let placement = match (lead, key.code) {
                    ('g', KeyCode::Char('g')) => {
                        self.scroll_offset = 0;
                        self.auto_scroll = false;
                        return KeyAction::None;
                    }
                    ('z', KeyCode::Char('z')) => Some(VimPlacement::Center),
                    ('z', KeyCode::Char('t')) => Some(VimPlacement::Top),
                    ('z', KeyCode::Char('b')) => Some(VimPlacement::Bottom),
                    _ => None,
                };
                if let Some(placement) = placement {
                    self.vim_position_selected(placement);
                    return KeyAction::None;
                }
            }
            // 未构成合法序列：落到正常注册表解析。
        }
        // 单键 g/z（无挂起序列）→ 启动双键序列并提示。
        if self.vim_chord.is_none() && key.modifiers.is_empty() {
            match key.code {
                KeyCode::Char('g') | KeyCode::Char('z') => {
                    self.vim_chord = Some((
                        match key.code {
                            KeyCode::Char('g') => 'g',
                            _ => 'z',
                        },
                        std::time::Instant::now(),
                    ));
                    self.show_notice(self.tr.t(Key::VimChordNotice));
                    return KeyAction::None;
                }
                _ => {}
            }
        }
        // H/M/L 单键（无挂起序列）→ 视位滚动（选中段贴顶/居中/贴底）。
        if self.vim_chord.is_none() && key.modifiers.is_empty() {
            let placement = match key.code {
                KeyCode::Char('H') => Some(VimPlacement::Top),
                KeyCode::Char('M') => Some(VimPlacement::Center),
                KeyCode::Char('L') => Some(VimPlacement::Bottom),
                _ => None,
            };
            if let Some(placement) = placement {
                self.vim_position_selected(placement);
                return KeyAction::None;
            }
        }
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
        crate::app::dispatch::fold_effects(crate::app::dispatch::dispatch(self, action))
    }

    /// Sidebar 焦点：action 注册表驱动（面板切换 / 关闭）。执行收敛到
    /// [`crate::app::dispatch`] 单一分发点（对齐 grok 的 Action→Effect 架构）。
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
        crate::app::dispatch::fold_effects(crate::app::dispatch::dispatch(self, action))
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

    /// 鼠标点击 → 选中并折叠命中的段（供 app/mod.rs 事件循环接线）。
    ///
    /// `y` 为终端行号（0 起；对话区在窗口顶部，状态行与输入区在其下方）。
    /// 命中判定按「虚拟行」= `y + scroll_offset` 与各段估算高度累计匹配，
    /// 行高估算见 [`AppState::estimate_segment_rows`]。调用方（app/mod.rs）
    /// 应只在对话区内的左键点击时转发；其余焦点返回 `KeyAction::None`。
    ///
    /// 折叠/展开后保持选中该段，便于用户查看光标高亮位置。
    pub(crate) fn handle_mouse_click(&mut self, y: u16) -> KeyAction {
        if self.focus != Focus::Conversation {
            return KeyAction::None;
        }
        let segs: Vec<(SegId, usize)> = self
            .conversation
            .iter_segments()
            .map(|(id, seg)| (id, self.estimate_segment_rows(id, seg)))
            .collect();
        if segs.is_empty() {
            return KeyAction::None;
        }
        let target = y as usize + self.scroll_offset;
        let mut row = 0usize;
        for (seg_id, height) in segs.iter().copied() {
            let height = height.max(1);
            if target < row + height {
                self.selected = Some(seg_id);
                self.toggle_fold(seg_id);
                return KeyAction::None;
            }
            row += height;
        }
        // 点击落在最后一段之后（对话区底部空白）：选中最末段并折叠。
        if let Some((last, _)) = segs.last() {
            self.selected = Some(*last);
            self.toggle_fold(*last);
        }
        KeyAction::None
    }

    /// 估算单段的渲染行数（鼠标命中映射用）。
    ///
    /// 与 `render::message` 同源（`segment_plain_text` + `estimate_wrapped_lines`），
    /// 渲染宽度以 [`CLICK_HIT_WIDTH`] 近似：折叠段 1 行、Lite 模式隐藏推理段
    /// 0 行、展开段按折行估算。由于缺少真实面板宽度与回合间空行，映射为
    /// 尽力而为——短内容精确，长内容可能与真实渲染行号略有偏差。
    fn estimate_segment_rows(&self, seg_id: SegId, seg: &Segment) -> usize {
        // Lite 模式隐藏推理段。
        if self.display_mode == DisplayMode::Lite && seg.line_kind() == LineKind::Reasoning {
            return 0;
        }
        // 折叠段显示为单行摘要。
        if self.is_folded(seg_id, seg.line_kind()) {
            return 1;
        }
        let text = crate::render::message::segment_plain_text(seg, self.tr);
        let lines: Vec<Line<'_>> = text.split('\n').map(Line::from).collect();
        crate::render::message::estimate_wrapped_lines(&lines, CLICK_HIT_WIDTH).max(1)
    }

    /// 全局模态热键：Ctrl+T 鼠标捕获切换、Ctrl+\ 侧边栏开合（任意焦点生效）。
    /// 命令面板为纯 `/` 触发（就地候选，非模态），无 Ctrl+K。
    pub fn handle_modal_shortcuts(&mut self, key: &KeyEvent) -> bool {
        // Ctrl+T：切换鼠标捕获（滚轮滚动对话 vs 鼠标选中复制）。
        // Ctrl+M 在终端层面等价 Enter，不可用，故取未占用的 Ctrl+T。
        if key.code == KeyCode::Char('t') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.mouse_capture = !self.mouse_capture;
            if self.mouse_capture {
                self.show_notice(self.tr.t(Key::MouseCaptureOn));
            } else {
                self.show_notice(self.tr.t(Key::MouseCaptureOff));
            }
            return true;
        }
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
            Action::PermModeCycle => {
                // Ctrl+Shift+P：循环权限模式预设（事件循环用真实 gate 消费）。
                self.perm_mode_cycle = true;
                true
            }
            Action::OpenCommandPalette => {
                // Ctrl+P：打开命令面板（全命令模糊搜索 + 最近使用排序）。
                self.command_palette = Some(crate::app::state::CommandPaletteState::open(self));
                true
            }
            Action::ToggleTasks => {
                // Ctrl+G：切换 Tasks 面板（进行中的工具/子代理任务，grok 对齐）。
                self.tasks_visible = !self.tasks_visible;
                true
            }
            Action::AppHelp => {
                // F1：复用 /help 命令（pending_command 由事件循环用真实 caps 执行）。
                self.pending_command = Some(("help".to_string(), String::new()));
                true
            }
            Action::AppRedraw => {
                // Ctrl+L：请求清屏重绘（事件循环消费）。
                self.redraw_requested = true;
                true
            }
            Action::ToggleFullscreen => {
                // Ctrl+Shift+F：全屏切换（隐藏状态行/提示行），带临时反馈。
                self.toggle_fullscreen();
                let text = if self.fullscreen {
                    self.tr.t(Key::FullscreenOn)
                } else {
                    self.tr.t(Key::FullscreenOff)
                };
                self.show_notice(text);
                true
            }
            Action::Redraw => {
                // Ctrl+Shift+R：请求强制全量重绘（事件循环消费）。
                self.redraw_requested = true;
                true
            }
            _ => false,
        }
    }

    /// grok 对齐：rewind 浮层按键（激活时优先消费）。
    ///
    /// k/j、↑/↓ 循环选择回退点；Enter 置 `rewind_pending`（保留浮层状态
    /// 供事件循环读取选中项后截断并统一关闭）；Esc 直接关闭。
    fn handle_rewind_key(&mut self, key: &KeyEvent) -> KeyAction {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.step_rewind(-1);
                KeyAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.step_rewind(1);
                KeyAction::None
            }
            KeyCode::Enter => {
                // 确认回退：置待消费标记（事件循环消费时读取选中项并关闭）。
                self.rewind_pending = true;
                KeyAction::None
            }
            KeyCode::Esc => {
                self.rewind = None;
                self.rewind_pending = false;
                KeyAction::None
            }
            _ => KeyAction::None,
        }
    }

    /// rewind 选择导航（循环）。
    fn step_rewind(&mut self, dir: isize) {
        let Some(r) = &mut self.rewind else {
            return;
        };
        let n = r.turns.len();
        if n == 0 {
            return;
        }
        if dir > 0 {
            r.selected = (r.selected + 1) % n;
        } else {
            r.selected = (r.selected + n - 1) % n;
        }
    }

    /// 打开 rewind 回退浮层：遍历会话中的用户回合生成 `#N 首行摘要` 标题
    /// （最早的在先，摘要截 60 字符）。
    fn open_rewind(&mut self) {
        let turns = self
            .ordered_user_turn_ids()
            .iter()
            .enumerate()
            .map(|(i, tid)| {
                let summary = self
                    .conversation
                    .user_text_of(*tid)
                    .map(|t| t.lines().next().unwrap_or("").trim())
                    .unwrap_or("");
                if summary.is_empty() {
                    format!("#{}", i + 1)
                } else {
                    let head: String = summary.chars().take(60).collect();
                    format!("#{} {}", i + 1, head)
                }
            })
            .collect();
        self.rewind = Some(RewindState { turns, selected: 0 });
    }

    /// grok 对齐：会话中的用户回合 id（按出现顺序去重），
    /// rewind 浮层构建与回退截断共用。
    pub(crate) fn ordered_user_turn_ids(&self) -> Vec<u64> {
        let mut ids: Vec<u64> = Vec::new();
        for (id, _) in self.conversation.iter_segments() {
            if !ids.contains(&id.0) {
                ids.push(id.0);
            }
        }
        ids
    }

    /// grok 对齐：消费 rewind 回退请求——把会话截断到选中回合之前
    /// （重建消息树，保留完整回合），并复位 rewind 状态。
    ///
    /// 由事件循环在 `rewind_pending` 置位后调用；浮层已关闭（异常路径）
    /// 时回退到最近回合（清空除最后一回合外的全部）。
    pub(crate) fn consume_rewind(&mut self) {
        self.rewind_pending = false;
        let ids = self.ordered_user_turn_ids();
        let selected = self
            .rewind
            .as_ref()
            .map(|r| r.selected)
            .unwrap_or_else(|| ids.len().saturating_sub(1));
        self.rewind = None;
        let keep_count = selected.min(ids.len());
        let mut conv = crate::model::conversation::Conversation::default();
        for tid in ids.into_iter().take(keep_count) {
            let text = self
                .conversation
                .user_text_of(tid)
                .map(|s| s.to_string())
                .unwrap_or_default();
            conv.begin_turn(text);
            for (id, seg) in self.conversation.iter_segments() {
                if id.0 == tid {
                    if let Some(t) = conv.current_mut() {
                        t.assistant.segments.push(seg.clone());
                    }
                }
            }
            if let Some(t) = conv.current_mut() {
                t.assistant.flush_all();
                t.status = crate::model::conversation::TurnStatus::Done;
            }
        }
        self.conversation = conv;
        self.turn = self.conversation.turn_count();
        self.selected = None;
        self.fold.clear();
        self.auto_scroll = true;
    }

    /// grok 对齐：对话内搜索按键（激活时优先消费）。返回 `Some` = 已消费。
    ///
    /// 字符输入/Backspace 即时重算命中（`iter_segments` +
    /// `segment_plain_text` 子串匹配）；Enter/`n` 下一个、Shift+Enter/`N`
    /// 上一个、Esc 关闭、Ctrl+F 二次按下关闭；无命中时导航不循环
    /// （钳制在首尾）。
    fn handle_search_key(&mut self, key: &KeyEvent) -> Option<KeyAction> {
        self.search.as_ref()?;
        let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let is_shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let has_alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => {
                self.search = None;
                Some(KeyAction::None)
            }
            KeyCode::Char('f') if is_ctrl => {
                // Ctrl+F 二次按下 = 关闭（与 ConvSearchOpen 开关语义一致）。
                self.search = None;
                Some(KeyAction::None)
            }
            KeyCode::Enter if !is_shift => {
                self.step_search(1);
                Some(KeyAction::None)
            }
            KeyCode::Enter if is_shift => {
                self.step_search(-1);
                Some(KeyAction::None)
            }
            // 导航键改为带 Ctrl 修饰（grok 搜索同款）：裸 `n`/`N`/Shift+n
            // 必须能输入查询（如 "function"/"connection"——审查#9 P2 回归），
            // 否则查询含 'n' 无法键入。Ctrl+n 下一个、Ctrl+N 上一个。
            KeyCode::Char('n') if is_ctrl => {
                self.step_search(1);
                Some(KeyAction::None)
            }
            KeyCode::Char('N') if is_ctrl => {
                self.step_search(-1);
                Some(KeyAction::None)
            }
            KeyCode::Char(c) if !is_ctrl && !has_alt => {
                // 允许带 Shift 的字符（大写字母等）进入查询，裸小写 `n` 同此路径。
                if let Some(s) = &mut self.search {
                    s.query.push(c);
                }
                self.recompute_search();
                self.scroll_to_search_hit();
                Some(KeyAction::None)
            }
            KeyCode::Backspace => {
                if let Some(s) = &mut self.search {
                    s.query.pop();
                }
                self.recompute_search();
                self.scroll_to_search_hit();
                Some(KeyAction::None)
            }
            _ => Some(KeyAction::None),
        }
    }

    /// 搜索命中导航：`dir>0` 下一个、`dir<0` 上一个；无命中不循环
    /// （下一个钳制到末尾，上一个钳制到开头）。
    fn step_search(&mut self, dir: isize) {
        let Some(s) = &mut self.search else {
            return;
        };
        if s.matches.is_empty() {
            return;
        }
        if dir > 0 {
            s.selected = (s.selected + 1).min(s.matches.len() - 1);
        } else {
            s.selected = s.selected.saturating_sub(1);
        }
        self.scroll_to_search_hit();
    }

    /// 按当前查询重算搜索命中（段正文子串匹配，忽略大小写），
    /// 选中复位到首个命中、总数更新。
    fn recompute_search(&mut self) {
        // 先取查询与翻译器，避免与 search 的可变借用冲突。
        let (query, tr) = match &self.search {
            Some(s) => (s.query.to_lowercase(), self.tr),
            None => return,
        };
        let mut matches: Vec<SegId> = Vec::new();
        for (id, seg) in self.conversation.iter_segments() {
            let text = crate::render::message::segment_plain_text(seg, tr);
            if text.to_lowercase().contains(&query) {
                matches.push(id);
            }
        }
        if let Some(s) = &mut self.search {
            s.matches = matches;
            s.total = s.matches.len() as isize;
            s.selected = 0;
        }
    }

    /// 滚动到当前选中的搜索命中段（段顶对齐视口，估算行高）。
    fn scroll_to_search_hit(&mut self) {
        let target = self
            .search
            .as_ref()
            .and_then(|s| s.matches.get(s.selected).copied());
        let Some(target) = target else {
            return;
        };
        if let Some(row) = self.segment_row(target) {
            self.scroll_offset = row;
            self.auto_scroll = false;
        }
    }

    /// 段在会话中的起始行（其上方各段估算行高累计）；未找到返回 None。
    fn segment_row(&self, target: SegId) -> Option<usize> {
        let mut row = 0usize;
        for (id, seg) in self.conversation.iter_segments() {
            if id == target {
                return Some(row);
            }
            row += self.estimate_segment_rows(id, seg).max(1);
        }
        None
    }

    /// grok 对齐：历史搜索按键（激活时优先消费）。返回 `Some` = 已消费。
    ///
    /// 字符输入/Backspace 即时重算前缀匹配（倒序：最近的在先）；
    /// ↑/Ctrl+R 上一个、↓ 下一个、Enter 采纳回写输入框（光标到末尾）、
    /// Esc 关闭。
    fn handle_history_search_key(&mut self, key: &KeyEvent) -> Option<KeyAction> {
        self.history_search.as_ref()?;
        let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let has_alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => {
                self.history_search = None;
                Some(KeyAction::None)
            }
            KeyCode::Char(c) if !is_ctrl && !has_alt => {
                if let Some(h) = &mut self.history_search {
                    h.query.push(c);
                }
                self.recompute_history_search();
                Some(KeyAction::None)
            }
            KeyCode::Backspace => {
                if let Some(h) = &mut self.history_search {
                    h.query.pop();
                }
                self.recompute_history_search();
                Some(KeyAction::None)
            }
            KeyCode::Up => {
                self.step_history_search(-1);
                Some(KeyAction::None)
            }
            KeyCode::Char('r') if is_ctrl => {
                self.step_history_search(-1);
                Some(KeyAction::None)
            }
            KeyCode::Down => {
                self.step_history_search(1);
                Some(KeyAction::None)
            }
            KeyCode::Enter => {
                let picked = self
                    .history_search
                    .as_ref()
                    .and_then(|h| h.matches.get(h.selected))
                    .and_then(|&i| self.history.get(i).cloned());
                self.history_search = None;
                if let Some(text) = picked {
                    self.input.set_text(text);
                    self.refresh_command_hint();
                }
                Some(KeyAction::None)
            }
            _ => Some(KeyAction::None),
        }
    }

    /// 历史搜索命中导航（循环）：`dir>0` 下一个、`dir<0` 上一个。
    fn step_history_search(&mut self, dir: isize) {
        let Some(h) = &mut self.history_search else {
            return;
        };
        let n = h.matches.len();
        if n == 0 {
            return;
        }
        if dir > 0 {
            h.selected = (h.selected + 1) % n;
        } else {
            h.selected = (h.selected + n - 1) % n;
        }
    }

    /// 按当前查询重算历史前缀匹配（倒序：最近的在先），选中复位到首个。
    fn recompute_history_search(&mut self) {
        let query = match &self.history_search {
            Some(h) => h.query.clone(),
            None => return,
        };
        let mut matches: Vec<usize> = self
            .history
            .iter()
            .enumerate()
            .filter(|(_, item)| item.starts_with(&query))
            .map(|(i, _)| i)
            .collect();
        matches.reverse();
        if let Some(h) = &mut self.history_search {
            h.matches = matches;
            h.selected = 0;
        }
    }

    /// vim 视位滚动：把选中段定位到视口顶部/中部/底部（估算行高）。
    fn vim_position_selected(&mut self, placement: VimPlacement) {
        let Some(seg) = self.selected else {
            return;
        };
        let mut row = 0usize;
        let mut height = 0usize;
        let mut found = false;
        for (id, s) in self.conversation.iter_segments() {
            let h = self.estimate_segment_rows(id, s).max(1);
            if id == seg {
                height = h;
                found = true;
                break;
            }
            row += h;
        }
        if !found {
            return;
        }
        let viewport = VIM_VIEWPORT_ROWS.max(1);
        let offset = match placement {
            VimPlacement::Top => row,
            VimPlacement::Center => (row + height / 2).saturating_sub(viewport / 2),
            VimPlacement::Bottom => (row + height).saturating_sub(viewport),
        };
        self.scroll_offset = offset;
        self.auto_scroll = false;
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
            saved_sessions: vec![
                crate::app::state::SessionMeta {
                    id: "chat-a".into(),
                    preview: "第一个".into(),
                    title: None,
                    workspace: None,
                },
                crate::app::state::SessionMeta {
                    id: "chat-b".into(),
                    preview: "第二个".into(),
                    title: None,
                    workspace: None,
                },
            ],
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
        let mut app = AppState {
            tr: crate::i18n::Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        assert!(!app.handle_modal_shortcuts(&key(KeyCode::Char('k'), KeyModifiers::CONTROL)));
        assert_eq!(app.focus, Focus::Input, "Ctrl+K 无模态可开");
        assert!(!app.sidebar_visible);
        // Ctrl+T：鼠标捕获开关（默认 false 的测试构造 → 切到开启，并给临时提示）。
        assert!(app.handle_modal_shortcuts(&key(KeyCode::Char('t'), KeyModifiers::CONTROL)));
        assert!(app.mouse_capture, "Ctrl+T 应开启鼠标捕获");
        assert!(
            app.notice
                .as_ref()
                .is_some_and(|(t, _)| t.contains("鼠标捕获已开启")),
            "开启时应给临时反馈"
        );
        assert!(app.handle_modal_shortcuts(&key(KeyCode::Char('t'), KeyModifiers::CONTROL)));
        assert!(!app.mouse_capture, "再次 Ctrl+T 应关闭鼠标捕获");
        assert!(
            app.notice
                .as_ref()
                .is_some_and(|(t, _)| t.contains("鼠标捕获已关闭")),
            "关闭时应给临时反馈"
        );
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
    fn f1_opens_help_and_ctrl_l_requests_redraw() {
        let mut app = AppState::default();
        // F1 → 经 pending_command 复用 /help 命令路径。
        assert!(app.handle_modal_shortcuts(&key(KeyCode::F(1), KeyModifiers::NONE)));
        assert_eq!(
            app.pending_command,
            Some(("help".to_string(), String::new()))
        );
        // Ctrl+L → 请求清屏重绘（事件循环消费）。
        assert!(app.handle_modal_shortcuts(&key(KeyCode::Char('l'), KeyModifiers::CONTROL)));
        assert!(app.redraw_requested);
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
        // grok 对齐：ctrl+f 现在是打开对话内搜索，不再是翻页。
        app.handle_key(&key(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert!(app.search.is_some(), "ctrl+f 打开对话内搜索");
        assert_eq!(app.scroll_offset, 30, "ctrl+f 不再改变滚动位置");
        app.handle_key(&key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.search.is_none(), "Esc 关闭搜索");
        // grok 对齐：g 是 vim 双键首键（gg 到顶），不再是单键到顶。
        app.handle_key(&key(KeyCode::Char('g'), KeyModifiers::NONE));
        assert!(app.vim_chord.is_some(), "g 置位 vim 双键序列");
        app.handle_key(&key(KeyCode::Char('g'), KeyModifiers::NONE));
        assert_eq!(app.scroll_offset, 0, "gg → 顶部");
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

    #[test]
    fn sidebar_brackets_adjust_width_with_clamp_and_notice() {
        let mut app = AppState {
            focus: Focus::Sidebar,
            sidebar_width: 30,
            tr: crate::i18n::Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        // `[` 加宽一列并回显当前宽度。
        app.handle_key(&key(KeyCode::Char('['), KeyModifiers::NONE));
        assert_eq!(app.sidebar_width, 31);
        assert!(
            app.notice
                .as_ref()
                .is_some_and(|(t, _)| t.contains("侧边栏宽度: 31")),
            "加宽后回显当前宽度"
        );
        // 收窄到下限 26，继续 `]` 不再变。
        for _ in 0..20 {
            app.handle_key(&key(KeyCode::Char(']'), KeyModifiers::NONE));
        }
        assert_eq!(app.sidebar_width, 26, "下限 26 钳制");
        // 加宽到上限 60，继续 `[` 不再变。
        for _ in 0..100 {
            app.handle_key(&key(KeyCode::Char('['), KeyModifiers::NONE));
        }
        assert_eq!(app.sidebar_width, 60, "上限 60 钳制");
        assert!(
            app.notice
                .as_ref()
                .is_some_and(|(t, _)| t.contains("侧边栏宽度: 60")),
            "钳制后仍回显当前宽度"
        );
        // 输入区 `[` 不被劫持：仍是自由插入字符。
        let mut app = AppState::default();
        app.handle_key(&key(KeyCode::Char('['), KeyModifiers::NONE));
        assert_eq!(app.input.text, "[", "输入区 `[` 仍插入文本");
    }

    #[test]
    fn fullscreen_toggle_flips_field_and_notice() {
        let mut app = AppState {
            tr: crate::i18n::Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        assert!(!app.fullscreen);
        assert!(app.handle_modal_shortcuts(&key(
            KeyCode::Char('F'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        )));
        assert!(app.fullscreen, "Ctrl+Shift+F 进入全屏");
        assert!(
            app.notice
                .as_ref()
                .is_some_and(|(t, _)| t.contains("已进入全屏模式")),
            "进入全屏给临时反馈"
        );
        assert!(app.handle_modal_shortcuts(&key(
            KeyCode::Char('F'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        )));
        assert!(!app.fullscreen, "再次 Ctrl+Shift+F 退出全屏");
        assert!(
            app.notice
                .as_ref()
                .is_some_and(|(t, _)| t.contains("已退出全屏模式")),
            "退出全屏给临时反馈"
        );
    }

    #[test]
    fn ctrl_shift_r_requests_redraw() {
        let mut app = AppState::default();
        assert!(app.handle_modal_shortcuts(&key(
            KeyCode::Char('R'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        )));
        assert!(app.redraw_requested, "Ctrl+Shift+R 置位重绘标记");
    }

    #[test]
    fn mouse_click_selects_and_toggles_fold() {
        let mut app = AppState::default();
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

        // 点击第 0 行：命中推理段（Auto 策略默认折叠 → 估算 1 行），选中并展开。
        let action = app.handle_mouse_click(0);
        assert!(matches!(action, KeyAction::None));
        assert_eq!(app.selected, Some((id, 0)));
        assert_eq!(
            app.fold.get(&(id, 0)),
            Some(&crate::app::state::FoldState::Expanded),
            "默认折叠的推理段被展开"
        );

        // 点击第 1 行：命中正文段，选中并折叠。
        app.handle_mouse_click(1);
        assert_eq!(app.selected, Some((id, 1)));
        assert_eq!(
            app.fold.get(&(id, 1)),
            Some(&crate::app::state::FoldState::Collapsed),
            "正文段被折叠"
        );

        // 点击最后一段之后（对话区底部空白）：选中最末段并再次切换折叠。
        app.handle_mouse_click(10);
        assert_eq!(app.selected, Some((id, 1)));
        assert_eq!(
            app.fold.get(&(id, 1)),
            Some(&crate::app::state::FoldState::Truncated),
            "再点一次切到截断态"
        );
    }

    #[test]
    fn mouse_click_ignored_outside_conversation() {
        let mut app = AppState {
            focus: Focus::Input,
            ..Default::default()
        };
        let action = app.handle_mouse_click(0);
        assert!(matches!(action, KeyAction::None));
        assert_eq!(app.selected, None, "非对话焦点点击不选中");
    }

    #[test]
    fn vim_chord_gg_scrolls_top_and_timeout_clears() {
        // g → 置位序列并提示；gg → 回到顶部；超时后下一键清除序列并回落。
        let mut app = AppState {
            focus: Focus::Conversation,
            scroll_offset: 50,
            ..Default::default()
        };
        app.handle_key(&key(KeyCode::Char('g'), KeyModifiers::NONE));
        assert!(app.vim_chord.is_some(), "g 置位双键序列");
        assert!(app.notice.is_some(), "g 显示等待第二键提示");
        app.handle_key(&key(KeyCode::Char('g'), KeyModifiers::NONE));
        assert_eq!(app.scroll_offset, 0, "gg 回到顶部");
        assert!(app.vim_chord.is_none(), "第二键后序列复位");
        // 再次 g 后把时间戳改过期：下一键清除序列并走正常解析（j 不再被吞）。
        app.handle_key(&key(KeyCode::Char('g'), KeyModifiers::NONE));
        app.vim_chord = Some((
            'g',
            std::time::Instant::now() - std::time::Duration::from_secs(5),
        ));
        assert_eq!(
            app.handle_key(&key(KeyCode::Char('j'), KeyModifiers::NONE)),
            KeyAction::None,
            "超时后第二键回落到正常解析"
        );
        assert!(app.vim_chord.is_none(), "超时后序列清除");
    }

    #[test]
    fn vim_hm_l_single_key_positions_selected_segment() {
        // H/M/L 单键（无 g/z 序列）按视口高度估算滚动，定位选中段。
        let mut app = AppState {
            focus: Focus::Conversation,
            ..Default::default()
        };
        let _id1 = app.conversation.begin_turn("one".into());
        app.apply_run_event(deepseeknova_core::runner::RunEvent::TextDelta(
            "alpha".into(),
        ));
        app.apply_run_event(deepseeknova_core::runner::RunEvent::Done(
            crate::model::conversation::done_output(""),
        ));
        let id2 = app.conversation.begin_turn("two".into());
        app.apply_run_event(deepseeknova_core::runner::RunEvent::TextDelta(
            "beta".into(),
        ));
        app.apply_run_event(deepseeknova_core::runner::RunEvent::Done(
            crate::model::conversation::done_output(""),
        ));
        let second_text = (id2, 0);
        app.selected = Some(second_text);
        // 选中第二回合正文段，H 应把其估算行定位到视口顶部（row > 0）。
        app.handle_key(&key(KeyCode::Char('H'), KeyModifiers::NONE));
        assert!(
            app.scroll_offset > 0,
            "H 定位到首段之后: {}",
            app.scroll_offset
        );
        assert!(!app.auto_scroll, "H 手动定位关闭跟随");
        assert!(app.vim_chord.is_none(), "H 不启动双键序列");
        // L 贴底：目标行 + 段高 - 视口（估算，仍 >= 0）。
        app.handle_key(&key(KeyCode::Char('L'), KeyModifiers::NONE));
        assert!(!app.auto_scroll);
        // 未选中段时 H/M/L 无副作用（不 panic）。
        let mut app = AppState {
            focus: Focus::Conversation,
            ..Default::default()
        };
        app.handle_key(&key(KeyCode::Char('M'), KeyModifiers::NONE));
        assert_eq!(app.scroll_offset, 0);
    }

    #[test]
    fn search_filters_segments_and_navigates() {
        let mut app = AppState {
            focus: Focus::Conversation,
            ..Default::default()
        };
        app.conversation.begin_turn("one".into());
        app.apply_run_event(deepseeknova_core::runner::RunEvent::TextDelta(
            "alpha".into(),
        ));
        app.apply_run_event(deepseeknova_core::runner::RunEvent::Done(
            crate::model::conversation::done_output(""),
        ));
        app.conversation.begin_turn("two".into());
        app.apply_run_event(deepseeknova_core::runner::RunEvent::TextDelta(
            "beta".into(),
        ));
        app.apply_run_event(deepseeknova_core::runner::RunEvent::Done(
            crate::model::conversation::done_output(""),
        ));
        // Ctrl+F 打开搜索条。
        app.handle_key(&key(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert!(app.search.is_some(), "ctrl+f 打开搜索");
        // 输入 'a'：alpha 与 beta 均含 'a' → 2 个命中，选中复位 0。
        app.handle_key(&key(KeyCode::Char('a'), KeyModifiers::NONE));
        let s = app.search.as_ref().unwrap();
        assert_eq!(s.matches.len(), 2, "两个段都含 'a'");
        assert_eq!(s.total, 2);
        assert_eq!(s.selected, 0);
        // Enter 下一个命中（钳制到末尾，不循环）；N 上一个。
        app.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.search.as_ref().unwrap().selected, 1);
        app.handle_key(&key(KeyCode::Char('N'), KeyModifiers::NONE));
        assert_eq!(app.search.as_ref().unwrap().selected, 0);
        // 无命中时 Enter 不循环。
        app.handle_key(&key(KeyCode::Char('x'), KeyModifiers::NONE));
        app.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.search.as_ref().unwrap().selected, 0, "无命中不移动");
        // Esc 关闭。
        app.handle_key(&key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.search.is_none(), "Esc 关闭搜索");
    }

    #[test]
    fn history_search_accept_writes_input() {
        let mut app = AppState {
            history: vec!["hello".into(), "help".into(), "world".into()],
            ..Default::default()
        };
        // Ctrl+R 打开历史搜索。
        app.handle_key(&key(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert!(app.history_search.is_some(), "ctrl+r 打开历史搜索");
        // 输入 'h'：前缀匹配 hello/help，倒序（最近的在先）。
        app.handle_key(&key(KeyCode::Char('h'), KeyModifiers::NONE));
        let h = app.history_search.as_ref().unwrap();
        assert_eq!(h.matches, vec![1, 0], "倒序：help 在前");
        // Ctrl+R 上一个（切回 hello）；↓ 下一个（回到 help）。
        app.handle_key(&key(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert_eq!(app.history_search.as_ref().unwrap().selected, 1);
        app.handle_key(&key(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.history_search.as_ref().unwrap().selected, 0);
        // Enter 采纳 help 到输入框（光标到末尾）。
        app.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.history_search.is_none(), "采纳后关闭");
        assert_eq!(app.input.text, "help");
    }

    #[test]
    fn esc_esc_opens_rewind_with_turns() {
        let mut app = AppState::default();
        app.conversation.begin_turn("first".into());
        app.apply_run_event(deepseeknova_core::runner::RunEvent::TextDelta("a".into()));
        app.apply_run_event(deepseeknova_core::runner::RunEvent::Done(
            crate::model::conversation::done_output(""),
        ));
        // 首次 Esc：confirm_quit 置位并提示。
        app.handle_key(&key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.quit_armed, "首次 Esc 置位退出确认");
        // 空 prompt 二次 Esc：打开 rewind 并复位退出确认（不退出）。
        app.handle_key(&key(KeyCode::Esc, KeyModifiers::NONE));
        let rewind = app.rewind.as_ref().expect("空 prompt 二次 Esc 打开 rewind");
        assert_eq!(rewind.turns.len(), 1, "每个用户回合一条可回退项");
        assert!(rewind.turns[0].starts_with("#1 "), "标题含回合号+首行摘要");
        assert!(!app.quit_armed, "打开 rewind 后复位退出确认");
        // rewind 激活时 k/j 选择、Esc 关闭（不触发退出）。
        app.handle_key(&key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.rewind.is_none(), "rewind 内 Esc 关闭浮层");
        assert!(!app.quit_armed, "rewind 内 Esc 不触发退出");
    }

    #[test]
    fn consume_rewind_truncates_to_selected_turn() {
        let mut app = AppState::default();
        app.conversation.begin_turn("one".into());
        app.apply_run_event(deepseeknova_core::runner::RunEvent::TextDelta("a".into()));
        app.apply_run_event(deepseeknova_core::runner::RunEvent::Done(
            crate::model::conversation::done_output(""),
        ));
        let id2 = app.conversation.begin_turn("two".into());
        app.apply_run_event(deepseeknova_core::runner::RunEvent::TextDelta("b".into()));
        app.apply_run_event(deepseeknova_core::runner::RunEvent::Done(
            crate::model::conversation::done_output(""),
        ));
        // 打开 rewind，选中第二个回合（selected=1）→ 消费后只保留第一个。
        app.open_rewind();
        app.rewind.as_mut().unwrap().selected = 1;
        app.rewind_pending = true;
        app.consume_rewind();
        assert_eq!(app.conversation.turn_count(), 1, "截断到选中回合之前");
        assert_eq!(app.conversation.user_text_of(id2), None, "选中回合被移除");
        assert!(!app.rewind_pending, "消费后复位标记");
        assert!(app.rewind.is_none(), "消费后关闭浮层");
    }

    #[test]
    fn alt_m_toggles_multiline_mode() {
        // Alt+M：Input 焦点经默认绑定表命中 ChatToggleMultiline 并切换。
        let mut app = AppState::default();
        assert!(
            !app.handle_modal_shortcuts(&key(KeyCode::Char('m'), KeyModifiers::ALT)),
            "Alt+M 不被全局模态热键吞掉"
        );
        app.handle_key(&key(KeyCode::Char('m'), KeyModifiers::ALT));
        assert!(app.multiline_mode, "Alt+M 开启多行模式");
        app.handle_key(&key(KeyCode::Char('m'), KeyModifiers::ALT));
        assert!(!app.multiline_mode, "再按 Alt+M 关闭多行模式");
    }
}

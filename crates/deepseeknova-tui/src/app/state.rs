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

use super::focus::{CompletionState, Focus, HelpOverlay, SidebarTab};
use crate::i18n::{Key, Tr};
use crate::input::editor::InputState;
use crate::model::apply::ConversationApply;
use crate::model::conversation::{Conversation, SegId};

/// 回显通道的行上限（命令反馈滚动，防无界增长）。
const MAX_ECHO: usize = 500;

/// 临时命令反馈（状态变更类命令）在状态行上方的存活时长。
pub const NOTICE_TTL: std::time::Duration = std::time::Duration::from_secs(6);

/// 对话面板显示模式（`/raw` 循环切换）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DisplayMode {
    #[default]
    Normal,
    Lite,
    Raw,
}

pub fn display_mode_label(mode: DisplayMode) -> Key {
    match mode {
        DisplayMode::Normal => Key::DisplayModeNormal,
        DisplayMode::Lite => Key::DisplayModeLite,
        DisplayMode::Raw => Key::DisplayModeRaw,
    }
}

/// 权限模式预设的用户可见标签键（状态栏/浮层指示）。
pub fn permission_mode_label(mode: Option<deepseeknova_permission::PermissionMode>) -> Key {
    use deepseeknova_permission::PermissionMode::*;
    match mode {
        None => Key::PermModeLegacy,
        Some(Plan) => Key::PermModePlan,
        Some(AcceptEdits) => Key::PermModeAcceptEdits,
        Some(Auto) => Key::PermModeAuto,
    }
}

/// 权限模式循环顺序（Ctrl+P / `/mode cycle`）：
/// default(None) → plan → accept_edits → auto → plan → …
pub fn next_permission_mode(
    current: Option<deepseeknova_permission::PermissionMode>,
) -> deepseeknova_permission::PermissionMode {
    use deepseeknova_permission::PermissionMode::*;
    match current {
        None => Plan,
        Some(Plan) => AcceptEdits,
        Some(AcceptEdits) => Auto,
        Some(Auto) => Plan,
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
    /// 已保存会话（含首句预览，供侧边栏区分会话），最新优先。
    async fn list_sessions(&self) -> anyhow::Result<Vec<SessionMeta>>;
    async fn current_session(&self) -> Option<String>;
    /// 重命名会话 title（`/rename <title>` 作用于当前会话）。
    async fn rename(&self, id: &str, title: &str) -> anyhow::Result<()>;
    async fn resume(&self, id: &str) -> anyhow::Result<Vec<ResumedLine>>;
    async fn record_turn(
        &self,
        prompt: &str,
        output_text: &str,
        model: Option<String>,
    ) -> anyhow::Result<()>;
}

/// 侧边栏会话列表的一条：id + 首句预览 + 用户命名 title。
/// 渲染时 title 优先；无 title 回退 id（preview 空时）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMeta {
    pub id: String,
    pub preview: String,
    /// 用户命名（`/rename`）；`None` 表示未命名，回退显示 id。
    pub title: Option<String>,
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

/// 会话级检查点控制器（`/checkpoint save|list|rollback`）。
///
/// 由 CLI 用 deepseeknova-checkpoint 的
/// [`SessionCheckpointManager`](deepseeknova_checkpoint::SessionCheckpointManager)
/// 实现。
/// 快照内容为对话行（用户/助手/系统），回退时除返回对话给 TUI 恢复显示外，
/// 实现方还应同步重写 agent 共享 history，使模型上下文与恢复后显示一致。
#[async_trait]
pub trait SessionCheckpointController: Send + Sync {
    /// 保存一个会话检查点（快照当前对话 + 可选标签），返回检查点 id。
    async fn save(
        &self,
        label: Option<String>,
        conversation: Vec<deepseeknova_checkpoint::ConversationLine>,
    ) -> anyhow::Result<String>;
    /// 检查点列表（最新优先），每行含 id/时间/消息数（渲染展示用）。
    async fn list(&self) -> anyhow::Result<Vec<String>>;
    /// 按 id（或最新，`None`）回退：恢复文件快照并返回检查点内容；
    /// 未知 id / 无检查点返回 `Ok(None)`。
    async fn rollback(
        &self,
        id: Option<&str>,
    ) -> anyhow::Result<Option<deepseeknova_checkpoint::SessionCheckpoint>>;
}

/// 工作区信任控制器（由 CLI 用 config 的 `TrustStore` 实现）。
///
/// 首进带权限规则的项目时 TUI 弹信任确认浮层：`trust` 把工作区根写入
/// `~/.deepseeknova/trusted.toml` 并解锁项目层 allow 规则；`untrusted` 项目
/// 的项目层 allow 降级为 ask（不能静默放行陌生项目的自配置规则）。
pub trait TrustController: Send + Sync {
    /// 工作区根是否已信任。
    fn is_trusted(&self, root: &std::path::Path) -> bool;
    /// 标记为信任并落盘。
    fn trust(&self, root: &std::path::Path) -> anyhow::Result<()>;
    /// 撤销信任并落盘。
    fn untrust(&self, root: &std::path::Path) -> anyhow::Result<()>;
}

/// 信任确认浮层内容（首次进入带权限规则的项目时展示）。
#[derive(Debug, Clone)]
pub struct TrustPrompt {
    /// 工作区根目录（信任写入目标）。
    pub workspace_root: std::path::PathBuf,
    /// 项目层权限规则条数（展示用）。
    pub rule_count: usize,
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
    /// 界面翻译器（语言由 `DEEPSEEKNOVA_LANG` / `TuiRunner::with_lang` 决定）。
    pub tr: Tr,
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
    /// 最近一次 `/scorecard` 加载的测光评分卡（侧边栏 Cost 面板展示）。
    pub scorecard: Option<crate::model::scorecard::Scorecard>,
    /// 最近一次请求的实际上下文占用 `(used_tokens, window_tokens)`，
    /// 由事件循环每帧从 `usage` + `TuiCaps.context_window` 刷新；无 router
    /// 或未配置 window 时为 None（状态行不显示占用率）。
    pub context_usage: Option<(u64, u64)>,
    /// 临时命令反馈（如 `/fold`、`/raw` 的状态变更提示），渲染在状态行
    /// 上方，超时自动消失；不进入对话面板的永久 echo 通道。
    pub notice: Option<(String, std::time::Instant)>,
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
    /// 鼠标捕获开关：开启时应用消费滚轮滚动对话历史，终端文本无法用
    /// 鼠标选中复制；Ctrl+T 切换。启动时由 TUI 注入为 true（与
    /// EnableMouseCapture 状态一致）。
    pub mouse_capture: bool,
    pub sidebar_tab: SidebarTab,
    pub completion: Option<CompletionState>,
    /// 斜杠命令行内候选（输入 `/` 开头时触发，Claude Code 风格）。
    pub command_hint: Option<crate::commands::CommandHintState>,
    /// /help 帮助浮层：全量帮助文本 + 滚动位置。Esc/q 关闭，j/k 滚动。
    pub help_overlay: Option<HelpOverlay>,
    /// 磁盘保存的会话（最新优先；事件循环经 SessionController 刷新）。
    pub saved_sessions: Vec<SessionMeta>,
    /// 当前会话 id（SessionController 上报；侧边栏“当前”标记用）。
    pub current_session: Option<String>,
    /// 是否已从磁盘加载过会话列表（首次加载前渲染“加载中”占位）。
    pub sessions_loaded: bool,
    /// 侧边栏会话列表当前选中项。
    pub saved_session_selected: usize,
    /// @ 补全候选文件清单（由 CLI 注入；为空则不触发补全）。
    pub at_files: Vec<String>,
    /// `/` 面板选中命令后待执行请求（事件循环用真实 caps 消费）。
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
    /// 当前权限模式预设（每帧从 PermissionGate 刷新；None = 旧行为）。
    /// 状态栏指示 + 审批浮层模式上下文用。
    pub permission_mode: Option<deepseeknova_permission::PermissionMode>,
    /// Ctrl+P 循环切换权限模式的待消费标记（事件循环用真实 gate 消费）。
    pub perm_mode_cycle: bool,
    /// 信任确认浮层（首进带权限规则的项目时展示；None = 无需确认）。
    pub trust_prompt: Option<TrustPrompt>,
    /// 信任确认结果（y=true / n|Esc=false；事件循环用 TrustController 消费）。
    pub trust_decision: Option<bool>,
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

    /// 显示一条临时命令反馈（状态变更类命令用），NOTICE_TTL 后自动消失。
    pub fn show_notice(&mut self, text: impl Into<String>) {
        self.notice = Some((text.into(), std::time::Instant::now()));
    }

    /// 临时反馈是否已超过存活时长（事件循环每帧检查后清除）。
    pub fn notice_expired(&self) -> bool {
        self.notice
            .as_ref()
            .is_some_and(|(_, shown_at)| shown_at.elapsed() >= NOTICE_TTL)
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
                self.tr.t(Key::PressEscAgain),
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
        self.conversation.apply(ev, self.tr);
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

    /// 折叠判断：显式设置优先，默认按智能策略（Claude Code 风格：推理折叠、
    /// 工具调用展开，其余展开）。工具调用默认展开——参数与截断结果直接可见，
    /// 需要整洁视图时用 `/fold all` 或 Enter 折叠单个段。
    pub fn is_folded(&self, seg: SegId, kind: crate::model::conversation::LineKind) -> bool {
        match self.fold.get(&seg) {
            Some(f) => *f,
            None => kind == crate::model::conversation::LineKind::Reasoning,
        }
    }

    /// 切换折叠态；以**有效**折叠态（含默认策略）为基准取反，
    /// 默认值也物化为显式设置（便于重置）。
    pub fn toggle_fold(&mut self, seg: SegId) {
        let folded = self
            .conversation
            .segment_kind(seg)
            .map(|kind| self.is_folded(seg, kind))
            .unwrap_or(false);
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

    /// 当前折叠模式的用户可见摘要键（状态栏指示用）：
    /// 无显式设置 → 默认；全部折叠/展开 → 全折叠/全展开；混合 → 混合。
    pub fn fold_label(&self) -> Key {
        if self.fold.is_empty() {
            return Key::FoldDefault;
        }
        let mut all_folded = true;
        let mut all_open = true;
        for folded in self.fold.values() {
            if *folded {
                all_open = false;
            } else {
                all_folded = false;
            }
        }
        if all_folded {
            Key::FoldAllFolded
        } else if all_open {
            Key::FoldAllOpen
        } else {
            Key::FoldMixed
        }
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
                self.tr.t(Key::NoSelectedMessage),
            );
            return;
        };
        let text = crate::render::message::segment_plain_text(&seg, self.tr);
        // 剪贴板能力探测：本期降级为回显（见 spec「明确不做」与 plan Task 12 回退），
        // 文案明确提示「未复制到剪贴板」，避免用户误以为已复制。
        self.echo_line(
            crate::model::conversation::LineKind::System,
            &self
                .tr
                .t_args(Key::ClipboardUnavailable, &[("text", &text)]),
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
    /// /help 浮层按键：Esc/q 关闭；j/k、↑/↓ 滚动。
    pub fn handle_help_key(&mut self, key: &KeyEvent) -> KeyAction {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.help_overlay = None;
                KeyAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(overlay) = &mut self.help_overlay {
                    let max_scroll = overlay.lines.len().saturating_sub(1);
                    overlay.scroll = (overlay.scroll + 1).min(max_scroll);
                }
                KeyAction::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(overlay) = &mut self.help_overlay {
                    overlay.scroll = overlay.scroll.saturating_sub(1);
                }
                KeyAction::None
            }
            KeyCode::PageDown => {
                if let Some(overlay) = &mut self.help_overlay {
                    let max_scroll = overlay.lines.len().saturating_sub(1);
                    overlay.scroll = (overlay.scroll + 10).min(max_scroll);
                }
                KeyAction::None
            }
            KeyCode::PageUp => {
                if let Some(overlay) = &mut self.help_overlay {
                    overlay.scroll = overlay.scroll.saturating_sub(10);
                }
                KeyAction::None
            }
            _ => KeyAction::None,
        }
    }

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

    /// 导出当前会话为可持久化的会话行（`/checkpoint save` 用）。
    ///
    /// 保真口径与 `/resume` 一致：用户正文、助手正文与推理、系统事件；
    /// 工具调用与验证段省略（恢复时按用户/助手/系统重建回合）。空回合
    /// （无已落段）一并省略。导出结果经
    /// [`SessionCheckpointController::save`] 落盘，回退时用
    /// [`Self::restore_conversation`] 重建。
    pub fn export_conversation_lines(&self) -> Vec<deepseeknova_checkpoint::ConversationLine> {
        use deepseeknova_checkpoint::{ConversationLine, ConversationRole};
        let mut lines = Vec::new();
        let mut seen_turns = std::collections::HashSet::new();
        for (seg_id, seg) in self.conversation.iter_segments() {
            let turn_id = seg_id.0;
            if seen_turns.insert(turn_id) {
                if let Some(text) = self.conversation.user_text_of(turn_id) {
                    lines.push(ConversationLine::new(
                        ConversationRole::User,
                        text.to_string(),
                    ));
                }
            }
            match seg {
                crate::model::conversation::Segment::Text { text }
                | crate::model::conversation::Segment::Reasoning { text } => {
                    lines.push(ConversationLine::new(
                        ConversationRole::Assistant,
                        text.clone(),
                    ));
                }
                crate::model::conversation::Segment::System { text, .. } => {
                    lines.push(ConversationLine::new(
                        ConversationRole::System,
                        text.clone(),
                    ));
                }
                _ => {}
            }
        }
        lines
    }

    /// 把检查点对话行还原为 [`ResumedLine`]（`/checkpoint rollback` 恢复显示用）。
    pub fn resumed_lines_from_checkpoint(
        ck: &deepseeknova_checkpoint::SessionCheckpoint,
    ) -> Vec<ResumedLine> {
        use deepseeknova_checkpoint::ConversationRole;
        ck.conversation
            .iter()
            .map(|l| ResumedLine {
                role: match l.role {
                    ConversationRole::User => ResumedRole::User,
                    ConversationRole::Assistant => ResumedRole::Assistant,
                    ConversationRole::System => ResumedRole::System,
                },
                text: l.text.clone(),
            })
            .collect()
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
    use std::time::Duration;

    #[test]
    fn echo_line_caps_at_max() {
        let mut app = AppState::default();
        for i in 0..(MAX_ECHO + 10) {
            app.echo_line(LineKind::System, &format!("x{i}"));
        }
        assert_eq!(app.echo.len(), MAX_ECHO);
    }

    #[test]
    fn notice_expires_after_ttl() {
        let mut app = AppState::default();
        app.show_notice("临时反馈");
        assert!(app.notice.is_some());
        assert!(!app.notice_expired());
        app.notice = Some((
            "临时反馈".to_string(),
            std::time::Instant::now() - NOTICE_TTL - Duration::from_secs(1),
        ));
        assert!(app.notice_expired(), "超过 TTL 后应判定过期");
    }

    #[test]
    fn help_overlay_scrolls_and_closes() {
        let mut app = AppState::default();
        app.help_overlay = Some(crate::app::focus::HelpOverlay {
            lines: (0..30).map(|i| format!("行 {i}")).collect(),
            scroll: 0,
        });
        // j 滚动下移。
        app.handle_help_key(&KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(app.help_overlay.as_ref().unwrap().scroll, 1);
        // k 上移。
        app.handle_help_key(&KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        assert_eq!(app.help_overlay.as_ref().unwrap().scroll, 0);
        // Esc 关闭。
        app.handle_help_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.help_overlay.is_none(), "Esc 关闭帮助浮层");
        // 关闭后再按不 panic。
        app.handle_help_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    }

    #[test]
    fn fold_label_reflects_state() {
        // 折叠模式标签是 i18n 文案：固定中文断言语言无关行为。
        let mut app = AppState {
            tr: Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        assert_eq!(app.tr.t(app.fold_label()), "默认");

        let id = app.conversation.begin_turn("q".into());
        app.fold.insert((id, 0), true);
        assert_eq!(app.tr.t(app.fold_label()), "全折叠");
        app.fold.clear();
        app.fold.insert((id, 0), false);
        assert_eq!(app.tr.t(app.fold_label()), "全展开");

        app.fold.insert((id, 0), true);
        app.fold.insert((id, 1), false);
        assert_eq!(app.tr.t(app.fold_label()), "混合");
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
        let mut app = AppState {
            tr: Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
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

    #[tokio::test]
    async fn export_conversation_lines_keeps_user_assistant_system() {
        let mut app = AppState::default();
        app.conversation.begin_turn("问题".into());
        app.apply_run_event(RunEvent::ReasoningDelta {
            text: "思考".into(),
            signature: None,
        });
        app.apply_run_event(RunEvent::TextDelta("回答".into()));
        app.apply_run_event(RunEvent::ToolCallStart {
            id: "t1".into(),
            name: "grep".into(),
        });
        app.apply_run_event(RunEvent::Done(done_output("")));
        app.conversation.push_system(
            crate::model::conversation::SystemKind::Info,
            "系统事件".into(),
        );

        use deepseeknova_checkpoint::ConversationRole;
        let lines = app.export_conversation_lines();
        let roles: Vec<ConversationRole> = lines.iter().map(|l| l.role).collect();
        assert_eq!(
            roles,
            vec![
                ConversationRole::User,
                ConversationRole::Assistant,
                ConversationRole::Assistant,
                ConversationRole::System
            ],
            "推理并入助手文本、工具调用省略、系统事件保留: {roles:?}"
        );
        assert_eq!(lines[0].text, "问题");
        assert!(
            lines[1].text.contains("思考"),
            "推理保留: {}",
            lines[1].text
        );
        assert_eq!(lines[2].text, "回答");
        assert_eq!(lines[3].text, "系统事件");

        // 导出 → 检查点落盘 → 回退 → restore 往返：回合数/正文一致。
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = deepseeknova_checkpoint::SessionCheckpointManager::new()
            .with_persistence(dir.path().join("ck.jsonl"));
        let id = mgr.save(lines, Some("阶段一".into())).await.unwrap();
        assert!(id.starts_with("ck-"));
        let popped = mgr
            .rollback(Some(&id))
            .await
            .unwrap()
            .expect("应弹出检查点");

        let mut app2 = AppState::default();
        app2.restore_conversation(AppState::resumed_lines_from_checkpoint(&popped));
        assert_eq!(app2.conversation.turn_count(), 1);
        assert_eq!(app2.conversation.user_text_of(1), Some("问题"));
        let texts: Vec<String> = app2
            .conversation
            .iter_segments()
            .map(|(_, s)| match s {
                crate::model::conversation::Segment::Text { text } => text.clone(),
                crate::model::conversation::Segment::System { text, .. } => format!("[{text}]"),
                _ => String::new(),
            })
            .collect();
        assert_eq!(
            texts,
            vec![
                "思考".to_string(),
                "回答".to_string(),
                "[系统事件]".to_string()
            ],
            "助手正文 + 系统事件均保留"
        );
    }

    // 迁移占位：原 lib.rs 的输入/滚动测试在 input/editor 模块，见 Task 4。

    #[test]
    fn permission_mode_cycle_order() {
        use deepseeknova_permission::PermissionMode::*;
        assert_eq!(next_permission_mode(None), Plan, "default → plan");
        assert_eq!(next_permission_mode(Some(Plan)), AcceptEdits);
        assert_eq!(next_permission_mode(Some(AcceptEdits)), Auto);
        assert_eq!(next_permission_mode(Some(Auto)), Plan, "auto 回绕到 plan");
    }

    #[test]
    fn permission_mode_label_maps_to_keys() {
        use deepseeknova_permission::PermissionMode::*;
        assert_eq!(permission_mode_label(None), Key::PermModeLegacy);
        assert_eq!(permission_mode_label(Some(Plan)), Key::PermModePlan);
        assert_eq!(
            permission_mode_label(Some(AcceptEdits)),
            Key::PermModeAcceptEdits
        );
        assert_eq!(permission_mode_label(Some(Auto)), Key::PermModeAuto);
        // 模式名中英一致（词表值）。
        let tr = Tr::new(crate::i18n::Lang::En);
        assert_eq!(
            tr.t(permission_mode_label(Some(AcceptEdits))),
            "accept_edits"
        );
        let tr_zh = Tr::new(crate::i18n::Lang::Zh);
        assert_eq!(
            tr_zh.t(permission_mode_label(Some(AcceptEdits))),
            "accept_edits"
        );
    }

    #[test]
    fn trust_prompt_key_handling() {
        let mut app = AppState {
            trust_prompt: Some(TrustPrompt {
                workspace_root: std::path::PathBuf::from("/ws"),
                rule_count: 2,
            }),
            ..Default::default()
        };
        // y → 决策 true，浮层关闭。
        app.handle_key(&KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert_eq!(app.trust_decision, Some(true));
        assert!(app.trust_prompt.is_none());
        // 再次弹出 → n → 决策 false。
        app.trust_prompt = Some(TrustPrompt {
            workspace_root: std::path::PathBuf::from("/ws"),
            rule_count: 2,
        });
        app.handle_key(&KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        assert_eq!(app.trust_decision, Some(false));
        assert!(app.trust_prompt.is_none());
    }

    #[test]
    fn ctrl_p_cycles_perm_mode_via_modal_shortcuts() {
        let mut app = AppState {
            tr: Tr::new(crate::i18n::Lang::Zh),
            ..Default::default()
        };
        let key = |code: KeyCode, modifiers: KeyModifiers| KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        };
        assert!(
            app.handle_modal_shortcuts(&key(KeyCode::Char('p'), KeyModifiers::CONTROL)),
            "Ctrl+P 是全局热键"
        );
        assert!(app.perm_mode_cycle, "置位循环标记");
        // 其余键不影响 perm_mode_cycle。
        app.perm_mode_cycle = false;
        assert!(!app.handle_modal_shortcuts(&key(KeyCode::Char('q'), KeyModifiers::NONE)));
        assert!(!app.perm_mode_cycle);
    }
}

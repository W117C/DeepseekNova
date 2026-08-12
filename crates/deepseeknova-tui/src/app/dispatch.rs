//! Action → Effect 分发器（对齐 grok build 的 dispatch 架构）。
//!
//! 设计要点（对照 xai-grok-pager `app/dispatch/mod.rs`）：
//! - **同步分发**：`dispatch(app, action)` 只做确定性的状态变更，不触碰
//!   终端 / 网络 / 文件系统；
//! - **异步副作用描述为 [`Effect`]**：需要事件循环异步执行的工作（提交
//!   prompt、取消 run、退出、外部编辑器）以 Effect 值返回，由
//!   `app::mod` 的 run_loop 消费执行；
//! - **单一执行点**：focus.rs 的按键路由只做「键 → action」解析，action
//!   的执行全部收敛到本模块，改键/重绑定行为一致（grok 的
//!   `router::dispatch` 同款思路）。
//!
//! 不变量：本模块不持有 AppState 之外的可变全局；纯同步、可单测。

use crate::app::actions::Action;
use crate::app::state::{AppState, KeyAction, TurnView};
use crate::i18n::Key;

/// 事件循环需要异步执行的副作用（描述值，不在本模块执行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// 提交 prompt（含斜杠命令原样上送，由 run_loop 分流）。
    Submit(String),
    /// 取消当前 run。
    Cancel,
    /// 退出 TUI（Esc Esc / 空行 Ctrl+D 等二次确认路径）。
    Quit,
    /// ctrl+x ctrl+e：挂起终端跑 $EDITOR 并读回。
    ///
    /// 当前外部编辑器经 focus.rs 的 ctrl+x 双键序列直发（不落 dispatch），
    /// 此处保留为完整 Effect 集成员（fold_effects 映射 + 架构对齐）；
    /// 未来双键序列收敛到 dispatch 时启用。
    #[allow(dead_code)]
    ExternalEditor,
}

/// 把 dispatch 返回的副作用折叠成旧的 [`KeyAction`]（兼容 focus.rs 接口）。
/// 多副作用时取最后一个（现有语义下每次按键至多一个副作用）。
pub fn fold_effects(effects: Vec<Effect>) -> KeyAction {
    let mut out = KeyAction::None;
    for e in effects {
        match e {
            Effect::Submit(p) => out = KeyAction::Submit(p),
            Effect::Cancel => out = KeyAction::Cancel,
            Effect::Quit => out = KeyAction::Quit,
            Effect::ExternalEditor => out = KeyAction::ExternalEditor,
        }
    }
    out
}

/// 同步分发：按 action 做状态变更，返回需要异步执行的副作用。
///
/// 覆盖 focus.rs / state.rs 中全部语义化动作：输入编辑、对话导航、
/// 侧边栏、模态热键与全局动作。纯同步、无 IO，可直接单测。
pub fn dispatch(app: &mut AppState, action: Action) -> Vec<Effect> {
    use Action::*;
    match action {
        // ── 全局 ─────────────────────────────────────────────
        AppQuit => match app.confirm_quit() {
            KeyAction::Quit => vec![Effect::Quit],
            _ => vec![],
        },
        AppCancel => {
            if app.running {
                vec![Effect::Cancel]
            } else {
                vec![]
            }
        }
        AppToggleSidebar => {
            app.sidebar_visible = !app.sidebar_visible;
            vec![]
        }
        PermModeCycle => {
            // Ctrl+Shift+P：循环权限模式预设（事件循环用真实 gate 消费）。
            app.perm_mode_cycle = true;
            vec![]
        }
        OpenCommandPalette => {
            // Ctrl+P：打开命令面板（全命令候选，最近使用排序）。
            app.command_palette = Some(crate::app::state::CommandPaletteState::open(app));
            vec![]
        }
        ToggleTasks => {
            // Ctrl+G：切换 Tasks 面板（进行中的工具/子代理任务）。
            app.tasks_visible = !app.tasks_visible;
            vec![]
        }
        AppHelp => {
            // F1：复用 /help 命令（pending_command 由事件循环用真实 caps 执行）。
            app.pending_command = Some(("help".to_string(), String::new()));
            vec![]
        }
        AppRedraw => {
            // Ctrl+L：请求清屏重绘（事件循环消费）。
            app.redraw_requested = true;
            vec![]
        }
        ToggleFullscreen => {
            // Ctrl+Shift+F：全屏切换（隐藏状态行/提示行），带临时反馈。
            app.toggle_fullscreen();
            let text = if app.fullscreen {
                app.tr.t(Key::FullscreenOn)
            } else {
                app.tr.t(Key::FullscreenOff)
            };
            app.show_notice(text);
            vec![]
        }
        Redraw => {
            // Ctrl+Shift+R：请求强制全量重绘（事件循环消费）。
            app.redraw_requested = true;
            vec![]
        }
        // ── 输入（Chat context）──────────────────────────────
        ChatSubmit => {
            if app.running {
                return vec![];
            }
            let prompt = std::mem::take(&mut app.input);
            let mut prompt = prompt.text.trim().to_string();
            if prompt.is_empty() {
                return vec![];
            }
            // grok 对齐：bash 模式提交即执行 shell 命令，提交后退出 bash 模式。
            let was_bash = app.bash_mode;
            app.bash_mode = false;
            // 首次提交：退出全屏欢迎屏，回到正常对话布局。
            app.welcome = false;
            if was_bash {
                prompt = prompt.trim_start_matches('!').trim().to_string();
            }
            if prompt.is_empty() {
                return vec![];
            }
            if prompt.starts_with('/') {
                return vec![Effect::Submit(prompt)];
            }
            app.history.push(prompt.clone());
            app.history_idx = None;
            vec![Effect::Submit(prompt)]
        }
        ChatNewline => {
            if !app.running {
                app.input.insert_char('\n');
            }
            vec![]
        }
        ChatClearInput => {
            if !app.running {
                app.input.clear();
                app.refresh_command_hint();
            }
            vec![]
        }
        ChatDeleteWord => {
            if !app.running {
                app.input.delete_word_before();
                app.refresh_command_hint();
            }
            vec![]
        }
        ChatHistoryPrev => {
            if !app.running && app.command_hint.is_none() {
                if app.input.text.contains('\n') {
                    app.input.move_line_up();
                } else {
                    app.history_prev();
                    app.refresh_command_hint();
                }
            }
            vec![]
        }
        ChatHistoryNext => {
            if !app.running && app.command_hint.is_none() {
                if app.input.text.contains('\n') {
                    app.input.move_line_down();
                } else {
                    app.history_next();
                    app.refresh_command_hint();
                }
            }
            vec![]
        }
        ChatMoveLeft => {
            if !app.running {
                app.input.move_left();
            }
            vec![]
        }
        ChatMoveRight => {
            if !app.running {
                app.input.move_right();
            }
            vec![]
        }
        ChatMoveLineUp => {
            if !app.running {
                app.input.move_line_up();
            }
            vec![]
        }
        ChatMoveLineDown => {
            if !app.running {
                app.input.move_line_down();
            }
            vec![]
        }
        ChatHome => {
            if app.running {
                app.scroll_offset = 0;
                app.auto_scroll = false;
            } else {
                app.input.home();
            }
            vec![]
        }
        ChatEnd => {
            if app.running {
                app.auto_scroll = true;
            } else {
                app.input.end();
            }
            vec![]
        }
        ChatHomeLine => {
            if !app.running {
                app.input.home_line();
            }
            vec![]
        }
        ChatEndLine => {
            if !app.running {
                app.input.end_line();
            }
            vec![]
        }
        ChatFocusConversation => {
            // 焦点循环入口：Input（空闲）→ Conversation（消息导航）。
            if !app.running {
                app.focus = crate::app::focus::Focus::Conversation;
            }
            vec![]
        }
        // ── 对话导航（Conversation context，vim 血统）────────
        ConvSelectPrev => {
            app.select_prev();
            vec![]
        }
        ConvSelectNext => {
            app.select_next();
            vec![]
        }
        ConvToggleFold => {
            if let Some(seg) = app.selected {
                app.toggle_fold(seg);
            }
            vec![]
        }
        ConvCopy => {
            app.copy_selected();
            vec![]
        }
        ConvScrollPageUp => {
            // 向上翻页 = 看更早记录（offset 减小）。
            app.scroll_offset = app.scroll_offset.saturating_sub(20);
            app.auto_scroll = false;
            vec![]
        }
        ConvScrollPageDown => {
            app.scroll_offset = app.scroll_offset.saturating_add(20);
            vec![]
        }
        ConvScrollHalfUp => {
            // vim `Ctrl+U`：向上半页（看更早记录，offset 减小）。
            app.scroll_offset = app.scroll_offset.saturating_sub(10);
            app.auto_scroll = false;
            vec![]
        }
        ConvScrollHalfDown => {
            // vim `Ctrl+D`：向下半页（看更新内容，offset 增大）。
            app.scroll_offset = app.scroll_offset.saturating_add(10);
            vec![]
        }
        ConvScrollTop => {
            app.scroll_offset = 0;
            app.auto_scroll = false;
            vec![]
        }
        ConvScrollBottom => {
            app.auto_scroll = true;
            vec![]
        }
        ConvFocusInput => {
            app.focus = crate::app::focus::Focus::Input;
            vec![]
        }
        // ── 侧边栏（Sidebar context）─────────────────────────
        SidebarNextTab => {
            app.sidebar_tab = app.sidebar_tab.next();
            vec![]
        }
        SidebarPrevTab => {
            app.sidebar_tab = app.sidebar_tab.prev();
            vec![]
        }
        SidebarClose => {
            app.sidebar_visible = false;
            app.focus = crate::app::focus::Focus::Input;
            vec![]
        }
        SidebarSelectPrev => {
            if app.sidebar_tab == crate::app::focus::SidebarTab::Sessions
                && !app.saved_sessions.is_empty()
            {
                app.saved_session_selected =
                    (app.saved_session_selected + app.saved_sessions.len() - 1)
                        % app.saved_sessions.len();
            }
            vec![]
        }
        SidebarSelectNext => {
            if app.sidebar_tab == crate::app::focus::SidebarTab::Sessions
                && !app.saved_sessions.is_empty()
            {
                app.saved_session_selected =
                    (app.saved_session_selected + 1) % app.saved_sessions.len();
            }
            vec![]
        }
        SidebarResumeSelected => {
            if app.sidebar_tab == crate::app::focus::SidebarTab::Sessions {
                if let Some(meta) = app.saved_sessions.get(app.saved_session_selected) {
                    // 事件循环用真实 caps 消费 pending_command，与 `/` 同路。
                    app.pending_command = Some(("resume".to_string(), meta.id.clone()));
                    app.focus = crate::app::focus::Focus::Input;
                }
            }
            vec![]
        }
        SidebarWiden => {
            // `[`：加宽一列，并回显当前宽度（26..=60 钳制）。
            app.adjust_sidebar_width(1);
            app.show_notice(app.tr.t_args(
                Key::SidebarWidthNotice,
                &[("n", &app.sidebar_width.to_string())],
            ));
            vec![]
        }
        SidebarNarrow => {
            // `]`：收窄一列，并回显当前宽度（26..=60 钳制）。
            app.adjust_sidebar_width(-1);
            app.show_notice(app.tr.t_args(
                Key::SidebarWidthNotice,
                &[("n", &app.sidebar_width.to_string())],
            ));
            vec![]
        }
        // ── 模态（Completion context）────────────────────────
        // 模态按键（↑↓/Enter/Esc/回退/字符输入）在 at_complete /
        // command_hint 的按键处理器内完成（就地候选，非 dispatch 层），
        // 这里兜底返回无副作用。
        ModalDismiss | ModalSelectPrev | ModalSelectNext | ModalAccept | ModalBackspace
        | ModalTypeChar | ModalArgSubmit | ModalArgCancel => vec![],
        // ── grok 对齐：对话内搜索（Conversation context）─────
        // 字符输入/回退在 focus.rs 的搜索处理器内就地完成（与模态同构），
        // dispatch 只做开关与命中导航。
        ConvSearchOpen => {
            if app.search.is_some() {
                app.search = None;
            } else {
                app.search = Some(Default::default());
            }
            vec![]
        }
        ConvSearchType | ConvSearchBackspace => vec![],
        ConvSearchNext => {
            if let Some(s) = &mut app.search {
                if !s.matches.is_empty() {
                    s.selected = (s.selected + 1).min(s.matches.len() - 1);
                }
            }
            vec![]
        }
        ConvSearchPrev => {
            if let Some(s) = &mut app.search {
                if !s.matches.is_empty() {
                    s.selected = s.selected.saturating_sub(1);
                }
            }
            vec![]
        }
        ConvSearchClose => {
            app.search = None;
            vec![]
        }
        // ── grok 对齐：历史搜索（Chat context，Ctrl+R）────────
        // 字符输入/选择在 focus.rs 的历史搜索处理器内完成；
        // dispatch 只做开关与采纳（采纳把选中项写回输入框）。
        ChatHistorySearchOpen => {
            if app.history_search.is_some() {
                app.history_search = None;
            } else {
                app.history_search = Some(Default::default());
            }
            vec![]
        }
        ChatHistorySearchType | ChatHistorySearchPrev | ChatHistorySearchNext => vec![],
        ChatHistorySearchAccept => {
            // 采纳：把选中 history 项写回输入框（focus.rs 处理器已同步完成）。
            app.history_search = None;
            vec![]
        }
        ChatHistorySearchClose => {
            app.history_search = None;
            vec![]
        }
        // ── grok 对齐：rewind（Esc Esc 空 prompt）─────────────
        // turn 清单构建与回退消费在 focus.rs / 事件循环完成；
        // dispatch 只做开关与选择导航。
        RewindOpen => {
            if app.rewind.is_some() {
                app.rewind = None;
            } else {
                app.rewind = Some(Default::default());
            }
            vec![]
        }
        RewindSelectPrev => {
            if let Some(r) = &mut app.rewind {
                let n = r.turns.len();
                if n > 0 {
                    r.selected = (r.selected + n - 1) % n;
                }
            }
            vec![]
        }
        RewindSelectNext => {
            if let Some(r) = &mut app.rewind {
                let n = r.turns.len();
                if n > 0 {
                    r.selected = (r.selected + 1) % n;
                }
            }
            vec![]
        }
        RewindAccept => {
            // 确认回退：置待消费标记；保留浮层状态供事件循环读取选中项
            // 后统一截断并关闭（事件循环消费 rewind_pending 时完成）。
            app.rewind_pending = true;
            vec![]
        }
        RewindClose => {
            app.rewind = None;
            vec![]
        }
        // ── grok 对齐：vim 双键（g/z 前缀序列）────────────────
        // 双键序列状态在 focus.rs 维护（vim_chord 置位/校验）；
        // dispatch 只执行解析出的动作，占位返回。
        ConvVimLead | ConvVimExec => vec![],
        // ── grok 对齐：turn 导航（h/l、v）────────────────────
        ConvTurnPrev => {
            if app.turn_view == TurnView::Single {
                app.selected_turn = app.selected_turn.map(|t| t.saturating_sub(1)).or(Some(0));
            }
            vec![]
        }
        ConvTurnNext => {
            if app.turn_view == TurnView::Single {
                app.selected_turn = Some(app.selected_turn.map(|t| t + 1).unwrap_or(0));
            }
            vec![]
        }
        ConvToggleTurnView => {
            app.turn_view = match app.turn_view {
                TurnView::All => TurnView::Single,
                TurnView::Single => TurnView::All,
            };
            app.selected_turn = None;
            vec![]
        }
        ConvAnchorPrev | ConvAnchorNext => vec![],
        // ── grok 对齐：多行模式 ───────────────────────────────
        ChatToggleMultiline => {
            app.multiline_mode = !app.multiline_mode;
            app.show_notice(if app.multiline_mode {
                app.tr.t(Key::MultilineOn)
            } else {
                app.tr.t(Key::MultilineOff)
            });
            vec![]
        }
        // ── grok 对齐：统一模态（`?` 快捷键速查）──────────────
        OpenShortcutsHelp => {
            app.active_modal = Some(crate::app::state::ActiveModal::ShortcutsHelp);
            vec![]
        }
        ModalClose => {
            app.active_modal = None;
            vec![]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::focus::Focus;

    fn base_app() -> AppState {
        AppState::default()
    }

    #[test]
    fn submit_returns_effect_and_records_history() {
        let mut app = base_app();
        app.input.insert_str("hello");
        let effects = dispatch(&mut app, Action::ChatSubmit);
        assert_eq!(effects, vec![Effect::Submit("hello".to_string())]);
        assert_eq!(app.history, vec!["hello".to_string()]);
    }

    #[test]
    fn submit_slash_command_passthrough_without_history() {
        let mut app = base_app();
        app.input.insert_str("/help");
        let effects = dispatch(&mut app, Action::ChatSubmit);
        assert_eq!(effects, vec![Effect::Submit("/help".to_string())]);
        assert!(app.history.is_empty(), "斜杠命令不进输入历史");
    }

    #[test]
    fn submit_while_running_is_ignored() {
        let mut app = base_app();
        app.running = true;
        app.input.insert_str("hi");
        let effects = dispatch(&mut app, Action::ChatSubmit);
        assert!(effects.is_empty());
    }

    #[test]
    fn conversation_navigation_mutates_selection() {
        let mut app = base_app();
        let id = app.conversation.begin_turn("q".into());
        app.apply_run_event(deepseeknova_core::runner::RunEvent::TextDelta("a".into()));
        app.apply_run_event(deepseeknova_core::runner::RunEvent::Done(
            crate::model::conversation::done_output(""),
        ));
        app.focus = Focus::Conversation;
        let effects = dispatch(&mut app, Action::ConvSelectNext);
        assert!(effects.is_empty());
        assert_eq!(app.selected, Some((id, 0)));
    }

    #[test]
    fn fullscreen_toggle_flips_flag() {
        let mut app = base_app();
        assert!(!app.fullscreen);
        dispatch(&mut app, Action::ToggleFullscreen);
        assert!(app.fullscreen);
        dispatch(&mut app, Action::ToggleFullscreen);
        assert!(!app.fullscreen);
    }

    #[test]
    fn sidebar_width_clamped_at_bounds() {
        let mut app = base_app();
        for _ in 0..100 {
            dispatch(&mut app, Action::SidebarWiden);
        }
        assert_eq!(app.sidebar_width, 60);
        for _ in 0..100 {
            dispatch(&mut app, Action::SidebarNarrow);
        }
        assert_eq!(app.sidebar_width, 26);
    }

    #[test]
    fn vim_half_scroll_moves_offset_and_disables_follow() {
        let mut app = base_app();
        app.scroll_offset = 30;
        app.auto_scroll = true;
        // Ctrl+U：向上半页（看更早记录，offset 减小，停用自动跟随）。
        dispatch(&mut app, Action::ConvScrollHalfUp);
        assert_eq!(app.scroll_offset, 20);
        assert!(!app.auto_scroll);
        // Ctrl+D：向下半页（offset 增大）。
        dispatch(&mut app, Action::ConvScrollHalfDown);
        assert_eq!(app.scroll_offset, 30);
    }

    #[test]
    fn vim_half_scroll_clamps_at_zero() {
        let mut app = base_app();
        app.scroll_offset = 5;
        dispatch(&mut app, Action::ConvScrollHalfUp);
        assert_eq!(app.scroll_offset, 0, "半页向上不越界");
    }

    #[test]
    fn fold_effects_takes_last() {
        let folded = fold_effects(vec![Effect::Submit("a".into()), Effect::Quit]);
        assert_eq!(folded, KeyAction::Quit);
        let none = fold_effects(vec![]);
        assert_eq!(none, KeyAction::None);
    }
}

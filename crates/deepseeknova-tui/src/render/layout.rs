//! 布局（grok build 对齐）：顶部状态栏 + 弹性 scrollback + 底部 prompt + shortcuts 栏。
//! 与 xai-grok-pager AgentViewLayout 同构：StatusBar(1) → scrollback(Min) → prompt → shortcuts。

use ratatui::layout::Constraint;

use crate::app::state::AppState;

/// 主布局（grok 对齐）：顶部状态栏(1) + 弹性对话区 + 输入区(4) + shortcuts 栏(1)。
/// 状态栏在顶部（xai-grok-pager StatusBar 同款），对话区弹性占满中间，
/// prompt 输入区（3 行输入 + 1 行 info）与快捷键栏在底部。
pub fn layout_constraints() -> [Constraint; 4] {
    [
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(4),
        Constraint::Length(1),
    ]
}

/// 全屏模式布局：隐藏状态栏与 shortcuts 栏（约束为 0），最大化对话区与输入区。
/// 供 WP2/WP4 消费（`Ctrl+Shift+F` 切换，见 `app::state::toggle_fullscreen`）。
pub fn layout_constraints_fullscreen() -> [Constraint; 4] {
    [
        Constraint::Length(0),
        Constraint::Min(0),
        Constraint::Length(4),
        Constraint::Length(0),
    ]
}

/// 按当前状态选择底部面板布局：全屏时隐藏状态行/提示行，否则常规四段。
pub fn layout_constraints_for(app: &AppState) -> [Constraint; 4] {
    if app.fullscreen {
        layout_constraints_fullscreen()
    } else {
        layout_constraints()
    }
}

/// 侧边栏宽度约束：跟随 `app.sidebar_width`（`[`/`]` 调整，26..=60 钳制）。
/// 供 WP2/WP4 消费（替代原先硬编码 `Length(30)` 的切分点）。
pub fn sidebar_constraint(app: &AppState) -> Constraint {
    Constraint::Length(app.sidebar_width)
}

/// 侧边栏可见：显式开启且终端宽度 ≥ 90 列（窄终端自动隐藏）。
pub fn sidebar_visible(width: u16, app: &AppState) -> bool {
    app.sidebar_visible && width >= 90
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_constraints_keep_grok_structure() {
        // grok 对齐：顶部状态栏(1) + 弹性对话区 + 输入区(4，含 info 行) + shortcuts 栏(1)。
        assert_eq!(
            layout_constraints(),
            [
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(4),
                Constraint::Length(1),
            ]
        );
    }

    #[test]
    fn sidebar_hides_on_narrow_terminal() {
        let open = AppState {
            sidebar_visible: true,
            ..Default::default()
        };
        assert!(sidebar_visible(120, &open));
        assert!(!sidebar_visible(89, &open), "窄终端自动隐藏");
        let closed = AppState::default();
        assert!(!sidebar_visible(120, &closed));
    }

    #[test]
    fn fullscreen_layout_returns_fullscreen_constraints() {
        // 全屏：隐藏顶部状态栏与 shortcuts 栏（约束 0），最大化对话区与输入区。
        let app = AppState {
            fullscreen: true,
            ..Default::default()
        };
        assert_eq!(
            layout_constraints_for(&app),
            [
                Constraint::Length(0),
                Constraint::Min(0),
                Constraint::Length(4),
                Constraint::Length(0),
            ]
        );
        assert_eq!(
            layout_constraints_for(&app),
            layout_constraints_fullscreen()
        );
        // 非全屏：常规四段（顶部状态栏 + 对话区 + 输入区 + shortcuts 栏）。
        let normal = AppState::default();
        assert_eq!(layout_constraints_for(&normal), layout_constraints());
    }

    #[test]
    fn sidebar_constraint_follows_app_sidebar_width() {
        // 约束跟随 app.sidebar_width；`[`/`]` 调整在 26..=60 内钳制。
        let mut app = AppState {
            sidebar_width: 40,
            ..Default::default()
        };
        assert_eq!(sidebar_constraint(&app), Constraint::Length(40));
        app.adjust_sidebar_width(5);
        assert_eq!(sidebar_constraint(&app), Constraint::Length(45));
        app.adjust_sidebar_width(100);
        assert_eq!(
            sidebar_constraint(&app),
            Constraint::Length(60),
            "上限 60 钳制"
        );
        app.adjust_sidebar_width(-100);
        assert_eq!(
            sidebar_constraint(&app),
            Constraint::Length(26),
            "下限 26 钳制"
        );
    }

    #[test]
    fn sidebar_default_width_matches_production_initialization() {
        // 审查#3：生产入口（lib.rs run）把 sidebar_width 初始化为 30，替换
        // 旧硬编码 Length(30)。derive Default 会把字段置 0 导致侧边栏零宽，
        // 因此必须断言 30 落在有效钳制范围且约束正确（回归保护）。
        let app = AppState {
            sidebar_width: 30,
            ..Default::default()
        };
        assert_eq!(sidebar_constraint(&app), Constraint::Length(30));
        assert!((26..=60).contains(&app.sidebar_width));
    }
}

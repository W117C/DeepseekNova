//! 布局：纵向 对话区 + 状态行 + 输入区 + 提示行；侧边栏窄终端降级。

use ratatui::layout::Constraint;

use crate::app::state::AppState;

/// 底部面板布局（与旧版一致）：对话区 + 状态行(1) + 输入框(5) + 提示行(1)。
pub fn layout_constraints() -> [Constraint; 4] {
    [
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(5),
        Constraint::Length(1),
    ]
}

/// 侧边栏可见：显式开启且终端宽度 ≥ 90 列（窄终端自动隐藏）。
pub fn sidebar_visible(width: u16, app: &AppState) -> bool {
    app.sidebar_visible && width >= 90
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_constraints_keep_bottom_panel_structure() {
        assert_eq!(
            layout_constraints(),
            [
                Constraint::Min(0),
                Constraint::Length(1),
                Constraint::Length(5),
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
}

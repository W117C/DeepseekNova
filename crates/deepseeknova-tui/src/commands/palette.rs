//! Ctrl+K 命令面板：模糊搜索 + 参数子输入，把选中命令写入 `pending_command`。
//!
//! 面板只负责收集命令名与参数；实际执行由事件循环读取 `app.pending_command`
//! 并用真实 [`crate::commands::TuiCaps`] 构造上下文调用。这样避免面板处理函数与 caps
//! 的签名耦合（`AppState::handle_key` 分派只有 `(app, key)`）。

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{ArgsSpec, CommandRegistry};
use crate::app::focus::{Focus, PaletteState};
use crate::app::state::{AppState, KeyAction};

/// Ctrl+K 面板按键处理（`AppState::handle_key` 分派）。
pub fn handle_key(app: &mut AppState, key: &KeyEvent) -> KeyAction {
    let Some(mut pal) = app.palette.take() else {
        return KeyAction::None;
    };
    // 参数子输入态：输入参数，Enter 提交执行。
    if pal.arg_input.is_some() {
        let (action, close) = handle_arg_input(app, &mut pal, key);
        if close {
            // 已提交执行：关闭面板并把焦点交还输入区。
            app.palette = None;
            app.focus = Focus::Input;
        } else {
            app.palette = Some(pal);
        }
        return action;
    }
    match key.code {
        KeyCode::Esc => {
            app.palette = None;
            app.focus = Focus::Input;
            KeyAction::None
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.palette = None;
            app.focus = Focus::Input;
            KeyAction::None
        }
        KeyCode::Char(c)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            pal.query.push(c);
            pal.selected = 0;
            app.palette = Some(pal);
            KeyAction::None
        }
        KeyCode::Backspace => {
            pal.query.pop();
            pal.selected = 0;
            app.palette = Some(pal);
            KeyAction::None
        }
        KeyCode::Up | KeyCode::Char('k') => {
            let n = CommandRegistry::search(&pal.query).len();
            if n > 0 {
                pal.selected = (pal.selected + n - 1) % n;
            }
            app.palette = Some(pal);
            KeyAction::None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let n = CommandRegistry::search(&pal.query).len();
            if n > 0 {
                pal.selected = (pal.selected + 1) % n;
            }
            app.palette = Some(pal);
            KeyAction::None
        }
        KeyCode::Enter => {
            let candidates = CommandRegistry::search(&pal.query);
            let Some(cmd) = candidates.get(pal.selected.min(candidates.len().saturating_sub(1)))
            else {
                app.palette = None;
                app.focus = Focus::Input;
                return KeyAction::None;
            };
            if cmd.args_spec != ArgsSpec::None {
                // 需要参数的命令：进入参数子输入。
                pal.arg_input = Some(String::new());
                app.palette = Some(pal);
                return KeyAction::None;
            }
            app.pending_command = Some((cmd.name.to_string(), String::new()));
            app.palette = None;
            app.focus = Focus::Input;
            KeyAction::None
        }
        _ => {
            app.palette = Some(pal);
            KeyAction::None
        }
    }
}

/// 参数子输入态按键；Enter 提交参数到 `app.pending_command`。
///
/// 返回 `(KeyAction, close)`：`close=true` 表示已提交执行，调用方应关闭
/// 面板并把焦点交还输入区（与无参命令分支的行为一致）。
fn handle_arg_input(
    app: &mut AppState,
    pal: &mut PaletteState,
    key: &KeyEvent,
) -> (KeyAction, bool) {
    let input = pal.arg_input.as_mut().expect("arg_input set");
    match key.code {
        KeyCode::Esc => {
            pal.arg_input = None;
            (KeyAction::None, false)
        }
        KeyCode::Enter => {
            let args = std::mem::take(input);
            let candidates = CommandRegistry::search(&pal.query);
            let Some(cmd) = candidates.get(pal.selected.min(candidates.len().saturating_sub(1)))
            else {
                pal.arg_input = None;
                return (KeyAction::None, false);
            };
            app.pending_command = Some((cmd.name.to_string(), args));
            pal.arg_input = None;
            pal.query.clear();
            (KeyAction::None, true)
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            input.push(c);
            (KeyAction::None, false)
        }
        KeyCode::Backspace => {
            input.pop();
            (KeyAction::None, false)
        }
        _ => (KeyAction::None, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: crossterm::event::KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    #[test]
    fn palette_typing_filters_and_enter_writes_pending() {
        let mut app = AppState {
            focus: Focus::Palette,
            palette: Some(PaletteState::default()),
            ..Default::default()
        };
        handle_key(&mut app, &key(KeyCode::Char('c')));
        handle_key(&mut app, &key(KeyCode::Char('o')));
        assert_eq!(app.palette.as_ref().unwrap().query, "co");
        handle_key(&mut app, &key(KeyCode::Enter));
        let (name, _) = app.pending_command.as_ref().expect("pending set");
        assert_eq!(name, "cost", "co 命中 cost");
        assert!(app.palette.is_none(), "面板关闭");
        assert_eq!(app.focus, Focus::Input);
    }

    #[test]
    fn palette_esc_closes() {
        let mut app = AppState {
            focus: Focus::Palette,
            palette: Some(PaletteState::default()),
            ..Default::default()
        };
        handle_key(&mut app, &key(KeyCode::Esc));
        assert!(app.palette.is_none());
        assert_eq!(app.focus, Focus::Input);
    }

    #[test]
    fn palette_required_arg_enters_subinput_then_submits() {
        let mut app = AppState {
            focus: Focus::Palette,
            palette: Some(PaletteState {
                query: "model".into(),
                selected: 0,
                arg_input: None,
            }),
            ..Default::default()
        };
        handle_key(&mut app, &key(KeyCode::Enter));
        assert!(
            app.palette.as_ref().unwrap().arg_input.is_some(),
            "model 需要参数，进入子输入"
        );
        for c in "switch deepseek-v4".chars() {
            handle_key(&mut app, &key(KeyCode::Char(c)));
        }
        handle_key(&mut app, &key(KeyCode::Enter));
        let (name, args) = app.pending_command.as_ref().expect("pending set");
        assert_eq!(name, "model");
        assert_eq!(args, "switch deepseek-v4");
        assert!(app.palette.is_none(), "提交后面板关闭");
        assert_eq!(app.focus, Focus::Input, "提交后焦点回输入");
    }
}

//! `@` 文件补全：补全源、词边界解析、纯函数替换，以及 Completion 焦点按键分派。
//!
//! 补全浮层状态（`CompletionState`）在 `app/focus.rs` 定义；本模块只负责
//! 「如何选候选」与「按键如何落盘到输入框」。

use crossterm::event::{KeyCode, KeyEvent};

use crate::app::focus::{CompletionState, Focus};
use crate::app::state::{AppState, KeyAction};

/// `@` 补全候选源（文件路径列表）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AtCompleter {
    /// 候选文件路径（原样保留，匹配后整体作为补全内容）。
    pub files: Vec<String>,
}

impl AtCompleter {
    /// 新建补全器。
    pub fn new(files: Vec<String>) -> Self {
        Self { files }
    }

    /// 子串模糊匹配：按全路径或文件名（末段）做大小写不敏感子串过滤，
    /// 保持原始顺序；空查询返回全部候选。
    pub fn candidates(&self, query: &str) -> Vec<String> {
        if query.is_empty() {
            return self.files.clone();
        }
        let q = query.to_lowercase();
        self.files
            .iter()
            .filter(|f| {
                let name = f.rsplit('/').next().unwrap_or(f.as_str());
                f.to_lowercase().contains(&q) || name.to_lowercase().contains(&q)
            })
            .cloned()
            .collect()
    }

    /// 解析光标前的 `@` 词：返回 `(起始字节, 光标字节, 前缀)`。
    ///
    /// 要求 `@` 与光标之间没有空白（视为同一词）；找不到则返回 None。
    pub fn word_at(text: &str, cursor: usize) -> Option<(usize, usize, String)> {
        let cursor = cursor.min(text.len());
        let before = &text[..cursor];
        let at = before.rfind('@')?;
        if before[at..].contains(char::is_whitespace) {
            return None;
        }
        Some((at, cursor, text[at + 1..cursor].to_string()))
    }

    /// 纯函数：把 `text[start..end]` 替换为 `candidate`，返回新文本。
    ///
    /// 不修改入参；`start`/`end` 会被钳制到合法字符边界区间内。
    pub fn paste_to_at(text: &str, start: usize, end: usize, candidate: &str) -> String {
        let start = start.min(text.len());
        let end = end.clamp(start, text.len());
        let mut out = String::with_capacity(text.len() - (end - start) + candidate.len());
        out.push_str(&text[..start]);
        out.push_str(candidate);
        out.push_str(&text[end..]);
        out
    }
}

/// Completion 焦点按键分派。
///
/// - ↑/k、↓/j：在候选间移动 `selected`（循环）；
/// - Enter/Tab：`take()` 取出浮层，替换输入并回 Input 焦点；
/// - Esc/Backspace：关闭浮层回 Input；
/// - 其余键转 `handle_editor_key` 继续编辑，并**重算 `@` 词**让浮层跟随
///   输入（候选随新前缀刷新；`@` 词被破坏则自动关闭）。
pub fn handle_key(app: &mut AppState, key: &KeyEvent) -> KeyAction {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            if let Some(c) = &mut app.completion {
                let n = c.candidates.len();
                if n > 0 {
                    c.selected = if c.selected == 0 {
                        n - 1
                    } else {
                        c.selected - 1
                    };
                }
            }
            KeyAction::None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if let Some(c) = &mut app.completion {
                let n = c.candidates.len();
                if n > 0 {
                    c.selected = (c.selected + 1) % n;
                }
            }
            KeyAction::None
        }
        KeyCode::Enter | KeyCode::Tab => {
            // 先 take 取出浮层（避免同时可变借用 app），再改输入框
            if let Some(c) = app.completion.take() {
                if let Some(cand) = c.candidates.get(c.selected) {
                    let new_text = AtCompleter::paste_to_at(&app.input.text, c.start, c.end, cand);
                    app.input.set_text(new_text);
                    app.input.cursor = c.start + cand.len();
                }
            }
            app.focus = Focus::Input;
            KeyAction::None
        }
        KeyCode::Esc | KeyCode::Backspace => {
            app.completion = None;
            app.focus = Focus::Input;
            KeyAction::None
        }
        _ => {
            let action = app.handle_editor_key(key);
            refresh_completion(app);
            action
        }
    }
}

/// 编辑后重算 `@` 词：浮层跟随光标与前缀刷新；词被破坏则关闭浮层回 Input。
fn refresh_completion(app: &mut AppState) {
    let Some(old) = app.completion.take() else {
        return;
    };
    if old.candidates.is_empty() {
        // 防御：空候选浮层（pub 字段可被外部构造），直接关闭回 Input。
        app.focus = Focus::Input;
        return;
    }
    let Some((start, end, prefix)) = AtCompleter::word_at(&app.input.text, app.input.cursor) else {
        app.focus = Focus::Input;
        return;
    };
    let previous = old.candidates[old.selected.min(old.candidates.len().saturating_sub(1))].clone();
    let candidates = AtCompleter::new(old.candidates).candidates(&prefix);
    if candidates.is_empty() {
        app.focus = Focus::Input;
        return;
    }
    let selected = candidates.iter().position(|c| *c == previous).unwrap_or(0);
    app.completion = Some(CompletionState {
        start,
        end,
        candidates,
        selected,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::focus::CompletionState;
    use crossterm::event::{KeyEventKind, KeyEventState, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn candidates_substring_match_on_path_and_name() {
        let c = AtCompleter::new(vec![
            "src/main.rs".to_string(),
            "src/lib.rs".to_string(),
            "README.md".to_string(),
        ]);
        assert_eq!(c.candidates(""), c.files, "空查询返回全部");
        assert_eq!(c.candidates("lib"), vec!["src/lib.rs"]);
        assert_eq!(c.candidates("main"), vec!["src/main.rs"], "文件名子串命中");
        assert_eq!(c.candidates("README"), vec!["README.md"]);
        assert_eq!(c.candidates("zzz"), Vec::<String>::new());
        // 大小写不敏感
        assert_eq!(c.candidates("readme"), vec!["README.md"]);
    }

    #[test]
    fn word_at_finds_at_prefix_and_rejects_whitespace() {
        // "hi @fo" ：h0 i1 空格2 @3 f4 o5
        assert_eq!(
            AtCompleter::word_at("hi @fo", 6),
            Some((3, 6, "fo".to_string()))
        );
        // 光标在 @ 前不触发
        assert_eq!(AtCompleter::word_at("hi @fo", 3), None);
        // @ 与光标之间有空白 → 不视为词
        assert_eq!(AtCompleter::word_at("hi @fo bar", 8), None);
        // 多行：光标在下一行不跨行匹配
        assert_eq!(AtCompleter::word_at("@a\nb", 5), None);
        // 空文本
        assert_eq!(AtCompleter::word_at("", 0), None);
    }

    #[test]
    fn paste_to_at_replaces_range_purely() {
        let text = "hi @fo bar";
        assert_eq!(
            AtCompleter::paste_to_at(text, 3, 6, "foo.rs"),
            "hi foo.rs bar"
        );
        assert_eq!(AtCompleter::paste_to_at(text, 3, 6, "x"), "hi x bar");
        assert_eq!(AtCompleter::paste_to_at(text, 3, 6, "abc"), "hi abc bar");
        // 入参不变（纯函数）
        assert_eq!(text, "hi @fo bar");
        // 越界钳制
        assert_eq!(AtCompleter::paste_to_at(text, 100, 200, "Z"), "hi @fo barZ");
        // 中文边界安全：start 在字符边界
        assert_eq!(AtCompleter::paste_to_at("你@好", 3, 4, "x"), "你x好");
    }

    #[test]
    fn completion_keys_move_select_and_apply() {
        let mut app = AppState::default();
        app.input.set_text("hi @fo".into());
        app.input.cursor = 6;
        app.completion = Some(CompletionState {
            start: 3,
            end: 6,
            candidates: vec!["foo.rs".to_string(), "foobar.txt".to_string()],
            selected: 0,
        });
        app.focus = Focus::Completion;

        // ↓ 移到第二个候选
        assert_eq!(app.handle_key(&key(KeyCode::Down)), KeyAction::None);
        assert_eq!(app.completion.as_ref().unwrap().selected, 1);
        // ↑ 回到第一个
        assert_eq!(app.handle_key(&key(KeyCode::Up)), KeyAction::None);
        assert_eq!(app.completion.as_ref().unwrap().selected, 0);
        // ↑ 循环到尾
        assert_eq!(app.handle_key(&key(KeyCode::Up)), KeyAction::None);
        assert_eq!(app.completion.as_ref().unwrap().selected, 1);

        // Enter 应用：替换并回 Input 焦点
        assert_eq!(app.handle_key(&key(KeyCode::Enter)), KeyAction::None);
        assert_eq!(app.input.text, "hi foobar.txt");
        assert_eq!(app.input.cursor, 3 + "foobar.txt".len());
        assert!(app.completion.is_none(), "应用后清浮层");
        assert_eq!(app.focus, Focus::Input);
    }

    #[test]
    fn esc_or_backspace_closes_completion() {
        let mut app = AppState {
            completion: Some(CompletionState {
                start: 0,
                end: 2,
                candidates: vec!["a.rs".to_string()],
                selected: 0,
            }),
            focus: Focus::Completion,
            ..Default::default()
        };
        assert_eq!(app.handle_key(&key(KeyCode::Esc)), KeyAction::None);
        assert!(app.completion.is_none());
        assert_eq!(app.focus, Focus::Input);

        app.completion = Some(CompletionState {
            start: 0,
            end: 2,
            candidates: vec!["a.rs".to_string()],
            selected: 0,
        });
        app.focus = Focus::Completion;
        assert_eq!(app.handle_key(&key(KeyCode::Backspace)), KeyAction::None);
        assert!(app.completion.is_none());
        assert_eq!(app.focus, Focus::Input);
    }

    #[test]
    fn other_keys_edit_and_refresh_completion() {
        let mut app = AppState::default();
        app.input.set_text("hi @fo".into());
        app.input.cursor = 6;
        app.completion = Some(CompletionState {
            start: 3,
            end: 6,
            candidates: vec!["foo.rs".to_string(), "foobar.txt".to_string()],
            selected: 0,
        });
        app.focus = Focus::Completion;
        // 输入 'o' 后前缀变 "foo"，候选刷新为匹配项。
        assert_eq!(app.handle_key(&key(KeyCode::Char('o'))), KeyAction::None);
        assert_eq!(app.input.text, "hi @foo");
        assert_eq!(app.input.cursor, 7);
        let c = app.completion.as_ref().unwrap();
        assert_eq!(c.start, 3, "start 跟随新词边界");
        assert_eq!(c.end, 7, "end 跟随光标");
        assert!(c.candidates.iter().all(|f| f.contains("foo")));
        assert!(app.focus == Focus::Completion, "词仍在，浮层保持");
    }

    #[test]
    fn breaking_at_word_closes_completion() {
        let mut app = AppState::default();
        app.input.set_text("hi @fo".into());
        app.input.cursor = 6;
        app.completion = Some(CompletionState {
            start: 3,
            end: 6,
            candidates: vec!["foo.rs".to_string()],
            selected: 0,
        });
        app.focus = Focus::Completion;
        // 输入空格破坏 @ 词 → 浮层关闭回 Input。
        assert_eq!(app.handle_key(&key(KeyCode::Char(' '))), KeyAction::None);
        assert!(app.completion.is_none());
        assert_eq!(app.focus, Focus::Input);
    }

    #[test]
    fn empty_candidates_completion_does_not_panic() {
        // 防御回归：pub 字段可被外部构造空候选浮层，编辑路径不得 panic。
        let mut app = AppState::default();
        app.input.set_text("hi @fo".into());
        app.input.cursor = 6;
        app.completion = Some(CompletionState {
            start: 3,
            end: 6,
            candidates: vec![],
            selected: 0,
        });
        app.focus = Focus::Completion;
        assert_eq!(app.handle_key(&key(KeyCode::Char('o'))), KeyAction::None);
        assert!(app.completion.is_none(), "空候选浮层关闭");
        assert_eq!(app.focus, Focus::Input);
    }
}

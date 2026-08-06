//! 多行输入编辑器纯逻辑：文本 + 光标状态、单行水平窗口、多行显示视图。
//!
//! 本模块只做状态与纯函数计算，不涉及渲染与按键分派；渲染由 `render/`
//! 消费 `input_view` + `window_slice`，按键分派在 `app/state.rs` 的
//! `handle_editor_key`（保留旧版全部编辑语义）。

use unicode_width::UnicodeWidthChar;

/// 输入框状态：文本 + 光标（字节下标，始终停在 UTF-8 字符边界上）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InputState {
    /// 输入文本。
    pub text: String,
    /// 光标位置（字节下标，位于字符边界）。
    pub cursor: usize,
}

impl InputState {
    /// 整体替换文本并把光标移到末尾。
    pub fn set_text(&mut self, text: String) {
        self.cursor = text.len();
        self.text = text;
    }

    /// 清空文本与光标。
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// 在光标处插入一个字符（UTF-8 安全）。
    pub fn insert_char(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// 删除光标前一个字符（UTF-8 安全）。
    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev = self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
        self.text.replace_range(prev..self.cursor, "");
        self.cursor = prev;
    }

    /// 删除光标后一个字符（UTF-8 安全）。
    pub fn delete(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let next = self.text[self.cursor..]
            .char_indices()
            .nth(1)
            .map(|(i, _)| self.cursor + i)
            .unwrap_or(self.text.len());
        self.text.replace_range(self.cursor..next, "");
    }

    /// 光标左移一个字符（UTF-8 安全）。
    pub fn move_left(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.cursor = self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
    }

    /// 光标右移一个字符（UTF-8 安全）。
    pub fn move_right(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let width = self.text[self.cursor..]
            .chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or(0);
        self.cursor += width;
    }

    /// 光标移到全文开头。
    pub fn home(&mut self) {
        self.cursor = 0;
    }

    /// 光标移到全文末尾。
    pub fn end(&mut self) {
        self.cursor = self.text.len();
    }

    /// Ctrl+W：删除光标前一段词（含光标前的空白），留下词前的分隔空白。
    pub fn delete_word_before(&mut self) {
        let before = &self.text[..self.cursor];
        let end = before.trim_end_matches(char::is_whitespace).len();
        let start = before[..end]
            .rfind(char::is_whitespace)
            .map(|i| i + 1)
            .unwrap_or(0);
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    /// 光标所在行的起止字节区间（不含换行符）。
    pub fn current_line_bounds(&self) -> (usize, usize) {
        let before = &self.text[..self.cursor];
        let start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
        let after = &self.text[self.cursor..];
        let end = self.cursor + after.find('\n').unwrap_or(after.len());
        (start, end)
    }

    /// 光标在当前行的列（字符数）。
    pub fn current_line_col(&self) -> usize {
        let (start, _) = self.current_line_bounds();
        self.text[start..self.cursor].chars().count()
    }

    /// 上一行同列；上一行更短则落在行尾；已在首行不动。
    pub fn move_line_up(&mut self) {
        let (start, _) = self.current_line_bounds();
        if start == 0 {
            return;
        }
        let col = self.current_line_col();
        let prev_start = self.text[..start - 1]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let prev_end = start - 1;
        let mut target = prev_start;
        let mut found = false;
        for (idx, (i, _)) in self.text[prev_start..prev_end].char_indices().enumerate() {
            if idx == col {
                target = prev_start + i;
                found = true;
                break;
            }
        }
        if !found {
            target = prev_end;
        }
        self.cursor = target;
    }

    /// 下一行同列；下一行更短则落在行尾；已在末行不动。
    pub fn move_line_down(&mut self) {
        let (_, end) = self.current_line_bounds();
        if end >= self.text.len() {
            return;
        }
        let col = self.current_line_col();
        let next_start = end + 1;
        let next_end = self.text[next_start..]
            .find('\n')
            .map(|i| next_start + i)
            .unwrap_or(self.text.len());
        let mut target = next_start;
        let mut found = false;
        for (idx, (i, _)) in self.text[next_start..next_end].char_indices().enumerate() {
            if idx == col {
                target = next_start + i;
                found = true;
                break;
            }
        }
        if !found {
            target = next_end;
        }
        self.cursor = target;
    }

    /// 光标移到当前行行首（Home）。
    pub fn home_line(&mut self) {
        let (start, _) = self.current_line_bounds();
        self.cursor = start;
    }

    /// 光标移到当前行行尾（End）。
    pub fn end_line(&mut self) {
        let (_, end) = self.current_line_bounds();
        self.cursor = end;
    }

    /// 可见窗口：让光标始终落在 `width` 内；返回（文本起点, 可见片段）。
    /// 仅测试使用（生产渲染走 `input_view` + `window_slice`）。
    #[cfg(test)]
    fn visible_window(&self, width: usize) -> (usize, &str) {
        window_slice(&self.text, self.cursor, width)
    }

    /// 光标相对 `start` 的显示列（按 Unicode 宽度计，中文算 2 列）。
    #[cfg(test)]
    fn cursor_column(&self, start: usize) -> u16 {
        text_width(&self.text[start..self.cursor]) as u16
    }
}

/// 单行水平窗口：让 `cursor`（行内字节下标）落在 `width`（显示列数）内，
/// UTF-8 边界安全。宽度按 Unicode 显示宽度计（中文等宽字符占 2 列），
/// 不再按字节截断——修复中文输入时光标错位/内容提前截断。
pub fn window_slice(text: &str, cursor: usize, width: usize) -> (usize, &str) {
    let width = width.max(1);
    let cursor = cursor.min(text.len());
    if text_width(text) <= width {
        return (0, text);
    }
    let cursor_col = text_width(&text[..cursor]);
    // 窗口起点列：光标列往前最多 width-1 列，保证光标落在窗口右缘内。
    let want_start_col = cursor_col.saturating_sub(width - 1);
    let start = col_ceil_boundary(text, want_start_col);
    let end = col_end_from(text, start, text_width(&text[..start]) + width);
    (start, &text[start..end])
}

/// 多行输入视图：每行水平窗口 + 纵向跟随光标行。
#[derive(Debug, PartialEq, Eq)]
pub struct InputView {
    /// 每行的可见片段（UTF-8 安全），渲染时按 `scroll_row` 起显示。
    pub rows: Vec<String>,
    /// 光标所在行（绝对行号）。
    pub cursor_row: usize,
    /// 光标在该行的显示列（Unicode 宽度）。
    pub cursor_col: u16,
    /// 首行显示偏移（让光标行始终可见）。
    pub scroll_row: usize,
}

/// 计算多行输入的显示视图；`width` 为每行宽度，`max_rows` 为可见行数。
pub fn input_view(text: &str, cursor: usize, width: usize, max_rows: usize) -> InputView {
    let width = width.max(1);
    let max_rows = max_rows.max(1);
    let lines: Vec<&str> = text.split('\n').collect();
    let cursor = cursor.min(text.len());
    let cursor_row = text[..cursor]
        .matches('\n')
        .count()
        .min(lines.len().saturating_sub(1));
    let line_start: usize = {
        let mut offset = 0;
        for _ in 0..cursor_row {
            offset = text[offset..]
                .find('\n')
                .map(|i| offset + i + 1)
                .unwrap_or(text.len());
        }
        offset
    };
    let cursor_line = &text[line_start..];
    let line_end = cursor_line
        .find('\n')
        .map(|i| line_start + i)
        .unwrap_or(text.len());
    let cursor_in_line = cursor.saturating_sub(line_start).min(line_end - line_start);
    let (win_start, _) = window_slice(cursor_line, cursor_in_line, width);
    let cursor_col = text_width(&cursor_line[win_start..cursor_in_line]) as u16;
    let rows = lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if i == cursor_row {
                window_slice(l, cursor_in_line, width).1.to_string()
            } else {
                let end = col_end_from(l, 0, width);
                l[..end].to_string()
            }
        })
        .collect();
    let scroll_row = if cursor_row < max_rows {
        0
    } else {
        cursor_row - max_rows + 1
    };
    InputView {
        rows,
        cursor_row,
        cursor_col,
        scroll_row,
    }
}

/// 文本的 Unicode 显示宽度（ratatui 同口径：窄字符 1 列、宽字符 2 列、
/// 零宽字符 0 列）。
fn text_width(s: &str) -> usize {
    s.chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum()
}

/// 从 `start`（字节下标，须为字符边界）起累加显示宽度，超过 `max_col` 时
/// 返回截断的字节位置（不切开字符）。
fn col_end_from(text: &str, start: usize, max_col: usize) -> usize {
    let mut w = text_width(&text[..start]);
    for (i, c) in text[start..].char_indices() {
        let cw = UnicodeWidthChar::width(c).unwrap_or(0);
        if w + cw > max_col {
            return start + i;
        }
        w += cw;
    }
    text.len()
}

/// 显示列 `>= col` 的第一个字符边界（字节位置）：`col` 落在宽字符中间时
/// 取该字符之后的边界，保证窗口起点不切开字符。
fn col_ceil_boundary(text: &str, col: usize) -> usize {
    if col == 0 {
        return 0;
    }
    let mut w = 0;
    for (i, c) in text.char_indices() {
        if w >= col {
            return i;
        }
        w += UnicodeWidthChar::width(c).unwrap_or(0);
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_state_insert_and_delete() {
        let mut input = InputState {
            text: "你好".into(),
            cursor: "你好".len(),
        };
        input.end();
        input.insert_char('!');
        assert_eq!(input.text, "你好!");
        assert_eq!(input.cursor, input.text.len());

        input.move_left();
        input.move_left();
        input.delete();
        assert_eq!(input.text, "你!");
        assert_eq!(input.cursor, "你".len());

        input.backspace();
        assert_eq!(input.text, "!");
        assert_eq!(input.cursor, 0);

        input.backspace();
        assert_eq!(input.text, "!");
        input.delete();
        assert_eq!(input.text, "");
    }

    #[test]
    fn input_state_cursor_moves_on_char_boundaries() {
        let mut input = InputState {
            text: "a你b".into(),
            cursor: "a你b".len(),
        };
        input.home();
        input.move_right();
        assert_eq!(input.cursor, 1, "跳过 ASCII 一个字节");
        input.move_right();
        assert_eq!(input.cursor, 1 + "你".len(), "跳过中文整字符");
        input.move_right();
        assert_eq!(input.cursor, input.text.len());
        input.move_right();
        assert_eq!(input.cursor, input.text.len(), "到头不再动");

        input.move_left();
        assert_eq!(input.cursor, 1 + "你".len());
        input.move_left();
        assert_eq!(input.cursor, 1);
        input.move_left();
        assert_eq!(input.cursor, 0);
        input.move_left();
        assert_eq!(input.cursor, 0, "到首不再动");
    }

    #[test]
    fn input_state_word_delete_and_clear() {
        let mut input = InputState {
            text: "hello world".into(),
            cursor: "hello world".len(),
        };
        input.end();
        input.delete_word_before();
        assert_eq!(input.text, "hello ");
        assert_eq!(input.cursor, 6);

        input.delete_word_before();
        assert_eq!(input.text, "");
        assert_eq!(input.cursor, 0);

        input.set_text("abc".into());
        input.clear();
        assert_eq!(input.text, "");
        assert_eq!(input.cursor, 0);
    }

    #[test]
    fn input_state_visible_window_follows_cursor() {
        let mut input = InputState {
            text: "abcdef".into(),
            cursor: 0,
        };
        input.home();
        let (start, visible) = input.visible_window(3);
        assert_eq!((start, visible), (0, "abc"));

        input.end();
        let (start, visible) = input.visible_window(3);
        assert_eq!((start, visible), (4, "ef"), "光标在尾时窗口跟随");
        assert_eq!(input.cursor_column(start), 2);

        let mut input = InputState {
            text: "你a好".into(),
            cursor: "你a好".len(),
        };
        input.end();
        let (start, visible) = input.visible_window(3);
        assert_eq!(visible, "好");
        assert_eq!(input.cursor_column(start), 2, "中文按双列计");

        let mut input = InputState {
            text: "你a好".into(),
            cursor: "你a好".len(),
        };
        input.end();
        let (start, visible) = input.visible_window(10);
        assert_eq!((start, visible), (0, "你a好"));
        assert_eq!(input.cursor_column(start), 5, "你=2 好=2 a=1");
    }

    #[test]
    fn window_slice_uses_display_width_not_bytes() {
        // 中文占 2 列但 3 字节：宽度窗口必须按列切，不能按字节切。
        let (start, visible) = window_slice("你好world", "你好world".len(), 4);
        assert_eq!(visible, "rld", "窗口从第 6 列起（宽 4），第 9 列光标在右缘");
        assert_eq!(start, "你好wo".len(), "起点是字符边界");

        // 光标在中部：窗口让光标落在右缘内（起点列 1）。
        let (start, visible) = window_slice("abcdef", 3, 3);
        assert_eq!((start, visible), (1, "bcd"));

        // 光标后内容不足一窗：窗口以光标为右缘（宽字符占 2 列）。
        let (start, visible) = window_slice("ab你", 5, 3);
        assert_eq!(visible, "你", "ab(2列)+你(2列)=4列 > 3列窗口");
        assert_eq!(start, 2);
    }

    #[test]
    fn window_slice_does_not_split_wide_char() {
        // 目标起点列 1 落在「你」(0..2 列) 中间：窗口从「好」开始，不切开「你」。
        let (start, visible) = window_slice("你好x", 6, 4);
        assert_eq!(visible, "好x", "起点列 2，宽 4 窗口");
        assert_eq!(start, 3, "'你' 占 3 字节");
    }

    #[test]
    fn input_state_multiline_line_moves() {
        let mut input = InputState {
            text: "ab\ncdef\ngh".into(),
            cursor: "ab\ncdef\ngh".len(),
        };
        input.move_line_up();
        assert_eq!(input.cursor, 5, "从 gh 列2 上移到 cdef 列2");
        assert_eq!(&input.text[input.cursor..input.cursor + 2], "ef");
        input.move_line_up();
        assert_eq!(input.cursor, 2, "从 cdef 列2 上移到 ab 列2");
        input.move_line_up();
        assert_eq!(input.cursor, 2, "首行不再上移");
        input.move_line_down();
        assert_eq!(input.cursor, 5, "回到 cdef 列2");
        input.move_line_down();
        assert_eq!(input.cursor, 10, "回到 gh 列2");
        input.move_line_down();
        assert_eq!(input.cursor, 10, "末行不再下移");

        // 上一行更短：同列越界落在行尾
        let mut short = InputState {
            text: "a\nbc".into(),
            cursor: 4,
        };
        short.move_line_up();
        assert_eq!(short.cursor, 1, "上一行只有 1 字符 → 行尾");

        // 下一行更短
        let mut short2 = InputState {
            text: "abc\nd".into(),
            cursor: 3,
        };
        short2.move_line_down();
        assert_eq!(short2.cursor, 5, "下一行只有 1 字符 → 行尾");
    }

    #[test]
    fn home_end_move_within_current_line() {
        let mut input = InputState {
            text: "ab\ncd".into(),
            cursor: 4,
        };
        input.home_line();
        assert_eq!(input.cursor, 3, "Home 到第二行行首");
        input.end_line();
        assert_eq!(input.cursor, 5, "End 到第二行行尾");

        // 单行时 Home/End 与全文一致
        input.set_text("abc".into());
        input.home_line();
        assert_eq!(input.cursor, 0);
        input.end_line();
        assert_eq!(input.cursor, 3);
    }

    #[test]
    fn input_view_follows_cursor_row_col_and_windows() {
        let view = input_view("ab\ncdef", 2, 10, 3);
        assert_eq!(view.cursor_row, 0);
        assert_eq!(view.cursor_col, 2);
        assert_eq!(view.scroll_row, 0);
        assert_eq!(view.rows, vec!["ab".to_string(), "cdef".to_string()]);

        // 纵向跟随：光标在末行、窗口 2 行时滚动到可见区
        let view2 = input_view("a\nb\nc\nd", 7, 10, 2);
        assert_eq!(view2.cursor_row, 3);
        assert_eq!(view2.scroll_row, 2);
        assert_eq!(view2.cursor_col, 1);

        // 横向跟随：宽行窗口让光标可见
        let view3 = input_view("abcdef", 5, 3, 2);
        assert_eq!(view3.rows[0], "def");
        assert_eq!(view3.cursor_col, 2);
        assert_eq!(view3.scroll_row, 0);

        // 非光标行从行首截断
        let view4 = input_view("abcdef\nxy", 8, 3, 2);
        assert_eq!(view4.rows[0], "abc", "非光标行只显示行首窗口");
        assert_eq!(view4.cursor_row, 1);
    }
}

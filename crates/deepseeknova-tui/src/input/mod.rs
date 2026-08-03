//! 输入子系统：多行编辑器纯逻辑、`@` 文件补全、markdown 行级着色。
//!
//! - [`editor`]：`InputState` 文本/光标状态 + 水平窗口 + 多行显示视图；
//! - [`at_complete`]：`@` 补全的候选匹配、词解析与 Completion 焦点按键；
//! - [`md_highlight`]：输入框 markdown 行级着色。

pub mod at_complete;
pub mod editor;
pub mod md_highlight;

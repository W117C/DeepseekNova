//! 消息树模块：会话内容的唯一真相源。
//!
//! - [`conversation`]：Turn / AssistantTurn / Segment 纯数据结构
//! - [`apply`]：RunEvent → 消息树增量构建（推理整段提交，解决乱序）

pub mod apply;
pub mod conversation;

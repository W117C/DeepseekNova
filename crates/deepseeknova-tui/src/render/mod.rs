//! 渲染层：布局、消息卡、侧边栏、输入区、状态行。
//!
//! 渲染 = 消息树渲染 → pending → 命令反馈 echo；全部经 [`crate::theme::Theme`] 取样式，
//! 不散落硬编码颜色。

pub mod approval;
pub mod input;
pub mod layout;
pub mod message;
pub mod sidebar;
pub mod status;
pub mod trust;

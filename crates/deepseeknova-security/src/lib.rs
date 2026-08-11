//! # Security — Access control and audit logging
//!
//! Capability-based tool authorization, path confinement,
//! command/domain allow-lists, resource limits, and structured audit trails.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented,
        clippy::dbg_macro
    )
)]

/// 安全事件审计：结构化审计日志（tracing 输出 + JSONL 落盘）。
pub mod audit;
/// 能力（Capability）：可被门禁控制的特权工具操作。
pub mod capability;
/// 安全上下文：已授予能力、资源限制、安全策略与审计日志器。
pub mod context;
pub mod failure_pattern;
/// 资源限制：工具执行的各项配额（文件数/大小/时长/输出等）。
pub mod limits;
/// 路径规范化与路径限制工具。
pub mod path;
/// 安全策略：路径/命令/域名的放行与拒绝列表评估。
pub mod policy;
pub mod quality;
pub mod readonly;
pub mod sanitize;

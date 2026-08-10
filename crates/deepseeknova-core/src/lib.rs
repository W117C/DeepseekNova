#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
#![warn(missing_docs)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

//! # deepseeknova-core
//!
//! Foundation crate for the deepseeknova agent framework. Provides the core type system,
//! execution abstractions, and registry infrastructure that all other crates build on.
//!
//! ## Key Abstractions
//!
//! - **[`Runner`]** — the central execution trait. Agent, Planner,
//!   Coordinator, and SubAgent all implement it. Produces a stream of
//!   [`RunEvent`]s.
//! - **[`Tool`]** — unified interface for all tools (builtin, MCP, skill).
//!   Each tool declares its schema and executes against JSON arguments.
//! - **[`ExecutionGraph`]** — a DAG of [`ExecutionNode`]s
//!   with retry policies and edge conditions. Used by the planner and graph executor.
//! - **[`RegistryHub`]** — centralized registry for tools,
//!   providers, planners, skills, and commands.
//!
//! ## Example
//!
//! ```rust
//! use deepseeknova_core::{
//!     runner::{RunInput, Runner},
//!     tool::{Tool, ToolContext},
//!     types::ToolSchema,
//!     registry::RegistryHub,
//! };
//! ```

/// 知识产物生成：卡片、风格蒸馏、Wiki/ADR。
pub mod artifacts;
/// 流式输出分片：Chunk / ChunkStream / Usage。
pub mod chunk;
/// 全局错误类型 DeepseeknovaError。
pub mod error;
/// 可持久化执行账本的事件、投影与存储契约。
pub mod execution;
/// 图执行器及其回调 trait。
pub mod executor;
/// 执行图 DAG：节点、动作、边、重试策略。
pub mod graph;
/// 记忆系统：存储、召回、嵌入、证据、生命周期、策略、档案、技能、脱敏。
pub mod memory;
/// 计划器：Planner trait 及其实现。
pub mod planner;
/// 协议层：阶段门控 PhaseGate。
pub mod protocol;
/// 注册表中心：RegistryHub 统一管理 tools/providers/planners/skills/commands。
pub mod registry;
/// 运行器：Runner trait 及 RunEvent / RunInput。
pub mod runner;
/// token 计数与上下文窗口估算。
pub mod tokens;
/// 工具接口：Tool trait 及 ToolContext / ToolSchema。
pub mod tool;
/// 质量钩子：QualityFinding / FindingSeverity / ToolHook trait。
pub mod tool_hook;
/// 基础类型：Message / Role / ToolCall / FunctionCall。
pub mod types;

pub use chunk::*;
pub use error::*;
pub use execution::*;
pub use graph::*;
pub use protocol::*;
pub use registry::*;
pub use runner::*;
pub use tool::*;
pub use tool_hook::*;
pub use types::*;

#![allow(clippy::too_many_arguments, clippy::never_loop)]
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
//! # deepseeknova-agent
//!
//! Agent implementations — the brains of deepseeknova. Each agent type implements
//! [`Runner`](deepseeknova_core::runner::Runner) and can be plugged into the runtime.
//!
//! ## Agent Types
//!
//! - **[`Agent`]** — the main agent loop. Multi-step reasoning with
//!   tool use, memory management, streaming output, and cancellation support.
//! - **[`CoordinatorRunner`]** — two-model coordinator.
//!   Uses a planner model to produce an [`deepseeknova_core::graph::ExecutionGraph`] and an executor model to
//!   run it. Supports sub-agent delegation.
//! - **[`PlanModeRunner`]** — plan-first execution.
//!   The planner analyzes the task in a read-only session, produces a plan, then
//!   the executor carries it out.
//! - **[`SubAgentRunner`]** — lightweight agent for
//!   delegated tasks. Runs in isolation with its own context.
//!
//! ## Memory
//!
//! The [`Memory`] type manages conversation history with automatic
//! compaction. When the context approaches token limits, older messages are summarized
//! using the provider, keeping the working set small.

/// 主 agent：多步推理循环，支持工具调用、记忆、流式输出与取消。
pub mod agent;
mod agent_diag;
pub mod agent_manifest;
mod approval;
pub mod attribution;
/// Prompt/上下文与花费预算（controller 为 token 预算，cost 为美元花费上限）。
pub mod budget;
mod classify;
mod compaction;
/// 双模型协调器：planner 产出执行图，executor 逐节点执行。
pub mod coordinator;
pub mod delegate;
pub mod delegate_tool;
pub mod diagnose;
mod fetch_tool;
/// 对话历史内存管理（自动压缩、结果缓存与收缩）。
pub mod memory;
pub mod memory_distill;
pub mod mention;
mod path;
pub mod phase_runner;
/// 计划优先执行：只读规划、用户审批后执行。
pub mod plan_mode;
pub mod prompts;
pub mod quality;
pub mod recursion;
pub mod reflection;
mod render;
mod review;
/// 轻量子代理 runner：为委派任务提供独立上下文与递归派发。
pub mod sub_agent;
pub mod task_spec;
pub mod test_utils;
pub mod tokens;
mod tools;
mod verify;

pub use agent::*;
pub use agent_manifest::*;
pub use coordinator::*;
pub use delegate::*;
pub use delegate_tool::*;
pub use memory::*;
pub use mention::*;
pub use plan_mode::*;
pub use prompts::{compose_sub_agent_prompt, DEFAULT_SYSTEM_PROMPT};
pub use recursion::*;
pub use sub_agent::*;
pub use task_spec::*;

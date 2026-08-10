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

pub mod agent;
mod agent_diag;
pub mod agent_manifest;
mod approval;
pub mod attribution;
pub mod budget;
mod classify;
mod compaction;
pub mod coordinator;
pub mod delegate;
pub mod delegate_tool;
pub mod diagnose;
mod fetch_tool;
pub mod memory;
pub mod memory_distill;
pub mod mention;
mod path;
pub mod phase_runner;
pub mod plan_mode;
pub mod prompts;
pub mod quality;
pub mod recursion;
pub mod reflection;
mod render;
mod review;
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

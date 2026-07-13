#![allow(dead_code, clippy::too_many_arguments, clippy::never_loop)]
//! # dpronix-agent
//!
//! Agent implementations — the brains of dpronix. Each agent type implements
//! [`Runner`](dpronix_core::runner::Runner) and can be plugged into the runtime.
//!
//! ## Agent Types
//!
//! - **[`Agent`]** — the main agent loop. Multi-step reasoning with
//!   tool use, memory management, streaming output, and cancellation support.
//! - **[`CoordinatorRunner`]** — two-model coordinator.
//!   Uses a planner model to produce an [`dpronix_core::graph::ExecutionGraph`] and an executor model to
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
pub mod budget;
pub mod coordinator;
pub mod memory;
pub mod plan_mode;
pub mod sub_agent;
pub mod test_utils;

pub use agent::*;
pub use coordinator::*;
pub use memory::*;
pub use plan_mode::*;
pub use sub_agent::*;

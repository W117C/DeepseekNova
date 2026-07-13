//! # reasonix-agent
//!
//! Agent implementations — the brains of reasonix. Each agent type implements
//! [`Runner`](reasonix_core::runner::Runner) and can be plugged into the runtime.
//!
//! ## Agent Types
//!
//! - **[`Agent`](agent::Agent)** — the main agent loop. Multi-step reasoning with
//!   tool use, memory management, streaming output, and cancellation support.
//! - **[`CoordinatorRunner`](coordinator::CoordinatorRunner)** — two-model coordinator.
//!   Uses a planner model to produce an [`ExecutionGraph`] and an executor model to
//!   run it. Supports sub-agent delegation.
//! - **[`PlanModeRunner`](plan_mode::PlanModeRunner)** — plan-first execution.
//!   The planner analyzes the task in a read-only session, produces a plan, then
//!   the executor carries it out.
//! - **[`SubAgentRunner`](sub_agent::SubAgentRunner)** — lightweight agent for
//!   delegated tasks. Runs in isolation with its own context.
//!
//! ## Memory
//!
//! The [`Memory`](memory::Memory) type manages conversation history with automatic
//! compaction. When the context approaches token limits, older messages are summarized
//! using the provider, keeping the working set small.

pub mod agent;
pub mod coordinator;
pub mod memory;
pub mod plan_mode;
pub mod sub_agent;
pub mod test_utils;
pub mod budget;

pub use agent::*;
pub use coordinator::*;
pub use memory::*;
pub use plan_mode::*;
pub use sub_agent::*;

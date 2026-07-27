#![allow(
    clippy::len_without_is_empty,
    clippy::too_many_arguments,
    clippy::bool_assert_comparison
)]
//! # deepseeknova-orch
//!
//! Multi-agent orchestration for DeepSeek-V4: Goal-Oriented Action Planning (GOAP),
//! swarm coordination, and agent federation. Inspired by Ruflo's goal planner
//! and swarm system, optimized for DeepSeek-V4's thinking mode and context caching.
//!
//! > **Status: experimental.** This crate is not yet wired into any frontend
//! > (CLI/desktop/serve) — it has no external callers and is exercised only by
//! > its own tests.
//!
//! ## Architecture
//!
//! ```text
//! User Goal
//!    │
//!    ▼
//! GoalPlanner (GOAP: dependency/topological scheduling)
//!    │  └─ decomposes goal → Action DAG
//!    ▼
//! SwarmCoordinator (Queen-led)
//!    ├─ Worker Agent 1 (sub-goal A)
//!    ├─ Worker Agent 2 (sub-goal B)
//!    ├─ Worker Agent 3 (sub-goal C)
//!    └─ Shared Memory (AgentDB / HNSW)
//!    │
//!    ▼
//! Execution → Results → Learning Loop → Memory
//! ```

pub mod memory;
pub mod planner;
pub mod progress;
pub mod swarm;
pub mod types;

pub use memory::*;
pub use planner::*;
pub use progress::*;
pub use swarm::*;
pub use types::*;

//! Prompt/token and cost budgeting for the agent loop.
//!
//! [`crate::budget::controller::PromptBudgetController`] caps the
//! context-window and memory usage per step;
//! [`crate::budget::cost::CostBudget`] caps cumulative USD spend over the
//! session. Either cap triggering pauses the run at the step boundary.

/// Per-step token/context budget decisions.
pub mod controller;
pub mod cost;

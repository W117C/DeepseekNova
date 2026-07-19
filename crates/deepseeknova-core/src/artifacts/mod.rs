//! # Post-Project Artifacts
//!
//! After completing a project, DeepseekNova can optionally generate:
//!
//! - **Repo Wiki** — project knowledge base (ADRs, API docs, dependency graph)
//! - **Knowledge Cards** — structured decision/experience cards
//! - **Memory Distillation** — extract reusable experience into long-term memory
//!
//! These artifacts ensure knowledge doesn't disappear when a session ends.

pub mod cards;
pub mod distill;
pub mod wiki;

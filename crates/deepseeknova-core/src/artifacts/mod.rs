//! # Post-Project Artifacts
//!
//! After completing a project, DeepseekNova can optionally generate:
//!
//! - **Repo Wiki** — project knowledge base (ADRs, API docs, dependency graph)
//! - **Knowledge Cards** — structured decision/experience cards
//! - **Memory Distillation** — extract reusable experience into long-term memory
//!
//! These artifacts ensure knowledge doesn't disappear when a session ends.

/// 知识卡片生成模块。
pub mod cards;
/// 记忆蒸馏模块。
pub mod distill;
/// 仓库 Wiki 生成模块。
pub mod wiki;

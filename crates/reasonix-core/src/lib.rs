//! # reasonix-core
//!
//! Foundation crate for the reasonix agent framework. Provides the core type system,
//! execution abstractions, and registry infrastructure that all other crates build on.
//!
//! ## Key Abstractions
//!
//! - **[`Runner`](runner::Runner)** — the central execution trait. Agent, Planner,
//!   Coordinator, and SubAgent all implement it. Produces a stream of
//!   [`RunEvent`](runner::RunEvent)s.
//! - **[`Tool`](tool::Tool)** — unified interface for all tools (builtin, MCP, skill).
//!   Each tool declares its schema and executes against JSON arguments.
//! - **[`ExecutionGraph`](graph::ExecutionGraph)** — a DAG of [`ExecutionNode`](graph::ExecutionNode)s
//!   with retry policies and edge conditions. Used by the planner and graph executor.
//! - **[`RegistryHub`](registry::RegistryHub)** — centralized registry for tools,
//!   providers, planners, skills, and commands.
//!
//! ## Example
//!
//! ```rust
//! use reasonix_core::{
//!     runner::{RunInput, Runner},
//!     tool::{Tool, ToolContext},
//!     types::ToolSchema,
//!     registry::RegistryHub,
//! };
//! ```

pub mod chunk;
pub mod error;
pub mod executor;
pub mod graph;
pub mod planner;
pub mod registry;
pub mod runner;
pub mod tool;
pub mod types;

pub use chunk::*;
pub use error::*;
pub use graph::*;
pub use registry::*;
pub use runner::*;
pub use tool::*;
pub use types::*;

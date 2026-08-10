//! # Security — Access control and audit logging
//!
//! Capability-based tool authorization, path confinement,
//! command/domain allow-lists, resource limits, and structured audit trails.

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

pub mod audit;
pub mod capability;
pub mod context;
pub mod failure_pattern;
pub mod limits;
pub mod path;
pub mod policy;
pub mod quality;
pub mod readonly;
pub mod sanitize;

//! # Security — Access control and audit logging
//!
//! Capability-based tool authorization, path confinement,
//! command/domain allow-lists, resource limits, and structured audit trails.

pub mod audit;
pub mod capability;
pub mod context;
pub mod limits;
pub mod path;
pub mod policy;

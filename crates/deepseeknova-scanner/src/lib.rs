//! deepseeknova-scanner — deepsec-style security scanning (P1: scan + process).
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
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

pub mod finding;
pub mod investigate;
pub mod report;
pub mod rule;
pub mod scan;

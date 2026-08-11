//! # MCP — Model Context Protocol client
//!
//! Connects to external MCP-compatible tool servers (stdio, legacy SSE, or
//! streamable HTTP) and exposes their tools through the standard DeepseekNova
//! Tool trait. Supports listing and calling tools, resource and prompt access,
//! protocol-version negotiation, and SSE-framed (streamable HTTP) responses.

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

/// Adapter that exposes MCP tools through the DeepseekNova [`Tool`](deepseeknova_core::Tool) trait.
pub mod adapter;
/// High-level MCP client facade built on the connection layer.
pub mod client;
/// stdio-based MCP server connection (JSON-RPC over stdin/stdout).
pub mod connection;
/// Discovery and connection of MCP servers from configuration.
pub mod discovery;
pub mod http_client;
pub mod protocol;
/// MCP protocol data types: JSON-RPC 2.0, initialize, tools, resources, and prompts.
pub mod types;

#[cfg(test)]
mod test_util;

pub use adapter::*;
pub use client::*;
pub use connection::*;
pub use discovery::*;
pub use http_client::*;
pub use types::*;

//! # MCP — Model Context Protocol client
//!
//! Connects to external MCP-compatible tool servers (stdio, legacy SSE, or
//! streamable HTTP) and exposes their tools through the standard DeepseekNova
//! Tool trait. Supports listing and calling tools, resource and prompt access,
//! protocol-version negotiation, and SSE-framed (streamable HTTP) responses.

pub mod adapter;
pub mod client;
pub mod connection;
pub mod discovery;
pub mod http_client;
pub mod protocol;
pub mod types;

#[cfg(test)]
mod test_util;

pub use adapter::*;
pub use client::*;
pub use connection::*;
pub use discovery::*;
pub use http_client::*;
pub use types::*;

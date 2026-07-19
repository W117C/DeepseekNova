//! # MCP — Model Context Protocol client
//!
//! Connects to external MCP-compatible tool servers (stdio or HTTP)
//! and exposes their tools through the standard DeepNova Tool trait.
//! Supports listing, calling, and streaming from MCP servers.

pub mod adapter;
pub mod client;
pub mod connection;
pub mod discovery;
pub mod http_client;
pub mod types;

pub use adapter::*;
pub use client::*;
pub use connection::*;
pub use discovery::*;
pub use http_client::*;
pub use types::*;

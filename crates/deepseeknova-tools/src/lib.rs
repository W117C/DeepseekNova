//! # Tools — Built-in agent tools
//!
//! 13+ built-in tools for file I/O, globbing, grep, shell execution,
//! web fetching, task management, memory operations, and MCP bridging.
//! Each tool implements the `Tool` trait with security-aware execution.

pub mod delegate;
pub mod fs;
pub mod glob;
pub mod graph_tools;
pub mod grep;
pub mod ls;
pub mod memory;
pub mod shell;
pub mod snippet;
pub mod todo;
pub mod web_fetch;

pub use delegate::*;
pub use fs::*;
pub use glob::*;
pub use graph_tools::*;
pub use grep::*;
pub use ls::*;
pub use memory::*;
pub use shell::*;
pub use todo::*;
pub use web_fetch::*;

use deepseeknova_core::Tool;
use deepseeknova_sandbox::{NoOpSandbox, Sandbox};
use std::sync::Arc;

/// Returns all built-in tools ready for registration (shell uses `NoOpSandbox`).
pub fn all_builtin_tools() -> Vec<Arc<dyn Tool>> {
    all_builtin_tools_with_sandbox(Arc::new(NoOpSandbox))
}

/// Returns all built-in tools with the shell tool wired to the given sandbox
/// (macOS Seatbelt / Linux bubblewrap in production, or `NoOpSandbox`).
pub fn all_builtin_tools_with_sandbox(sandbox: Arc<dyn Sandbox>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(ReadFileTool),
        Arc::new(WriteFileTool::new()),
        Arc::new(EditFileTool::new()),
        Arc::new(MoveFileTool::new()),
        Arc::new(LsTool),
        Arc::new(GlobTool),
        Arc::new(GrepTool),
        Arc::new(ShellTool::new(sandbox)),
        Arc::new(TodoWriteTool),
        Arc::new(WebFetchTool),
        Arc::new(RememberTool),
        Arc::new(ForgetTool),
        Arc::new(RecallTool),
        Arc::new(SearchCodeTool),
        Arc::new(TraverseGraphTool),
        Arc::new(RetrieveEntityTool),
        Arc::new(DelegateTool),
    ]
}

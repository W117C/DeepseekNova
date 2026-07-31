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

#[cfg(test)]
mod schema_budget {
    use super::*;

    /// 全量内置工具 schema 序列化后的总字符数上限。schema 属稳定前缀，
    /// 每次缓存 MISS 全额重付——加此上限防止文案慢性膨胀（支柱③）。
    /// 收紧准则：压缩后取实测值 + ~10% 余量。
    const MAX_SCHEMA_CHARS: usize = 5000; // AFTER=4613 × 1.1 ≈ 5074，进位到最近千位

    #[test]
    fn builtin_tool_schemas_stay_within_budget() {
        let tools = all_builtin_tools();
        let total: usize = tools
            .iter()
            .map(|t| {
                let s = t.schema();
                s.name.len()
                    + s.description.len()
                    + serde_json::to_string(&s.parameters)
                        .map(|j| j.len())
                        .unwrap_or(0)
            })
            .sum();
        println!("BUILTIN_SCHEMA_TOTAL_CHARS = {total}");
        assert!(
            total <= MAX_SCHEMA_CHARS,
            "schema total {total} exceeds budget {MAX_SCHEMA_CHARS}"
        );
    }
}

//! # Tools — Built-in agent tools
//!
//! 16 built-in tools for file I/O, globbing, grep, shell execution,
//! web fetching, task management, memory operations, code graph, and Context7
//! docs. Each tool implements the `Tool` trait with security-aware execution.
//! (delegate tool moved to `deepseeknova-agent::DelegateTool`.)

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

pub mod docs_tools;
/// File read/write/edit/move/delete tools (`read_file`, `write_file`, `edit_file`, `move_file`, `delete_file`).
pub mod fs;
/// `ask_user` 工具（向用户提问取回输入；B.5 骨架，交互通道未接线时返回
/// 文档化占位说明）。
pub mod ask_user;
/// Glob pattern file-finding tool (`glob`).
pub mod glob;
pub mod graph_tools;
/// Recursive text-search tool (`grep`).
pub mod grep;
/// Directory-listing tool (`ls`).
pub mod ls;
pub mod lsp;
pub mod memory;
/// Sandboxed shell execution tool (`shell`).
pub mod shell;
pub mod snippet;
/// Structured task-list management tool (`todo_write`).
pub mod todo;
/// URL fetching tool with SSRF protection (`web_fetch`).
pub mod web_fetch;
pub mod web_search;

pub use docs_tools::*;
pub use fs::*;
pub use ask_user::*;
pub use glob::*;
pub use graph_tools::*;
pub use grep::*;
pub use ls::*;
pub use lsp::*;
pub use memory::*;
pub use shell::*;
pub use todo::*;
pub use web_fetch::*;
pub use web_search::*;

use deepseeknova_checkpoint::CheckpointManager;
use deepseeknova_core::Tool;
use deepseeknova_sandbox::{NoOpSandbox, Sandbox};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Returns all built-in tools ready for registration (shell uses `NoOpSandbox`).
pub fn all_builtin_tools() -> Vec<Arc<dyn Tool>> {
    all_builtin_tools_with_sandbox_and_checkpoint(Arc::new(NoOpSandbox), None)
}

/// Returns all built-in tools with the shell tool wired to the given sandbox
/// (macOS Seatbelt / Linux bubblewrap in production, or `NoOpSandbox`).
pub fn all_builtin_tools_with_sandbox(sandbox: Arc<dyn Sandbox>) -> Vec<Arc<dyn Tool>> {
    all_builtin_tools_with_sandbox_and_checkpoint(sandbox, None)
}

/// Returns all built-in tools with the shell tool wired to the given sandbox
/// and (optionally) a shared checkpoint manager for write/edit/move tools.
pub fn all_builtin_tools_with_sandbox_and_checkpoint(
    sandbox: Arc<dyn Sandbox>,
    checkpointer: Option<Arc<Mutex<CheckpointManager>>>,
) -> Vec<Arc<dyn Tool>> {
    let write = match &checkpointer {
        Some(ck) => WriteFileTool::with_checkpointer(Arc::clone(ck)),
        None => WriteFileTool::new(),
    };
    let edit = match &checkpointer {
        Some(ck) => EditFileTool::with_checkpointer(Arc::clone(ck)),
        None => EditFileTool::new(),
    };
    let mv = match &checkpointer {
        Some(ck) => MoveFileTool::with_checkpointer(Arc::clone(ck)),
        None => MoveFileTool::new(),
    };
    let del = match &checkpointer {
        Some(ck) => DeleteFileTool::with_checkpointer(Arc::clone(ck)),
        None => DeleteFileTool::new(),
    };
    vec![
        Arc::new(ReadFileTool),
        Arc::new(write),
        Arc::new(edit),
        Arc::new(mv),
        Arc::new(del),
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
        Arc::new(AskUserTool),
    ]
}

#[cfg(test)]
mod schema_budget {
    use super::*;

    /// 全量内置工具 schema 序列化后的总字符数上限。schema 属稳定前缀，
    /// 每次缓存 MISS 全额重付——加此上限防止文案慢性膨胀（支柱③）。
    /// 收紧准则：压缩后取实测值 + ~10% 余量。
    /// 2026-08-11 上调至 6700：工具描述增强后携带安全语义（read-only 分类、
    /// 沙箱隔离、权限要求、token 上限提示），属必要信息；已先精简冗余枚举
    /// 重复（edge_kinds / view 模式等由 parameters schema 承载）。
    /// (delegate 工具已移至 agent crate，不再计入本预算)
    const MAX_SCHEMA_CHARS: usize = 6700;

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
        eprintln!("BUILTIN_SCHEMA_TOTAL_CHARS = {total}");
        assert!(
            total <= MAX_SCHEMA_CHARS,
            "schema total {total} exceeds budget {MAX_SCHEMA_CHARS}"
        );
    }
}

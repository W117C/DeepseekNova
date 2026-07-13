pub mod fs;
pub mod glob;
pub mod grep;
pub mod ls;
pub mod memory;
pub mod shell;
pub mod snippet;
pub mod todo;
pub mod web_fetch;

pub use fs::*;
pub use glob::*;
pub use grep::*;
pub use ls::*;
pub use memory::*;
pub use shell::*;
pub use todo::*;
pub use web_fetch::*;

use reasonix_core::Tool;
use std::sync::Arc;

/// Returns all built-in tools ready for registration.
pub fn all_builtin_tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(ReadFileTool),
        Arc::new(WriteFileTool::new()),
        Arc::new(EditFileTool::new()),
        Arc::new(MoveFileTool::new()),
        Arc::new(LsTool),
        Arc::new(GlobTool),
        Arc::new(GrepTool),
        Arc::new(ShellTool::default()),
        Arc::new(TodoWriteTool),
        Arc::new(WebFetchTool),
        Arc::new(RememberTool),
        Arc::new(ForgetTool),
        Arc::new(RecallTool),
    ]
}

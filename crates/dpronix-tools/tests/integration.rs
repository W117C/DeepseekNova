//! Integration tests for the built-in tools — run against real temp dirs.
//!
//! These test the actual tool implementations (read_file, write_file, edit_file,
//! grep, glob, ls, move_file) operating on real files in a temp directory.

use dpronix_core::tool::ToolContext;
use dpronix_core::Tool;
use dpronix_tools::*;
use std::path::PathBuf;

use std::sync::Once;

static INIT: Once = Once::new();

fn init_test_workspace() {
    INIT.call_once(|| {
        let temp_dir = std::env::temp_dir().join("dpronix-test-workspace");
        std::fs::create_dir_all(&temp_dir).unwrap();
        let canonical = std::fs::canonicalize(&temp_dir).unwrap_or(temp_dir);
        std::env::set_current_dir(&canonical).unwrap();
    });
}

struct Fixture {
    dir: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        init_test_workspace();
        let cwd = std::env::current_dir().unwrap();
        let unique_name = format!("run-{}", uuid::Uuid::new_v4());
        let dir = cwd.join(unique_name);
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }

    fn write(&self, relative_path: &str, content: &str) -> PathBuf {
        let full = self.dir.join(relative_path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&full, content).unwrap();
        full
    }

    fn path(&self) -> PathBuf {
        self.dir.clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// ---------------------------------------------------------------------------
// ReadFileTool
// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_file_returns_content() {
    let f = Fixture::new();
    f.write("hello.txt", "world");
    let ctx = ToolContext::new("call-1");
    let tool = ReadFileTool;

    let result = tool
        .execute(
            &ctx,
            &format!(r#"{{"path":"{}"}}"#, f.path().join("hello.txt").display()),
        )
        .await
        .unwrap();

    assert!(result.contains("world"), "should contain file content");
    assert!(result.contains("[SNIPPED ID:"), "should include snippet id");
}

#[tokio::test]
async fn read_file_errors_on_missing() {
    let f = Fixture::new();
    let ctx = ToolContext::new("call-1");
    let tool = ReadFileTool;

    let result = tool
        .execute(
            &ctx,
            &format!(r#"{{"path":"{}"}}"#, f.path().join("nope.txt").display()),
        )
        .await;

    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// WriteFileTool
// ---------------------------------------------------------------------------

#[tokio::test]
async fn write_and_read_roundtrips() {
    let f = Fixture::new();
    let ctx = ToolContext::new("call-1");
    let write_tool = WriteFileTool::new();
    let read_tool = ReadFileTool;
    let target = f.path().join("output.txt");

    write_tool
        .execute(
            &ctx,
            &format!(
                r#"{{"path":"{}","content":"hello disk"}}"#,
                target.display()
            ),
        )
        .await
        .unwrap();

    let content = read_tool
        .execute(&ctx, &format!(r#"{{"path":"{}"}}"#, target.display()))
        .await
        .unwrap();
    assert!(
        content.contains("hello disk"),
        "should contain file content"
    );
}

#[tokio::test]
async fn write_creates_parent_dirs() {
    let f = Fixture::new();
    let ctx = ToolContext::new("call-1");
    let tool = WriteFileTool::new();
    let target = f.path().join("a/b/c/deep.txt");

    let result = tool
        .execute(
            &ctx,
            &format!(r#"{{"path":"{}","content":"deep"}}"#, target.display()),
        )
        .await;
    assert!(result.is_ok());
    assert!(target.exists());
}

// ---------------------------------------------------------------------------
// EditFileTool
// ---------------------------------------------------------------------------

#[tokio::test]
async fn edit_file_replaces_text() {
    let f = Fixture::new();
    let path = f.write("greeting.rs", "fn main() {\n    println!(\"hello\");\n}\n");
    let ctx = ToolContext::new("call-1");
    let tool = EditFileTool::new();

    let result = tool
        .execute(
            &ctx,
            &format!(
                r#"{{"path":"{}","search":"println!(\"hello\")","replace":"println!(\"hi\")"}}"#,
                path.display()
            ),
        )
        .await;

    assert!(result.is_ok());
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("println!(\"hi\")"));
    assert!(!content.contains("println!(\"hello\")"));
}

#[tokio::test]
async fn edit_file_errors_when_search_not_found() {
    let f = Fixture::new();
    let path = f.write("config.toml", "[server]\nport = 3000\n");
    let ctx = ToolContext::new("call-1");
    let tool = EditFileTool::new();

    let result = tool
        .execute(
            &ctx,
            &format!(
                r#"{{"path":"{}","search":"not_there","replace":"nope"}}"#,
                path.display()
            ),
        )
        .await;

    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// LsTool
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ls_lists_directory() {
    let f = Fixture::new();
    f.write("a.txt", "a");
    f.write("b.rs", "b");
    f.write("sub/c.md", "c");

    let ctx = ToolContext::new("call-1");
    let tool = LsTool;

    let result = tool
        .execute(&ctx, &format!(r#"{{"path":"{}"}}"#, f.path().display()))
        .await
        .unwrap();

    assert!(result.contains("a.txt"));
    assert!(result.contains("b.rs"));
    assert!(result.contains("sub"));
}

// ---------------------------------------------------------------------------
// GlobTool
// ---------------------------------------------------------------------------

#[tokio::test]
async fn glob_matches_pattern() {
    let f = Fixture::new();
    f.write("src/main.rs", "fn main() {}");
    f.write("src/lib.rs", "pub mod foo;");
    f.write("tests/test.rs", "// test");

    let ctx = ToolContext::new("call-1");
    let tool = GlobTool;

    let result = tool
        .execute(
            &ctx,
            &format!(r#"{{"path":"{}","pattern":"**/*.rs"}}"#, f.path().display()),
        )
        .await
        .unwrap();

    assert!(result.contains("main.rs"));
    assert!(result.contains("lib.rs"));
    assert!(result.contains("test.rs"));
}

// ---------------------------------------------------------------------------
// GrepTool
// ---------------------------------------------------------------------------

#[tokio::test]
async fn grep_finds_matches() {
    let f = Fixture::new();
    let auth_path = f.write(
        "auth.rs",
        "pub fn login() { /* TODO */ }\npub fn logout() {}\n",
    );

    let ctx = ToolContext::new("call-1");
    let tool = GrepTool;

    // Grep the file directly
    let result = tool
        .execute(
            &ctx,
            &format!(r#"{{"pattern":"TODO","path":"{}"}}"#, auth_path.display()),
        )
        .await
        .unwrap();

    assert!(result.contains("TODO"));
}

#[tokio::test]
async fn grep_no_matches_returns_info() {
    let f = Fixture::new();
    let path = f.write("just.rs", "nothing here");

    let ctx = ToolContext::new("call-1");
    let tool = GrepTool;

    let result = tool
        .execute(
            &ctx,
            &format!(r#"{{"pattern":"NONEXISTENT","path":"{}"}}"#, path.display()),
        )
        .await
        .unwrap();

    // Returns either "no matches" or empty
    assert!(result.contains("no matches") || result.is_empty());
}

// ---------------------------------------------------------------------------
// MoveFileTool
// ---------------------------------------------------------------------------

#[tokio::test]
async fn move_file_renames() {
    let f = Fixture::new();
    let src = f.write("old_name.txt", "rename me");
    let dst = f.path().join("new_name.txt");
    let ctx = ToolContext::new("call-1");
    let tool = MoveFileTool::new();

    let result = tool
        .execute(
            &ctx,
            &format!(
                r#"{{"source":"{}","destination":"{}"}}"#,
                src.display(),
                dst.display()
            ),
        )
        .await;

    assert!(result.is_ok());
    assert!(!src.exists());
    assert!(dst.exists());
    assert_eq!(std::fs::read_to_string(&dst).unwrap(), "rename me");
}

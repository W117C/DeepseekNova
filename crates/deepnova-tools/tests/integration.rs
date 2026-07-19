//! Integration tests for the built-in tools — run against real temp dirs.
//!
//! These test the actual tool implementations (read_file, write_file, edit_file,
//! grep, glob, ls, move_file) operating on real files in a temp directory.

use deepnova_core::tool::ToolContext;
use deepnova_core::Tool;
use deepnova_tools::*;
use std::path::PathBuf;

use std::sync::Once;

/// Escape backslashes in a path for safe inclusion in JSON string literals.
/// On Windows, paths use `\` which would break JSON parsing.
fn json_path(path: impl AsRef<std::path::Path>) -> String {
    path.as_ref().display().to_string().replace('\\', "\\\\")
}

/// Build a JSON argument string with a "path" field.
fn path_json(path: impl AsRef<std::path::Path>) -> String {
    format!(r#"{{"path":"{}"}}"#, json_path(path))
}

/// Build a JSON argument string with "path" and an extra field.
fn path_extra_json(path: impl AsRef<std::path::Path>, extra: &str) -> String {
    format!(r#"{{"path":"{}",{}}}"#, json_path(path), extra)
}

static INIT: Once = Once::new();

fn init_test_workspace() {
    INIT.call_once(|| {
        let temp_dir = std::env::temp_dir().join("deepnova-test-workspace");
        std::fs::create_dir_all(&temp_dir).unwrap();
        let canonical = std::fs::canonicalize(&temp_dir).unwrap_or(temp_dir);
        std::env::set_current_dir(&canonical).unwrap();
    });
}

fn test_ctx(workspace: PathBuf) -> ToolContext {
    ToolContext::new("call-1")
        .with_workspace(workspace)
        .with_extension(deepnova_security::context::SecurityContext::with_safe_defaults())
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
    let ctx = test_ctx(f.path());
    let tool = ReadFileTool;

    let result = tool
        .execute(&ctx, &path_json(f.path().join("hello.txt")))
        .await
        .unwrap();

    assert!(result.contains("world"), "should contain file content");
    assert!(result.contains("[SNIPPET ID:"), "should include snippet id");
}

#[tokio::test]
async fn read_file_errors_on_missing() {
    let f = Fixture::new();
    let ctx = test_ctx(f.path());
    let tool = ReadFileTool;

    let result = tool
        .execute(&ctx, &path_json(f.path().join("nope.txt")))
        .await;

    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// WriteFileTool
// ---------------------------------------------------------------------------

#[tokio::test]
async fn write_and_read_roundtrips() {
    let f = Fixture::new();
    let ctx = test_ctx(f.path());
    let write_tool = WriteFileTool::new();
    let read_tool = ReadFileTool;
    let target = f.path().join("output.txt");

    write_tool
        .execute(&ctx, &path_extra_json(&target, r#""content":"hello disk""#))
        .await
        .unwrap();

    let content = read_tool.execute(&ctx, &path_json(&target)).await.unwrap();
    assert!(
        content.contains("hello disk"),
        "should contain file content"
    );
}

#[tokio::test]
async fn write_creates_parent_dirs() {
    let f = Fixture::new();
    let ctx = test_ctx(f.path());
    let tool = WriteFileTool::new();
    let target = f.path().join("a/b/c/deep.txt");

    let result = tool
        .execute(&ctx, &path_extra_json(&target, r#""content":"deep""#))
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
    let ctx = test_ctx(f.path());

    // First, read the file to establish a valid snippet
    let read_tool = crate::ReadFileTool;
    let read_result = read_tool.execute(&ctx, &path_json(&path)).await.unwrap();
    // Extract snippet_id from the read result: ends with "[SNIPPED ID: snip_xxx]"
    let snippet_id = read_result
        .lines()
        .find_map(|l| l.strip_prefix("[SNIPPET ID: "))
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or("snip_missing");

    let tool = EditFileTool::new();
    let result = tool
        .execute(
            &ctx,
            &format!(
                r#"{{"path":"{}","search":"println!(\"hello\")","replace":"println!(\"hi\")","snippet_id":"{}"}}"#,
                json_path(&path),
                snippet_id
            ),
        )
        .await;

    assert!(result.is_ok(), "edit should succeed: {:?}", result);
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("println!(\"hi\")"));
    assert!(!content.contains("println!(\"hello\")"));
}

#[tokio::test]
async fn edit_file_errors_when_search_not_found() {
    let f = Fixture::new();
    let path = f.write("config.toml", "[server]\nport = 3000\n");
    let ctx = test_ctx(f.path());
    let tool = EditFileTool::new();

    let result = tool
        .execute(
            &ctx,
            &format!(
                r#"{{"path":"{}","search":"not_there","replace":"nope"}}"#,
                json_path(&path)
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

    let ctx = test_ctx(f.path());
    let tool = LsTool;

    let result = tool.execute(&ctx, &path_json(f.path())).await.unwrap();

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

    let ctx = test_ctx(f.path());
    let tool = GlobTool;

    let result = tool
        .execute(&ctx, &path_extra_json(f.path(), r#""pattern":"**/*.rs""#))
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

    let ctx = test_ctx(f.path());
    let tool = GrepTool;

    // Grep the file directly
    let result = tool
        .execute(
            &ctx,
            &format!(r#"{{"pattern":"TODO","path":"{}"}}"#, json_path(auth_path)),
        )
        .await
        .unwrap();

    assert!(result.contains("TODO"));
}

#[tokio::test]
async fn grep_no_matches_returns_info() {
    let f = Fixture::new();
    let path = f.write("just.rs", "nothing here");

    let ctx = test_ctx(f.path());
    let tool = GrepTool;

    let result = tool
        .execute(
            &ctx,
            &format!(
                r#"{{"pattern":"NONEXISTENT","path":"{}"}}"#,
                json_path(path)
            ),
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
    let ctx = test_ctx(f.path());
    let tool = MoveFileTool::new();

    let result = tool
        .execute(
            &ctx,
            &format!(
                r#"{{"source":"{}","destination":"{}"}}"#,
                json_path(&src),
                json_path(&dst)
            ),
        )
        .await;

    assert!(result.is_ok());
    assert!(!src.exists());
    assert!(dst.exists());
    assert_eq!(std::fs::read_to_string(&dst).unwrap(), "rename me");
}

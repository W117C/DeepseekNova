use async_trait::async_trait;
use deepseeknova_checkpoint::CheckpointManager;
use deepseeknova_core::{Tool, ToolContext, ToolSchema};
use serde::Deserialize;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

// ---------------------------------------------------------------------------
// ReadFileTool
// ---------------------------------------------------------------------------

pub struct ReadFileTool;

const MAX_READ_SIZE: u64 = 1024 * 1024; // 1 MB

#[derive(Deserialize)]
struct ReadFileArgs {
    path: String,
    #[serde(default)]
    start_line: Option<usize>,
    #[serde(default)]
    end_line: Option<usize>,
}

#[async_trait]
impl Tool for ReadFileTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "read_file".to_string(),
            description: "Reads a file; large: locate, read range, edit.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path."
                    },
                    "start_line": {
                        "type": "integer",
                        "description": "First line (1-based)."
                    },
                    "end_line": {
                        "type": "integer",
                        "description": "Last line (1-based)."
                    }
                },
                "required": ["path"]
            }),
        }
    }

    fn read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        deepseeknova_security::context::enforce_capability(
            ctx,
            deepseeknova_security::capability::Capability::FileRead,
        )?;
        let parsed: ReadFileArgs = serde_json::from_str(args)?;
        let path = sanitize_path(&ctx.workspace_root, &parsed.path)?;

        if ctx.cancellation.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let meta = fs::metadata(&path).await?;
        if meta.len() > MAX_READ_SIZE {
            anyhow::bail!("file too large: {} bytes (max {MAX_READ_SIZE})", meta.len());
        }

        let content = fs::read_to_string(&path).await?;

        // Ranged read (1-based inclusive): only the body returned to the model is
        // sliced; the snippet is still registered on the WHOLE file content so
        // edit_file's snippet validation stays compatible.
        let (display, range_note) = match (parsed.start_line, parsed.end_line) {
            (None, None) => (content.clone(), String::new()),
            (s, e) => {
                let lines: Vec<&str> = content.lines().collect();
                let total = lines.len();
                let start = s.unwrap_or(1).max(1);
                if start > total {
                    anyhow::bail!("start_line {start} exceeds file length ({total} lines)");
                }
                let end = e.unwrap_or(total).min(total); // clamp end to file length (lenient)
                if end < start {
                    anyhow::bail!("end_line {end} is before start_line {start}");
                }
                let slice: String = lines[start - 1..end]
                    .iter()
                    .enumerate()
                    .map(|(i, l)| format!("{}: {}\n", start + i, l)) // line-number prefix
                    .collect();
                (slice, format!("[Lines {start}-{end} of {total}]\n"))
            }
        };

        // Register snippet and append snippet ID for the model to reference
        let mut tracker = crate::snippet::global_tracker().lock().await;
        let snippet_id = tracker.register(&path.to_string_lossy(), &content);
        drop(tracker);

        // Return content with snippet marker for edit validation
        Ok(format!(
            "{}{}\n\n[SNIPPET ID: {}]\n[Snippet generated from: {}]\n",
            range_note,
            display.trim_end(),
            snippet_id,
            path.display()
        ))
    }
}

// ---------------------------------------------------------------------------
// WriteFileTool — atomic write via temp file + rename, with checkpoint support
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct WriteFileTool {
    checkpointer: Option<Arc<Mutex<CheckpointManager>>>,
}

impl WriteFileTool {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach a checkpoint manager. Before writing, the tool snapshots the
    /// target file so it can be rolled back later.
    pub fn with_checkpointer(checkpointer: Arc<Mutex<CheckpointManager>>) -> Self {
        Self {
            checkpointer: Some(checkpointer),
        }
    }
}

#[derive(Deserialize)]
struct WriteFileArgs {
    path: String,
    content: String,
}

#[async_trait]
impl Tool for WriteFileTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "write_file".to_string(),
            description: "Writes a file atomically.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path."
                    },
                    "content": {
                        "type": "string",
                        "description": "Content."
                    }
                },
                "required": ["path", "content"]
            }),
        }
    }

    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        deepseeknova_security::context::enforce_capability(
            ctx,
            deepseeknova_security::capability::Capability::FileWrite,
        )?;
        let parsed: WriteFileArgs = serde_json::from_str(args)?;
        let path = sanitize_path(&ctx.workspace_root, &parsed.path)?;

        if ctx.cancellation.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        // Snapshot before mutation if checkpointer is configured
        if let Some(ref ck) = self.checkpointer {
            ck.lock().await.snapshot_file(&path).await?;
        }

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }

        // Atomic write: write to temp file, then rename
        let tmp_path = path.with_extension(
            path.extension()
                .map(|e| format!("{}.tmp", e.to_string_lossy()))
                .unwrap_or_else(|| "tmp".to_string()),
        );

        let mut tmp = fs::File::create(&tmp_path).await?;
        tmp.write_all(parsed.content.as_bytes()).await?;
        tmp.flush().await?;

        fs::rename(&tmp_path, &path).await?;

        let size = parsed.content.len();
        Ok(format!("wrote {size} bytes to {}", path.display()))
    }
}

// ---------------------------------------------------------------------------
// EditFileTool — SEARCH/REPLACE block exact match, with checkpoint support
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct EditFileTool {
    checkpointer: Option<Arc<Mutex<CheckpointManager>>>,
}

impl EditFileTool {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach a checkpoint manager. Before editing, the tool snapshots the
    /// target file so it can be rolled back later.
    pub fn with_checkpointer(checkpointer: Arc<Mutex<CheckpointManager>>) -> Self {
        Self {
            checkpointer: Some(checkpointer),
        }
    }
}

#[derive(Deserialize)]
struct EditBlock {
    search: String,
    replace: String,
}

#[derive(Deserialize)]
struct EditFileArgs {
    path: String,
    #[serde(default)]
    snippet_id: Option<String>,
    // 单块（向后兼容）
    #[serde(default)]
    search: Option<String>,
    #[serde(default)]
    replace: Option<String>,
    // 多块
    #[serde(default)]
    edits: Vec<EditBlock>,
}

impl EditFileArgs {
    /// 归一为块列表：优先 edits；否则用顶层 search/replace 组一个单块。
    fn blocks(&self) -> anyhow::Result<Vec<EditBlock>> {
        if !self.edits.is_empty() {
            return Ok(self
                .edits
                .iter()
                .map(|b| EditBlock {
                    search: b.search.clone(),
                    replace: b.replace.clone(),
                })
                .collect());
        }
        match (&self.search, &self.replace) {
            (Some(s), Some(r)) => Ok(vec![EditBlock {
                search: s.clone(),
                replace: r.clone(),
            }]),
            _ => anyhow::bail!("provide either `edits: [...]` or both `search` and `replace`"),
        }
    }
}

#[async_trait]
impl Tool for EditFileTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "edit_file".to_string(),
            description: "SEARCH/REPLACE edit; search must match once (0 or >=2 fails); \
                 needs read_file snippet_id."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File." },
                    "edits": {
                        "type": "array",
                        "description": "Blocks or search/replace.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "search": { "type": "string" },
                                "replace": { "type": "string" }
                            },
                            "required": ["search", "replace"]
                        }
                    },
                    "search": { "type": "string", "description": "Search." },
                    "replace": { "type": "string", "description": "Replace." },
                    "snippet_id": { "type": "string", "description": "From read_file." }
                },
                "required": ["path", "snippet_id"]
            }),
        }
    }

    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        deepseeknova_security::context::enforce_capability(
            ctx,
            deepseeknova_security::capability::Capability::FileWrite,
        )?;
        let parsed: EditFileArgs = serde_json::from_str(args)?;
        let path = sanitize_path(&ctx.workspace_root, &parsed.path)?;

        // snippet_id is now required — enforce read-then-edit contract
        let snip_id = parsed.snippet_id.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "snippet_id is required. You MUST call read_file first and pass its snippet_id to edit_file."
            )
        })?;

        if ctx.cancellation.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        // Snapshot before mutation if checkpointer is configured
        if let Some(ref ck) = self.checkpointer {
            ck.lock().await.snapshot_file(&path).await?;
        }

        let original = fs::read_to_string(&path).await?;

        // Validate snippet (mandatory — read-then-edit contract)
        {
            let tracker = crate::snippet::global_tracker().lock().await;
            if let Err(current) = tracker.validate(snip_id, &original) {
                drop(tracker);
                return Ok(format!("SNIPPET STALE: The file has changed since you read it.\nCurrent content:\n---\n{}\n---\nPlease re-read the file first.", current));
            }
            drop(tracker);
        }

        // Attempt search/replace
        let blocks = parsed.blocks()?;

        // 逐块顺序应用：每块必须在当前（已应用前面各块的）工作副本上唯一命中，
        // 0 或 ≥2 处命中 → 整次失败并带块号。任何失败都不写盘，
        // 原子性由末尾单次 tmp+rename 保证。
        let mut working = original.clone();
        for (i, b) in blocks.iter().enumerate() {
            if b.search.is_empty() {
                anyhow::bail!("edit block #{}: search text must not be empty", i + 1);
            }
            let count = working.matches(&b.search).count();
            if count == 0 {
                anyhow::bail!(
                    "edit block #{} not found: search text has 0 matches (must be exactly 1)",
                    i + 1
                );
            }
            if count > 1 {
                anyhow::bail!(
                    "edit block #{} ambiguous: search text has {} matches (must be exactly 1); add surrounding context to disambiguate",
                    i + 1,
                    count
                );
            }
            // 唯一命中：应用（replacen 只替 1 处，等价于唯一替换）
            working = working.replacen(&b.search, &b.replace, 1);
        }

        // 原子写
        let tmp_path = path.with_extension(
            path.extension()
                .map(|e| format!("{}.tmp", e.to_string_lossy()))
                .unwrap_or_else(|| "tmp".to_string()),
        );
        let mut tmp = fs::File::create(&tmp_path).await?;
        tmp.write_all(working.as_bytes()).await?;
        tmp.flush().await?;
        fs::rename(&tmp_path, &path).await?;

        Ok(format!(
            "applied {} edit block(s) to {}",
            blocks.len(),
            path.display()
        ))
    }
}

// ---------------------------------------------------------------------------
// MoveFileTool — rename / move, with checkpoint support
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct MoveFileTool {
    checkpointer: Option<Arc<Mutex<CheckpointManager>>>,
}

impl MoveFileTool {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach a checkpoint manager.
    pub fn with_checkpointer(checkpointer: Arc<Mutex<CheckpointManager>>) -> Self {
        Self {
            checkpointer: Some(checkpointer),
        }
    }
}

#[derive(Deserialize)]
struct MoveFileArgs {
    source: String,
    destination: String,
}

#[async_trait]
impl Tool for MoveFileTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "move_file".to_string(),
            description: "Moves/renames a file.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "source": {
                        "type": "string",
                        "description": "From."
                    },
                    "destination": {
                        "type": "string",
                        "description": "To."
                    }
                },
                "required": ["source", "destination"]
            }),
        }
    }

    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        deepseeknova_security::context::enforce_capability(
            ctx,
            deepseeknova_security::capability::Capability::FileWrite,
        )?;
        let parsed: MoveFileArgs = serde_json::from_str(args)?;
        let src = sanitize_path(&ctx.workspace_root, &parsed.source)?;
        let dst = sanitize_path(&ctx.workspace_root, &parsed.destination)?;

        if ctx.cancellation.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        // Snapshot both source and destination before mutation
        if let Some(ref ck) = self.checkpointer {
            {
                let mut guard = ck.lock().await;
                guard.snapshot_file(&src).await?;
                guard.snapshot_file(&dst).await?;
            }
        }

        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent).await?;
        }

        fs::rename(&src, &dst).await?;
        Ok(format!("moved {} → {}", src.display(), dst.display()))
    }
}

// ---------------------------------------------------------------------------
// Path sanitization
// ---------------------------------------------------------------------------

/// Helper wrapper calling the centralized sanitize_path helper.
fn sanitize_path(workspace: &Path, raw: &str) -> anyhow::Result<PathBuf> {
    deepseeknova_security::path::sanitize_path(workspace, raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx(dir: &Path) -> ToolContext {
        ToolContext::new("call-test")
            .with_workspace(dir.to_path_buf())
            .with_extension(deepseeknova_security::context::SecurityContext::with_safe_defaults())
    }

    async fn seed_edit(dir: &std::path::Path, body: &str) -> (String, ToolContext) {
        tokio::fs::write(dir.join("e.rs"), body).await.unwrap();
        let ctx = test_ctx(dir);
        // 先 read_file 拿 snippet_id（整文件）
        let out = ReadFileTool
            .execute(&ctx, r#"{"path":"e.rs"}"#)
            .await
            .unwrap();
        let sid = out
            .split("[SNIPPET ID: ")
            .nth(1)
            .unwrap()
            .split(']')
            .next()
            .unwrap()
            .to_string();
        (sid, ctx)
    }

    #[tokio::test]
    async fn edit_file_multi_block_all_or_nothing() {
        let dir = std::env::temp_dir().join(format!("dnv-c1-edit-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let (sid, ctx) = seed_edit(&dir, "AAA\nBBB\nCCC\n").await;
        let args = format!(
            r#"{{"path":"e.rs","snippet_id":"{sid}","edits":[{{"search":"AAA","replace":"XXX"}},{{"search":"CCC","replace":"ZZZ"}}]}}"#
        );
        let out = EditFileTool::new().execute(&ctx, &args).await.unwrap();
        assert!(out.contains("2"));
        let after = tokio::fs::read_to_string(dir.join("e.rs")).await.unwrap();
        assert_eq!(after, "XXX\nBBB\nZZZ\n");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn edit_file_ambiguous_match_fails_whole_call() {
        let dir = std::env::temp_dir().join(format!("dnv-c1-editamb-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let (sid, ctx) = seed_edit(&dir, "dup\ndup\nkeep\n").await;
        // 第 1 块唯一 OK，第 2 块 "dup" 命中 2 处 → 整次失败，文件不变
        let args = format!(
            r#"{{"path":"e.rs","snippet_id":"{sid}","edits":[{{"search":"keep","replace":"k2"}},{{"search":"dup","replace":"d2"}}]}}"#
        );
        let res = EditFileTool::new().execute(&ctx, &args).await;
        assert!(res.is_err(), "ambiguous block must fail whole call");
        let after = tokio::fs::read_to_string(dir.join("e.rs")).await.unwrap();
        assert_eq!(after, "dup\ndup\nkeep\n", "no partial edit");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn edit_file_single_block_backcompat_unique() {
        let dir = std::env::temp_dir().join(format!("dnv-c1-edit1-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let (sid, ctx) = seed_edit(&dir, "hello world\n").await;
        let args =
            format!(r#"{{"path":"e.rs","snippet_id":"{sid}","search":"world","replace":"rust"}}"#);
        EditFileTool::new().execute(&ctx, &args).await.unwrap();
        let after = tokio::fs::read_to_string(dir.join("e.rs")).await.unwrap();
        assert_eq!(after, "hello rust\n");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn read_file_ranged_returns_only_slice() {
        let dir = std::env::temp_dir().join(format!("dnv-c1-read-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let dir = tokio::fs::canonicalize(&dir).await.unwrap();
        let f = dir.join("big.txt");
        let body: String = (1..=10).map(|i| format!("line{i}\n")).collect();
        tokio::fs::write(&f, &body).await.unwrap();

        let ctx = test_ctx(&dir);
        let tool = ReadFileTool;
        let args = r#"{"path":"big.txt","start_line":3,"end_line":5}"#;
        let out = tool.execute(&ctx, args).await.unwrap();
        // Only line3..line5 present, not line1/line2/line6
        assert!(out.contains("line3") && out.contains("line5"));
        assert!(!out.contains("line1") && !out.contains("line6"));
        // Snippet marker still present
        assert!(out.contains("[SNIPPET ID:"));
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn read_file_full_still_default() {
        let dir = std::env::temp_dir().join(format!("dnv-c1-readfull-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let dir = tokio::fs::canonicalize(&dir).await.unwrap();
        tokio::fs::write(dir.join("s.txt"), "a\nb\nc\n")
            .await
            .unwrap();
        let ctx = test_ctx(&dir);
        let out = ReadFileTool
            .execute(&ctx, r#"{"path":"s.txt"}"#)
            .await
            .unwrap();
        assert!(out.contains('a') && out.contains('b') && out.contains('c'));
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[test]
    fn test_sanitize_path_traversal() {
        let cwd = std::env::current_dir().unwrap();

        // Non-existent path inside workspace should succeed
        let ok_path = "src/nonexistent_file_xyz.rs";
        let res = sanitize_path(&cwd, ok_path).unwrap();
        assert_eq!(res, cwd.join(ok_path));

        // Path containing .. but staying inside workspace should succeed
        let ok_traversal = "src/../src/nonexistent_file_xyz.rs";
        let res = sanitize_path(&cwd, ok_traversal).unwrap();
        assert_eq!(res, cwd.join("src/nonexistent_file_xyz.rs"));

        // Non-existent path traversing outside workspace should be blocked
        let bad_path = "src/../../outside_workspace_xyz.rs";
        let res = sanitize_path(&cwd, bad_path);
        assert!(
            res.is_err(),
            "Should block path traversal outside workspace: {:?}",
            res
        );
        assert!(res
            .unwrap_err()
            .to_string()
            .contains("escapes workspace root"));
    }
}

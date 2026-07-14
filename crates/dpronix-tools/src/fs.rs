use async_trait::async_trait;
use dpronix_checkpoint::CheckpointManager;
use dpronix_core::{Tool, ToolContext, ToolSchema};
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
}

#[async_trait]
impl Tool for ReadFileTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "read_file".to_string(),
            description: "Reads the contents of a file at the specified path.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute or relative path to the file."
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
        dpronix_security::context::enforce_capability(
            ctx,
            dpronix_security::capability::Capability::FileRead,
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

        // Register snippet and append snippet ID for the model to reference
        let mut tracker = crate::snippet::global_tracker().lock().await;
        let snippet_id = tracker.register(&path.to_string_lossy(), &content);
        drop(tracker);

        // Return content with snippet marker for edit validation
        Ok(format!(
            "{}\n\n[SNIPPED ID: {}]\n[Snippet generated from: {}]\n",
            content.trim_end(),
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
            description: "Writes content to a file atomically (temp file + rename). \
                If a checkpoint manager is configured, the file is snapshotted before writing."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to write."
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write to the file."
                    }
                },
                "required": ["path", "content"]
            }),
        }
    }

    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        dpronix_security::context::enforce_capability(
            ctx,
            dpronix_security::capability::Capability::FileWrite,
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
struct EditFileArgs {
    path: String,
    search: String,
    replace: String,
    #[serde(default)]
    snippet_id: Option<String>,
}

#[async_trait]
impl Tool for EditFileTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "edit_file".to_string(),
            description: "Replaces the first exact match of SEARCH with REPLACE in a file. \
                 SEARCH must match exactly including whitespace and indentation. \
                 If a checkpoint manager is configured, the file is snapshotted before editing."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to edit."
                    },
                    "search": {
                        "type": "string",
                        "description": "Exact text to find."
                    },
                    "replace": {
                        "type": "string",
                        "description": "Text to replace with."
                    },
                    "snippet_id": {
                        "type": "string",
                        "description": "Optional snippet ID from read_file."
                    }
                },
                "required": ["path", "search", "replace"]
            }),
        }
    }

    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        dpronix_security::context::enforce_capability(
            ctx,
            dpronix_security::capability::Capability::FileWrite,
        )?;
        let parsed: EditFileArgs = serde_json::from_str(args)?;
        let path = sanitize_path(&ctx.workspace_root, &parsed.path)?;

        if ctx.cancellation.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        // Snapshot before mutation if checkpointer is configured
        if let Some(ref ck) = self.checkpointer {
            ck.lock().await.snapshot_file(&path).await?;
        }

        let original = fs::read_to_string(&path).await?;

        // Validate snippet if provided (deepcode-cli style)
        if let Some(ref snip_id) = parsed.snippet_id {
            let tracker = crate::snippet::global_tracker().lock().await;
            if let Err(current) = tracker.validate(snip_id, &original) {
                drop(tracker);
                return Ok(format!("SNIPPED STALE: The file has changed since you read it.\n                    Current content:\n---\n{}\n---\nPlease re-read.", current));
            }
            drop(tracker);
        }

        if let Some(pos) = original.find(&parsed.search) {
            let edited = format!(
                "{}{}{}",
                &original[..pos],
                parsed.replace,
                &original[pos + parsed.search.len()..]
            );

            // Atomic write via temp file
            let tmp_path = path.with_extension(
                path.extension()
                    .map(|e| format!("{}.tmp", e.to_string_lossy()))
                    .unwrap_or_else(|| "tmp".to_string()),
            );
            let mut tmp = fs::File::create(&tmp_path).await?;
            tmp.write_all(edited.as_bytes()).await?;
            tmp.flush().await?;
            fs::rename(&tmp_path, &path).await?;

            Ok(format!("replaced 1 occurrence in {}", path.display()))
        } else {
            anyhow::bail!(
                "SEARCH block not found in {}. The exact text must match including whitespace.",
                path.display()
            );
        }
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
            description: "Moves or renames a file from source to destination. \
                If a checkpoint manager is configured, both source and destination \
                are snapshotted before moving."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "source": {
                        "type": "string",
                        "description": "Source path."
                    },
                    "destination": {
                        "type": "string",
                        "description": "Destination path."
                    }
                },
                "required": ["source", "destination"]
            }),
        }
    }

    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        dpronix_security::context::enforce_capability(
            ctx,
            dpronix_security::capability::Capability::FileWrite,
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
    dpronix_security::path::sanitize_path(workspace, raw)
}

#[cfg(test)]
mod tests {
    use super::*;

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

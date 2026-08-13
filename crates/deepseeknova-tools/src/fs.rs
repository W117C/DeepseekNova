use async_trait::async_trait;
use deepseeknova_checkpoint::CheckpointManager;
use deepseeknova_core::{DeepseeknovaError, Tool, ToolContext, ToolSchema};
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

/// Reads a file's contents (optionally a 1-based line range). Returns the
/// contents directly, or an error if the file is missing or exceeds limits.
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
                        "minimum": 1,
                        "description": "First line (1-based)."
                    },
                    "end_line": {
                        "type": "integer",
                        "minimum": 1,
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

    async fn execute(
        &self,
        ctx: &ToolContext,
        args: &str,
    ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
        deepseeknova_security::context::enforce_capability(
            ctx,
            &self.schema().name,
            deepseeknova_security::capability::Capability::FileRead,
        )?;
        let parsed: ReadFileArgs = serde_json::from_str(args)?;
        let path = sanitize_path(&ctx.workspace_root, &parsed.path)?;
        check_policy_path_allowed(ctx, &path)?;

        if ctx.cancellation.is_cancelled() {
            return Err(deepseeknova_core::DeepseeknovaError::Cancelled);
        }

        // 资源限额统一：上限读 SecurityContext 的 `limits.max_file_size`
        // （未装配时回落内置默认 1 MB），与 grep/ls 一致。
        let max_size = ctx
            .extensions
            .get::<deepseeknova_security::context::SecurityContext>()
            .map(|s| s.limits.max_file_size)
            .unwrap_or(MAX_READ_SIZE);
        let meta = fs::metadata(&path).await?;
        if meta.len() > max_size {
            return Err(deepseeknova_core::DeepseeknovaError::tool(format!(
                "file too large: {} bytes (max {max_size})",
                meta.len()
            )));
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
                    return Err(deepseeknova_core::DeepseeknovaError::tool(format!(
                        "start_line {start} exceeds file length ({total} lines)"
                    )));
                }
                let end = e.unwrap_or(total).min(total); // clamp end to file length (lenient)
                if end < start {
                    return Err(deepseeknova_core::DeepseeknovaError::tool(format!(
                        "end_line {end} is before start_line {start}"
                    )));
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

/// Writes a file atomically (temp file + rename), optionally snapshotting the
/// target with a checkpoint manager so the write can be rolled back.
#[derive(Default)]
pub struct WriteFileTool {
    checkpointer: Option<Arc<Mutex<CheckpointManager>>>,
}

impl WriteFileTool {
    /// Create a `WriteFileTool` without checkpoint support.
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
            description: "Writes a file atomically (overwrites existing content). Use this to create new files or replace entire file content. For partial edits use edit_file instead.".to_string(),
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

    async fn execute(
        &self,
        ctx: &ToolContext,
        args: &str,
    ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
        deepseeknova_security::context::enforce_capability(
            ctx,
            &self.schema().name,
            deepseeknova_security::capability::Capability::FileWrite,
        )?;
        let parsed: WriteFileArgs = serde_json::from_str(args)?;
        let path = sanitize_path(&ctx.workspace_root, &parsed.path)?;
        check_policy_path_allowed(ctx, &path)?;

        if ctx.cancellation.is_cancelled() {
            return Err(deepseeknova_core::DeepseeknovaError::Cancelled);
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

        let mut tmp = create_temp_exclusive(&tmp_path).await?;
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

/// Applies SEARCH/REPLACE edit blocks to a file, requiring exact unique
/// matches; optionally snapshots the target with a checkpoint manager.
#[derive(Default)]
pub struct EditFileTool {
    checkpointer: Option<Arc<Mutex<CheckpointManager>>>,
}

impl EditFileTool {
    /// Create an `EditFileTool` without checkpoint support.
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
    fn blocks(&self) -> Result<Vec<EditBlock>, DeepseeknovaError> {
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
            _ => Err(DeepseeknovaError::tool(
                "provide either `edits: [...]` or both `search` and `replace`".to_string(),
            )),
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

    async fn execute(
        &self,
        ctx: &ToolContext,
        args: &str,
    ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
        deepseeknova_security::context::enforce_capability(
            ctx,
            &self.schema().name,
            deepseeknova_security::capability::Capability::FileWrite,
        )?;
        let parsed: EditFileArgs = serde_json::from_str(args)?;
        let path = sanitize_path(&ctx.workspace_root, &parsed.path)?;
        check_policy_path_allowed(ctx, &path)?;

        // snippet_id is now required — enforce read-then-edit contract
        let snip_id = parsed.snippet_id.as_deref().ok_or_else(|| {
            DeepseeknovaError::tool(
                "snippet_id is required. You MUST call read_file first and pass its snippet_id to edit_file."
                    .to_string(),
            )
        })?;

        if ctx.cancellation.is_cancelled() {
            return Err(deepseeknova_core::DeepseeknovaError::Cancelled);
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
                return Err(deepseeknova_core::DeepseeknovaError::tool(format!(
                    "edit block #{}: search text must not be empty",
                    i + 1
                )));
            }
            let count = working.matches(&b.search).count();
            if count == 0 {
                return Err(deepseeknova_core::DeepseeknovaError::tool(format!(
                    "edit block #{} not found: search text has 0 matches (must be exactly 1)",
                    i + 1
                )));
            }
            if count > 1 {
                return Err(deepseeknova_core::DeepseeknovaError::tool(format!(
                    "edit block #{} ambiguous: search text has {} matches (must be exactly 1); add surrounding context to disambiguate",
                    i + 1,
                    count
                )));
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
        let mut tmp = create_temp_exclusive(&tmp_path).await?;
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

/// Renames / moves a file (optionally across directories), snapshotting the
/// target with a checkpoint manager when attached.
#[derive(Default)]
pub struct MoveFileTool {
    checkpointer: Option<Arc<Mutex<CheckpointManager>>>,
}

impl MoveFileTool {
    /// Create a `MoveFileTool` without checkpoint support.
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
            description: "Moves or renames a file. Works across directories. The source path is removed after successful copy to destination.".to_string(),
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

    async fn execute(
        &self,
        ctx: &ToolContext,
        args: &str,
    ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
        deepseeknova_security::context::enforce_capability(
            ctx,
            &self.schema().name,
            deepseeknova_security::capability::Capability::FileWrite,
        )?;
        let parsed: MoveFileArgs = serde_json::from_str(args)?;
        let src = sanitize_path(&ctx.workspace_root, &parsed.source)?;
        let dst = sanitize_path(&ctx.workspace_root, &parsed.destination)?;
        check_policy_path_allowed(ctx, &src)?;
        check_policy_path_allowed(ctx, &dst)?;

        if ctx.cancellation.is_cancelled() {
            return Err(deepseeknova_core::DeepseeknovaError::Cancelled);
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
// DeleteFileTool
// ---------------------------------------------------------------------------

/// Deletes a file within the workspace. Path safety mirrors `move_file`:
/// sanitize + policy allowlist; outside-workspace paths are rejected.
#[derive(Default)]
pub struct DeleteFileTool {
    checkpointer: Option<Arc<Mutex<CheckpointManager>>>,
}

impl DeleteFileTool {
    /// Create a `DeleteFileTool` without checkpoint support.
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach a checkpoint manager (snapshot before deletion).
    pub fn with_checkpointer(checkpointer: Arc<Mutex<CheckpointManager>>) -> Self {
        Self {
            checkpointer: Some(checkpointer),
        }
    }
}

#[derive(Deserialize)]
struct DeleteFileArgs {
    path: String,
}

#[async_trait]
impl Tool for DeleteFileTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "delete_file".to_string(),
            description:
                "Deletes a file inside the workspace. Outside-workspace paths are rejected."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to delete."
                    }
                },
                "required": ["path"]
            }),
        }
    }

    fn read_only(&self) -> bool {
        false
    }

    fn writes_fs(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        ctx: &ToolContext,
        args: &str,
    ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
        deepseeknova_security::context::enforce_capability(
            ctx,
            &self.schema().name,
            deepseeknova_security::capability::Capability::FileWrite,
        )?;
        let parsed: DeleteFileArgs = serde_json::from_str(args)?;
        let path = sanitize_path(&ctx.workspace_root, &parsed.path)?;
        check_policy_path_allowed(ctx, &path)?;

        if ctx.cancellation.is_cancelled() {
            return Err(deepseeknova_core::DeepseeknovaError::Cancelled);
        }

        // Snapshot before deletion so the checkpoint can restore the file.
        if let Some(ref ck) = self.checkpointer {
            {
                let mut guard = ck.lock().await;
                guard.snapshot_file(&path).await?;
            }
        }

        fs::remove_file(&path).await?;
        Ok(format!("deleted {}", path.display()))
    }
}

// ---------------------------------------------------------------------------
// Path sanitization
// ---------------------------------------------------------------------------

/// Helper wrapper calling the centralized sanitize_path helper.
fn sanitize_path(workspace: &Path, raw: &str) -> Result<PathBuf, DeepseeknovaError> {
    deepseeknova_security::path::sanitize_path(workspace, raw)
}

/// 策略路径检查（T2 接线）：解析后的路径须通过
/// [`deepseeknova_security::policy::SecurityPolicy::is_path_allowed`]
/// （denied_paths 优先、allowed_paths
/// 前缀匹配；空列表 = 全放）。未装配
/// [`deepseeknova_security::context::SecurityContext`] 时直接放行，
/// 与 limits 读取同款 fail-open 口径（既有行为不变）。
///
/// `pub(crate)` 供同 crate 的 grep/glob/ls/graph_tools 读路径复用
/// （denied_paths 不能只拦 fs 工具，搜索/列举工具必须同口径拦截）。
pub(crate) fn check_policy_path_allowed(
    ctx: &ToolContext,
    path: &Path,
) -> Result<(), DeepseeknovaError> {
    if let Some(sec) = ctx
        .extensions
        .get::<deepseeknova_security::context::SecurityContext>()
    {
        if !sec.policy.is_path_allowed(path) {
            return Err(DeepseeknovaError::tool(format!(
                "Security violation: path '{}' is blocked by security policy \
                 ([security] allowed_paths / denied_paths)",
                path.display()
            )));
        }
    }
    Ok(())
}

/// 以 O_EXCL 语义打开临时文件：预埋的 symlink（指向工作区外）因"已存在"
/// 直接失败，杜绝 `File::create` 跟随链接把内容写到工作区外文件。
/// 残留的旧 tmp 也会触发 `AlreadyExists`，报错引导清理而非静默覆盖。
async fn create_temp_exclusive(
    tmp_path: &std::path::Path,
) -> Result<tokio::fs::File, DeepseeknovaError> {
    tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp_path)
        .await
        .map_err(|e| {
            DeepseeknovaError::tool(format!(
                "cannot create temp file {} (already exists or is a symlink): {e}",
                tmp_path.display()
            ))
        })
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

    #[cfg(unix)]
    #[tokio::test]
    async fn write_file_rejects_preplanted_tmp_symlink() {
        // F4 回归：tmp 路径被预埋 symlink 指向工作区外文件时，写入必须失败
        //（O_EXCL），不得跟随链接把内容写到外部；外部文件保持原样。
        let dir = std::env::temp_dir().join(format!("dnv-f4-write-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let ctx = test_ctx(&dir);

        let victim = std::env::temp_dir().join(format!("dnv-f4-victim-{}", std::process::id()));
        tokio::fs::write(&victim, "original").await.unwrap();

        // 预埋 symlink：write_file(path=foo.rs) 的 tmp=foo.rs.tmp → 外部 victim
        let link = dir.join("foo.rs.tmp");
        std::os::unix::fs::symlink(&victim, &link).unwrap();

        let args = r#"{"path":"foo.rs","content":"pwned"}"#;
        let res = WriteFileTool::new().execute(&ctx, args).await;
        assert!(
            res.is_err(),
            "write through preplanted tmp symlink must fail"
        );

        let after = tokio::fs::read_to_string(&victim).await.unwrap();
        assert_eq!(after, "original", "external file must be untouched");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        let _ = tokio::fs::remove_file(&victim).await;
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

    #[tokio::test]
    async fn read_file_enforces_configured_size_limit() {
        // 资源限额统一：read_file 上限必须读 `sec.limits.max_file_size`，
        // 而非内置常量。
        let dir = std::env::temp_dir().join(format!("dnv-lim-read-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("big.txt"), "x".repeat(100).as_bytes())
            .await
            .unwrap();

        let mut sec = deepseeknova_security::context::SecurityContext::with_safe_defaults();
        sec.limits.max_file_size = 50;
        let ctx = ToolContext::new("t")
            .with_workspace(dir.clone())
            .with_extension(sec);

        let err = ReadFileTool
            .execute(&ctx, r#"{"path":"big.txt"}"#)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("file too large"), "got: {err}");

        // 默认限额（1 MB）下正常读取
        let ctx = test_ctx(&dir);
        let out = ReadFileTool
            .execute(&ctx, r#"{"path":"big.txt"}"#)
            .await
            .unwrap();
        assert!(out.contains("xxx"), "got: {out}");
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

    /// B.5：delete_file 删除工作区内已存在文件，并拒绝越界路径。
    #[tokio::test]
    async fn delete_file_removes_inside_and_rejects_outside() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(dir.path().join("victim.txt"), "data")
            .await
            .unwrap();
        let ctx = test_ctx(dir.path());

        // 工作区内已存在文件：删除成功。
        let out = DeleteFileTool::new()
            .execute(&ctx, r#"{"path":"victim.txt"}"#)
            .await
            .unwrap();
        assert!(out.contains("deleted"), "got: {out}");
        assert!(!dir.path().join("victim.txt").exists(), "file must be gone");

        // 越界路径（含 .. 逃逸）：拒绝且不删除任何文件。
        let err = DeleteFileTool::new()
            .execute(&ctx, r#"{"path":"../outside_xyz.txt"}"#)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("escapes workspace root"),
            "outside path must be blocked, got: {err}"
        );
    }
}

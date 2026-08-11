use async_trait::async_trait;
use deepseeknova_core::{DeepseeknovaError, Tool, ToolContext, ToolSchema};
use serde::Deserialize;
use serde_json::json;

/// Recursively searches files for a regex pattern, aggregating matches under
/// a byte budget and enforcing the workspace read capability.
pub struct GrepTool;

#[derive(Deserialize)]
struct GrepArgs {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    glob: Option<String>,
}

#[async_trait]
impl Tool for GrepTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "grep".to_string(),
            description: "Searches file contents by regex (ripgrep syntax). Returns matching lines with path:line:content. Use this to find specific text or code patterns in files.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Regex."
                    },
                    "path": {
                        "type": "string",
                        "description": "Target (default: cwd)."
                    },
                    "glob": {
                        "type": "string",
                        "description": "Glob."
                    }
                },
                "required": ["pattern"]
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
        let parsed: GrepArgs = serde_json::from_str(args)?;

        if ctx.cancellation.is_cancelled() {
            return Err(deepseeknova_core::DeepseeknovaError::Cancelled);
        }

        let re = regex::Regex::new(&parsed.pattern)
            .map_err(|e| DeepseeknovaError::tool(format!("invalid regex: {e}")))?;

        let base = match parsed.path {
            Some(ref p) => deepseeknova_security::path::sanitize_path(&ctx.workspace_root, p)?,
            None => ctx.workspace_root.clone(),
        };

        let security = ctx
            .extensions
            .get::<deepseeknova_security::context::SecurityContext>();
        let max_files = security.map(|s| s.limits.max_files).unwrap_or(500) as u32;
        let max_total_bytes = security
            .map(|s| s.limits.max_total_read_bytes)
            .unwrap_or(50 * 1024 * 1024);
        let max_file_size = security
            .map(|s| s.limits.max_file_size)
            .unwrap_or(1024 * 1024);

        let limits = ScanLimits {
            max_files,
            max_total_bytes,
            max_file_size,
        };
        let mut state = ScanState::new();

        if base.is_file() {
            let bytes = search_file(&base, &re, &mut state.results, limits.max_file_size)?;
            state.total_bytes_searched += bytes;
            state.files_searched = 1;
        } else {
            walk_dir(
                &base,
                &ctx.workspace_root,
                &re,
                &parsed,
                ctx,
                &mut state,
                limits,
            )
            .await?;
        }

        if state.results.is_empty() {
            Ok(format!(
                "no matches for '{}' in {} (searched {} files, {} bytes)",
                parsed.pattern,
                base.display(),
                state.files_searched,
                state.total_bytes_searched
            ))
        } else {
            Ok(format!(
                "{} match(es) in {} files ({} bytes):\n{}",
                state.results.len(),
                state.files_searched,
                state.total_bytes_searched,
                state.results.join("\n")
            ))
        }
    }
}

/// grep 扫描的限额集合（来自 SecurityContext；缺省时回落内置默认值）。
#[derive(Clone, Copy)]
struct ScanLimits {
    max_files: u32,
    max_total_bytes: u64,
    max_file_size: u64,
}

/// grep 扫描的累计状态（会话级聚合计数器）。
struct ScanState {
    results: Vec<String>,
    files_searched: u32,
    total_bytes_searched: u64,
}

impl ScanState {
    fn new() -> Self {
        Self {
            results: Vec::new(),
            files_searched: 0,
            total_bytes_searched: 0,
        }
    }
}

/// 递归遍历目录，逐文件搜索；每层都检查取消/文件数上限/字节聚合上限。
/// 覆盖嵌套子目录（修复原单层 read_dir 只扫顶层的问题）。
async fn walk_dir(
    dir: &std::path::Path,
    root: &std::path::Path,
    re: &regex::Regex,
    parsed: &GrepArgs,
    ctx: &ToolContext,
    state: &mut ScanState,
    limits: ScanLimits,
) -> Result<(), DeepseeknovaError> {
    let mut read_dir = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = read_dir.next_entry().await? {
        if state.files_searched >= limits.max_files {
            state
                .results
                .push(format!("... (stopped after {} files)", limits.max_files));
            return Ok(());
        }
        if state.total_bytes_searched >= limits.max_total_bytes {
            state.results.push(format!(
                "... (stopped after reading {} bytes)",
                limits.max_total_bytes
            ));
            return Ok(());
        }
        if ctx.cancellation.is_cancelled() {
            return Err(DeepseeknovaError::Cancelled);
        }
        let path = entry.path();
        // Ensure the path is safe (prevent symlink escape)
        if deepseeknova_security::path::secure_resolve(root, &path).is_err() {
            continue;
        }
        if path.is_dir() {
            // 递归 async fn 必须装箱（Box::pin），避免无限大小的 future 类型。
            let recurse = walk_dir(&path, root, re, parsed, ctx, state, limits);
            Box::pin(recurse).await?;
        } else if path.is_file() {
            // Check glob filter if specified
            if let Some(ref g) = parsed.glob {
                let fname = path.file_name().unwrap_or_default().to_string_lossy();
                if !simple_glob_match(g, &fname) {
                    continue;
                }
            }
            let bytes = search_file(&path, re, &mut state.results, limits.max_file_size)?;
            state.total_bytes_searched += bytes;
            state.files_searched += 1;
        }
    }
    Ok(())
}

/// Search a single file for regex matches. Returns size of read file.
fn search_file(
    path: &std::path::Path,
    re: &regex::Regex,
    results: &mut Vec<String>,
    max_file_size: u64,
) -> Result<u64, DeepseeknovaError> {
    let metadata = std::fs::metadata(path)?;
    let size = metadata.len();
    if size > max_file_size {
        results.push(format!("{}: [file too large, skipped]", path.display()));
        return Ok(0);
    }

    let content = std::fs::read_to_string(path)?;
    for (line_num, line) in content.lines().enumerate() {
        if re.is_match(line) {
            let trimmed = if line.len() > 200 {
                let end = line.floor_char_boundary(200);
                format!("{}...", &line[..end])
            } else {
                line.to_string()
            };
            results.push(format!("{}:{}: {}", path.display(), line_num + 1, trimmed));
        }
    }
    Ok(size)
}

/// Simple glob match for file name filtering (supports * and ? wildcards).
fn simple_glob_match(pattern: &str, name: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if pattern.starts_with("*.") {
        let ext = &pattern[1..]; // e.g. ".rs"
        return name.ends_with(ext);
    }
    if pattern.starts_with('*') && pattern.ends_with('*') {
        let inner = &pattern[1..pattern.len() - 1];
        return name.contains(inner);
    }
    name == pattern
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_security::context::SecurityContext;

    fn temp_ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext::new("grep-test")
            .with_workspace(dir.to_path_buf())
            .with_extension(SecurityContext::with_safe_defaults())
    }

    #[tokio::test]
    async fn grep_searches_nested_directories_recursively() {
        // 回归：原单层 read_dir 只扫顶层，嵌套子目录不命中；递归后必须命中。
        let dir = std::env::temp_dir().join(format!("dnv-grep-rec-{}", std::process::id()));
        tokio::fs::create_dir_all(dir.join("src/sub"))
            .await
            .unwrap();
        tokio::fs::write(dir.join("src/a.rs"), "fn alpha_top() {}\n")
            .await
            .unwrap();
        tokio::fs::write(dir.join("src/sub/b.rs"), "fn alpha_nested() {}\n")
            .await
            .unwrap();
        tokio::fs::write(dir.join("src/sub/c.md"), "nothing here\n")
            .await
            .unwrap();

        let ctx = temp_ctx(&dir);
        let out = GrepTool
            .execute(&ctx, r#"{"pattern":"alpha","path":"src"}"#)
            .await
            .unwrap();
        assert!(
            out.contains("a.rs:1: fn alpha_top()"),
            "top-level match: {out}"
        );
        assert!(
            out.contains("b.rs:1: fn alpha_nested()"),
            "nested match must be found: {out}"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn grep_aggregates_byte_budget_across_recursion() {
        // 会话级聚合：max_total_read_bytes 跨子树累计，超限即停。
        let dir = std::env::temp_dir().join(format!("dnv-grep-bytes-{}", std::process::id()));
        tokio::fs::create_dir_all(dir.join("a/b")).await.unwrap();
        tokio::fs::write(dir.join("a/x.rs"), "word here\n".repeat(200).as_bytes())
            .await
            .unwrap();
        tokio::fs::write(dir.join("a/b/y.rs"), "word here\n".repeat(200).as_bytes())
            .await
            .unwrap();

        let mut sec = SecurityContext::with_safe_defaults();
        sec.limits.max_total_read_bytes = 10; // 极小预算，首个文件即超限
        let ctx = ToolContext::new("grep-test")
            .with_workspace(dir.clone())
            .with_extension(sec);
        let out = GrepTool
            .execute(&ctx, r#"{"pattern":"word","path":"a"}"#)
            .await
            .unwrap();
        assert!(
            out.contains("stopped after reading 10 bytes"),
            "byte aggregate must stop the scan: {out}"
        );
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}

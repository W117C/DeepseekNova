use async_trait::async_trait;
use deepseeknova_core::{Tool, ToolContext, ToolSchema};
use serde::Deserialize;
use serde_json::json;

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
            description:
                "Searches for a regex pattern in files. Returns matching lines with file path and line number."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Regex pattern to search for."
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory or file to search in (defaults to current dir)."
                    },
                    "glob": {
                        "type": "string",
                        "description": "Only search files matching this glob (e.g. '*.rs')."
                    }
                },
                "required": ["pattern"]
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
        let parsed: GrepArgs = serde_json::from_str(args)?;

        if ctx.cancellation.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let re = regex::Regex::new(&parsed.pattern)
            .map_err(|e| anyhow::anyhow!("invalid regex: {e}"))?;

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

        let mut results: Vec<String> = Vec::new();
        let mut files_searched = 0u32;
        let mut total_bytes_searched = 0u64;

        if base.is_file() {
            let bytes = search_file(&base, &re, &mut results, max_file_size)?;
            total_bytes_searched += bytes;
            files_searched = 1;
        } else {
            // Walk directory
            let mut read_dir = tokio::fs::read_dir(&base).await?;
            while let Some(entry) = read_dir.next_entry().await? {
                if files_searched >= max_files {
                    results.push(format!("... (stopped after {max_files} files)"));
                    break;
                }
                if total_bytes_searched >= max_total_bytes {
                    results.push(format!(
                        "... (stopped after reading {max_total_bytes} bytes)"
                    ));
                    break;
                }
                if ctx.cancellation.is_cancelled() {
                    anyhow::bail!("cancelled");
                }
                let path = entry.path();
                // Ensure the path is safe (prevent symlink escape)
                if deepseeknova_security::path::secure_resolve(&ctx.workspace_root, &path).is_err()
                {
                    continue;
                }
                if path.is_file() {
                    // Check glob filter if specified
                    if let Some(ref g) = parsed.glob {
                        let fname = path.file_name().unwrap_or_default().to_string_lossy();
                        if !simple_glob_match(g, &fname) {
                            continue;
                        }
                    }
                    let bytes = search_file(&path, &re, &mut results, max_file_size)?;
                    total_bytes_searched += bytes;
                    files_searched += 1;
                }
            }
        }

        if results.is_empty() {
            Ok(format!(
                "no matches for '{}' in {} (searched {files_searched} files, {total_bytes_searched} bytes)",
                parsed.pattern,
                base.display()
            ))
        } else {
            Ok(format!(
                "{} match(es) in {files_searched} files ({total_bytes_searched} bytes):\n{}",
                results.len(),
                results.join("\n")
            ))
        }
    }
}

/// Search a single file for regex matches. Returns size of read file.
fn search_file(
    path: &std::path::Path,
    re: &regex::Regex,
    results: &mut Vec<String>,
    max_file_size: u64,
) -> anyhow::Result<u64> {
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

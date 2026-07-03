use async_trait::async_trait;
use reasonix_core::{Tool, ToolContext, ToolSchema};
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
        let parsed: GrepArgs = serde_json::from_str(args)?;

        if ctx.cancellation.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let re = regex::Regex::new(&parsed.pattern)
            .map_err(|e| anyhow::anyhow!("invalid regex: {e}"))?;

        let base = match parsed.path {
            Some(ref p) => std::path::PathBuf::from(p),
            None => std::env::current_dir()?,
        };

        let mut results: Vec<String> = Vec::new();
        let mut files_searched = 0u32;
        const MAX_FILES: u32 = 500;

        if base.is_file() {
            search_file(&base, &re, &mut results)?;
            files_searched = 1;
        } else {
            // Walk directory
            let mut read_dir = tokio::fs::read_dir(&base).await?;
            while let Some(entry) = read_dir.next_entry().await? {
                if files_searched >= MAX_FILES {
                    results.push(format!("... (stopped after {MAX_FILES} files)"));
                    break;
                }
                if ctx.cancellation.is_cancelled() {
                    anyhow::bail!("cancelled");
                }
                let path = entry.path();
                if path.is_file() {
                    // Check glob filter if specified
                    if let Some(ref g) = parsed.glob {
                        let fname = path.file_name().unwrap_or_default().to_string_lossy();
                        if !simple_glob_match(g, &fname) {
                            continue;
                        }
                    }
                    search_file(&path, &re, &mut results)?;
                    files_searched += 1;
                }
            }
        }

        if results.is_empty() {
            Ok(format!(
                "no matches for '{}' in {} (searched {files_searched} files)",
                parsed.pattern,
                base.display()
            ))
        } else {
            Ok(format!(
                "{} match(es) in {files_searched} files:\n{}",
                results.len(),
                results.join("\n")
            ))
        }
    }
}

/// Search a single file for regex matches.
fn search_file(
    path: &std::path::Path,
    re: &regex::Regex,
    results: &mut Vec<String>,
) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(path)?;
    const MAX_SIZE: u64 = 1024 * 1024; // 1 MB per file
    if content.len() as u64 > MAX_SIZE {
        results.push(format!("{}: [file too large, skipped]", path.display()));
        return Ok(());
    }

    for (line_num, line) in content.lines().enumerate() {
        if re.is_match(line) {
            let trimmed = if line.len() > 200 {
                format!("{}...", &line[..200])
            } else {
                line.to_string()
            };
            results.push(format!("{}:{}: {}", path.display(), line_num + 1, trimmed));
        }
    }
    Ok(())
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

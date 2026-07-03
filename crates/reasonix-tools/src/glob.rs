use async_trait::async_trait;
use reasonix_core::{Tool, ToolContext, ToolSchema};
use serde::Deserialize;
use serde_json::json;

pub struct GlobTool;

#[derive(Deserialize)]
struct GlobArgs {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
}

#[async_trait]
impl Tool for GlobTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "glob".to_string(),
            description: "Finds files matching a glob pattern (e.g. '**/*.rs', 'src/*.ts')."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern to match."
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory to search in (defaults to current dir)."
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
        let parsed: GlobArgs = serde_json::from_str(args)?;

        if ctx.cancellation.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let base = match parsed.path {
            Some(p) => std::path::PathBuf::from(p),
            None => std::env::current_dir()?,
        };

        // Build the full glob pattern
        let full_pattern = base.join(&parsed.pattern);
        let pattern_str = full_pattern.to_string_lossy();

        let mut matches: Vec<String> = Vec::new();
        let paths = glob::glob(&pattern_str)?;

        for entry in paths {
            match entry {
                Ok(p) => {
                    matches.push(p.display().to_string());
                }
                Err(e) => {
                    tracing::warn!("glob error: {e}");
                }
            }
        }

        matches.sort();

        if matches.is_empty() {
            Ok(format!("no files matched '{pattern_str}'"))
        } else {
            Ok(format!(
                "{} matches for '{pattern_str}':\n{}",
                matches.len(),
                matches.join("\n")
            ))
        }
    }
}

use async_trait::async_trait;
use deepseeknova_core::{DeepseeknovaError, Tool, ToolContext, ToolSchema};
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
            description: "Finds files by glob.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Pattern."
                    },
                    "path": {
                        "type": "string",
                        "description": "Dir (default: cwd)."
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
        let parsed: GlobArgs = serde_json::from_str(args)?;

        if ctx.cancellation.is_cancelled() {
            return Err(deepseeknova_core::DeepseeknovaError::Cancelled);
        }

        let base = match parsed.path {
            Some(p) => deepseeknova_security::path::sanitize_path(&ctx.workspace_root, &p)?,
            None => ctx.workspace_root.clone(),
        };

        // Build the full glob pattern
        let full_pattern = base.join(&parsed.pattern);
        let pattern_str = full_pattern.to_string_lossy();

        let mut matches: Vec<String> = Vec::new();
        let paths = glob::glob(&pattern_str).map_err(|e| {
            DeepseeknovaError::Tool(format!("invalid glob pattern '{pattern_str}': {e}"))
        })?;

        for entry in paths {
            match entry {
                Ok(p) => {
                    if deepseeknova_security::path::secure_resolve(&ctx.workspace_root, &p).is_ok()
                    {
                        matches.push(p.display().to_string());
                    } else {
                        tracing::warn!(
                            security_event = "glob_match_blocked",
                            match_path = ?p.display(),
                            workspace = ?ctx.workspace_root.display(),
                            reason = "matched path escapes workspace root"
                        );
                    }
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

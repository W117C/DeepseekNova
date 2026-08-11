use async_trait::async_trait;
use deepseeknova_core::{Tool, ToolContext, ToolSchema};
use serde::Deserialize;
use serde_json::json;

/// Lists a directory's immediate entries (directories prefixed `d `, files
/// `f `), sorted by name, relative to the workspace root.
pub struct LsTool;

#[derive(Deserialize)]
struct LsArgs {
    #[serde(default)]
    path: Option<String>,
}

#[async_trait]
impl Tool for LsTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "ls".to_string(),
            description: "Lists a directory.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path."
                    }
                },
                "required": []
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
        let parsed: LsArgs = if args.trim().is_empty() {
            LsArgs { path: None }
        } else {
            serde_json::from_str(args)?
        };

        if ctx.cancellation.is_cancelled() {
            return Err(deepseeknova_core::DeepseeknovaError::Cancelled);
        }

        let dir = match parsed.path {
            Some(p) => deepseeknova_security::path::sanitize_path(&ctx.workspace_root, &p)?,
            None => ctx.workspace_root.clone(),
        };

        let mut entries: Vec<String> = Vec::new();
        let mut read_dir = tokio::fs::read_dir(&dir).await?;

        while let Some(entry) = read_dir.next_entry().await? {
            let ft = entry.file_type().await?;
            let name = entry.file_name().to_string_lossy().to_string();
            let prefix = if ft.is_dir() { "d " } else { "f " };
            entries.push(format!("{prefix}{name}"));
        }

        entries.sort();
        if entries.is_empty() {
            Ok(format!("{} (empty)", dir.display()))
        } else {
            Ok(format!("{}:\n{}", dir.display(), entries.join("\n")))
        }
    }
}

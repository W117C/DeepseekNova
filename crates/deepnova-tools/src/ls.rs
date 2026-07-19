use async_trait::async_trait;
use deepnova_core::{Tool, ToolContext, ToolSchema};
use serde::Deserialize;
use serde_json::json;

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
            description:
                "Lists files and directories in a given path. Defaults to current directory."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory path to list (defaults to .)."
                    }
                },
                "required": []
            }),
        }
    }

    fn read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        deepnova_security::context::enforce_capability(
            ctx,
            deepnova_security::capability::Capability::FileRead,
        )?;
        let parsed: LsArgs = if args.trim().is_empty() {
            LsArgs { path: None }
        } else {
            serde_json::from_str(args)?
        };

        if ctx.cancellation.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let dir = match parsed.path {
            Some(p) => deepnova_security::path::sanitize_path(&ctx.workspace_root, &p)?,
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

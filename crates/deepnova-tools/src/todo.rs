use async_trait::async_trait;
use deepnova_core::{Tool, ToolContext, ToolSchema};
use serde::Deserialize;
use serde_json::json;

// ---------------------------------------------------------------------------
// TodoWriteTool — structured task tracking
// ---------------------------------------------------------------------------

pub struct TodoWriteTool;

/// The valid status values for a todo item.
const VALID_STATUSES: &[&str] = &["pending", "in_progress", "completed", "cancelled"];

#[derive(Deserialize)]
struct TodoWriteArgs {
    #[serde(default)]
    merge: bool,
    todos: Vec<TodoItem>,
}

#[derive(Deserialize)]
struct TodoItem {
    id: String,
    content: String,
    status: String,
}

#[async_trait]
impl Tool for TodoWriteTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "todo_write".to_string(),
            description:
                "Creates and updates a structured task list for the current coding session. \
                 Use this to track progress across multi-step tasks. \
                 Set merge=true to update existing items; merge=false (default) to replace the \
                 entire list."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "merge": {
                        "type": "boolean",
                        "description": "If true, merge the provided todos with the existing list \
                            by matching on id. If false (default), replace the entire list.",
                        "default": false
                    },
                    "todos": {
                        "type": "array",
                        "description": "The list of todo items.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {
                                    "type": "string",
                                    "description": "Unique identifier for this todo item."
                                },
                                "content": {
                                    "type": "string",
                                    "description": "Description of what needs to be done."
                                },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed", "cancelled"],
                                    "description": "Current state of the todo item."
                                }
                            },
                            "required": ["id", "content", "status"]
                        }
                    }
                },
                "required": ["todos"]
            }),
        }
    }

    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        let parsed: TodoWriteArgs = serde_json::from_str(args)?;

        if ctx.cancellation.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        // Validate todos
        for (idx, todo) in parsed.todos.iter().enumerate() {
            if todo.id.is_empty() {
                anyhow::bail!("todo item at index {idx} has an empty id");
            }
            if todo.content.is_empty() {
                anyhow::bail!("todo item '{}' has empty content", todo.id);
            }
            if !VALID_STATUSES.contains(&todo.status.as_str()) {
                anyhow::bail!(
                    "todo item '{}' has invalid status '{}'; must be one of: {:?}",
                    todo.id,
                    todo.status,
                    VALID_STATUSES
                );
            }
        }

        // Format output
        let mode = if parsed.merge { "merged" } else { "replaced" };
        let status_width = VALID_STATUSES.iter().map(|s| s.len()).max().unwrap_or(8);

        let mut lines = Vec::with_capacity(parsed.todos.len() + 2);
        lines.push(format!(
            "{} {} todo item{}",
            mode,
            parsed.todos.len(),
            if parsed.todos.len() == 1 { "" } else { "s" }
        ));
        lines.push(String::new());

        for todo in &parsed.todos {
            let icon = match todo.status.as_str() {
                "completed" => "[x]",
                "in_progress" => "[>]",
                "cancelled" => "[-]",
                _ => "[ ]",
            };
            let status_padded = format!("{:<width$}", todo.status, width = status_width);
            lines.push(format!(
                "{} [{}] {} | {}",
                icon, status_padded, todo.id, todo.content
            ));
        }

        Ok(lines.join("\n"))
    }
}

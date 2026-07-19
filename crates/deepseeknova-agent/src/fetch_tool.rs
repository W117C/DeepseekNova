use deepseeknova_core::tool::{ParallelSafety, Tool, ToolContext};
use deepseeknova_core::types::ToolSchema;
use std::sync::{Arc, RwLock};

pub struct FetchFullResultTool {
    memory: Arc<RwLock<crate::memory::Memory>>,
}

impl FetchFullResultTool {
    pub fn new(memory: Arc<RwLock<crate::memory::Memory>>) -> Self {
        Self { memory }
    }
}

#[async_trait::async_trait]
impl Tool for FetchFullResultTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "fetch_full_result".to_string(),
            description: "Fetches the full original result of a truncated tool call by its ID.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "tool_call_id": {
                        "type": "string",
                        "description": "The ID of the tool call to retrieve the full result for."
                    }
                },
                "required": ["tool_call_id"]
            }),
        }
    }

    fn safety(&self) -> ParallelSafety {
        ParallelSafety::Safe
    }

    async fn execute(&self, _ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        let parsed: serde_json::Value = serde_json::from_str(args)?;
        let tool_call_id = parsed.get("tool_call_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing tool_call_id parameter"))?;

        let mem_guard = self.memory.read().map_err(|e| anyhow::anyhow!("Memory lock error: {}", e))?;
        
        if let Some(result) = mem_guard.get_full_result(tool_call_id) {
            Ok(result.clone())
        } else {
            Ok(format!("Error: No truncated result found for ID {}", tool_call_id))
        }
    }
}

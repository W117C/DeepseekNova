use deepseeknova_core::tool::{ParallelSafety, Tool, ToolContext};
use deepseeknova_core::types::ToolSchema;
use deepseeknova_core::DeepseeknovaError;
use std::sync::Arc;

/// 按需取回被截断的完整工具结果（token 节省 C1：大结果截断时完整版
/// 存入 [`crate::memory::Memory::full_results`]，模型凭 call_id 取回）。
///
/// 由 `Agent::run_stream` 用共享的 `Arc<tokio::sync::RwLock<Memory>>`
/// 注册进工具集；`execute` 以异步读锁访问，不会阻塞执行循环。
pub struct FetchFullResultTool {
    memory: Arc<tokio::sync::RwLock<crate::memory::Memory>>,
}

impl FetchFullResultTool {
    pub fn new(memory: Arc<tokio::sync::RwLock<crate::memory::Memory>>) -> Self {
        Self { memory }
    }
}

#[async_trait::async_trait]
impl Tool for FetchFullResultTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "fetch_full_result".to_string(),
            description: "Fetches the full original result of a truncated tool call by its ID."
                .to_string(),
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

    async fn execute(&self, _ctx: &ToolContext, args: &str) -> Result<String, DeepseeknovaError> {
        let parsed: serde_json::Value = serde_json::from_str(args)?;
        let tool_call_id = parsed
            .get("tool_call_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| DeepseeknovaError::tool("Missing tool_call_id parameter".to_string()))?;

        let mem_guard = self.memory.read().await;
        if let Some(result) = mem_guard.get_full_result(tool_call_id) {
            Ok(result.clone())
        } else {
            Ok(format!(
                "Error: No truncated result found for ID {}",
                tool_call_id
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::Memory;
    use deepseeknova_core::Message;

    #[tokio::test]
    async fn fetch_full_result_returns_truncated_original() {
        // 闭环：大工具结果被 shrink 截断并存完整版 → 工具凭 call_id 取回原样。
        let memory = Arc::new(tokio::sync::RwLock::new(Memory::new()));
        // 标记放字符串中部：截断（head+tail）时会被省略，可验证确实截断了
        let long = format!(
            "{}MIDDLE_MARKER_9f8e7d{}",
            "a".repeat(3000),
            "b".repeat(3000)
        );
        {
            let mut mem = memory.write().await;
            mem.add_message(Message {
                role: deepseeknova_core::Role::Tool,
                content: long.clone(),
                name: None,
                tool_calls: None,
                tool_call_id: Some("call_1".to_string()),
                reasoning_content: None,
                reasoning_signature: None,
            });
            // 预算极小：必然截断并缓存完整结果
            mem.shrink_large_results(1);
            // 截断后的消息不应再含中部标记
            let msgs = mem.get_all();
            let last = msgs.last().unwrap();
            assert!(!last.content.contains("MIDDLE_MARKER"), "must be truncated");
            assert!(
                last.content.contains("fetch_full_result"),
                "truncation hint must reference the tool"
            );
        }

        let tool = FetchFullResultTool::new(memory.clone());
        let ctx = ToolContext::new("fetch-test");
        let out = tool
            .execute(&ctx, r#"{"tool_call_id":"call_1"}"#)
            .await
            .unwrap();
        assert_eq!(out, long, "full original must be returned");
    }

    #[tokio::test]
    async fn fetch_full_result_missing_id_reports_error() {
        let memory = Arc::new(tokio::sync::RwLock::new(Memory::new()));
        let tool = FetchFullResultTool::new(memory);
        let ctx = ToolContext::new("fetch-test");
        let out = tool
            .execute(&ctx, r#"{"tool_call_id":"nope"}"#)
            .await
            .unwrap();
        assert!(out.contains("No truncated result found"));
    }
}

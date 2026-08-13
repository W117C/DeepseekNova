//! `ask_user` 工具：agent 向用户提问并获取输入（B.5 骨架）。
//!
//! 首版为骨架 + 文档标注：用户输入经 [`ToolContext`] 扩展注入的
//! [`AskUserResponder`] 获取；正式 CLI/serve 交互通道未接线前，未注入
//! responder 时工具返回文档化占位说明（不硬失败、不臆造输入）。

use async_trait::async_trait;
use deepseeknova_core::{DeepseeknovaError, Tool, ToolContext, ToolSchema};
use serde::Deserialize;
use serde_json::json;

/// ask_user 的用户输入应答器：由 CLI/serve 交互通道注入
/// （`ToolContext::with_extension`），返回对提问的回答文本。
pub trait AskUserResponder: Send + Sync {
    /// 回答一次用户提问。
    fn ask(&self, question: &str) -> String;
}

/// 向用户提问并取回输入的只读工具。
pub struct AskUserTool;

#[derive(Deserialize)]
struct AskUserArgs {
    question: String,
}

#[async_trait]
impl Tool for AskUserTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "ask_user".to_string(),
            description: "Asks the user a clarifying question and returns their answer. Use only when a decision genuinely requires user input.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "question": {
                        "type": "string",
                        "description": "The question to ask the user."
                    }
                },
                "required": ["question"]
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
    ) -> Result<String, DeepseeknovaError> {
        let parsed: AskUserArgs = serde_json::from_str(args)?;
        // 用户输入经注入的 responder 获取；未注入（CLI/serve 交互通道
        // 尚未接线）时返回文档化占位说明，提示当前通道不可用。
        match ctx.extensions.get::<Box<dyn AskUserResponder>>() {
            Some(r) => Ok(format!("[user answered]\n{}", r.ask(&parsed.question))),
            None => Ok(
                "Error: ask_user is not wired in this session (no interactive user channel); \
                 proceed with a reasonable default and note the assumption."
                    .to_string(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ask_user_without_responder_returns_documented_placeholder() {
        let ctx = ToolContext::new("c");
        let out = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(AskUserTool.execute(&ctx, r#"{"question":"proceed?"}"#))
            .unwrap();
        assert!(out.contains("not wired"), "got: {out}");
    }

    #[test]
    fn ask_user_with_responder_returns_answer() {
        struct Echo;
        impl AskUserResponder for Echo {
            fn ask(&self, q: &str) -> String {
                format!("echo:{q}")
            }
        }
        let ctx = ToolContext::new("c").with_extension(Box::new(Echo) as Box<dyn AskUserResponder>);
        let out = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(AskUserTool.execute(&ctx, r#"{"question":"proceed?"}"#))
            .unwrap();
        assert!(out.contains("echo:proceed?"), "got: {out}");
    }
}

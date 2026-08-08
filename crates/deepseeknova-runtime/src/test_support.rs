//! 测试共享替身（cfg(test) 专用）：Provider/Tool stub 与外部命令 mock。
//! M7b 拆分：从 lib.rs tests 模块抽出，供各子模块测试与 lib.rs 组合根集成
//! 测试复用。仅 `#[cfg(test)]` 编译（lib.rs 以 `#[cfg(test)] mod test_support;`
//! 声明）。

// Minimal Provider stub: never actually called by these tests (they only
// assert on the synchronously-registered tool set), but build_agent needs
// a concrete provider to construct the agent.
pub(crate) struct StubProvider;

#[async_trait::async_trait]
impl deepseeknova_provider::Provider for StubProvider {
    async fn generate(
        &self,
        _validated: deepseeknova_provider::ValidatedRequest<'_>,
    ) -> anyhow::Result<deepseeknova_core::Message> {
        Ok(deepseeknova_core::Message {
            role: deepseeknova_core::Role::Assistant,
            content: "ok".to_string(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        })
    }
}

pub(crate) fn stub_provider() -> StubProvider {
    StubProvider
}

/// 空内容 provider：agent 每步无输出 → MaxSteps → Paused（构造失败 run
/// 用；默认 `stream` 只透出 TextDelta+Done，Message 带 tool_calls 也不会
/// 触发工具执行，故空内容即可稳定命中 MaxSteps 路径）。
pub(crate) struct EmptyProvider;

#[async_trait::async_trait]
impl deepseeknova_provider::Provider for EmptyProvider {
    async fn generate(
        &self,
        _validated: deepseeknova_provider::ValidatedRequest<'_>,
    ) -> anyhow::Result<deepseeknova_core::Message> {
        Ok(deepseeknova_core::Message {
            role: deepseeknova_core::Role::Assistant,
            content: String::new(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        })
    }
}

/// 构造「追加固定文本到文件」的配置命令（外部命令 mock）。
pub(crate) fn marker_cmd(
    marker: &str,
    path: &std::path::Path,
) -> deepseeknova_config::HookCommandConfig {
    deepseeknova_config::HookCommandConfig {
        command: "sh".into(),
        args: vec![
            "-c".into(),
            format!("echo '{}' >> '{}'", marker, path.display()),
        ],
        timeout_secs: Some(10),
        disabled: false,
    }
}

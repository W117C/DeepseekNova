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
    ) -> Result<deepseeknova_core::Message, deepseeknova_core::DeepseeknovaError> {
        Ok(deepseeknova_core::Message {
            role: deepseeknova_core::Role::Assistant,
            content: "ok".to_string(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            reasoning_signature: None,
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
    ) -> Result<deepseeknova_core::Message, deepseeknova_core::DeepseeknovaError> {
        Ok(deepseeknova_core::Message {
            role: deepseeknova_core::Role::Assistant,
            content: String::new(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            reasoning_signature: None,
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

/// env 串行化锁：`std::env` 的 `set_var`/`remove_var` 非线程安全，所有修改
/// env 或构建 reqwest::Client 的测试须用此锁串行化，避免并发 UB。
/// 异步测试用 `.lock().await`，同步测试用 `.blocking_lock()`。
pub(crate) static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 清除代理环境变量：reqwest 默认尊重 HTTP_PROXY/HTTPS_PROXY，会把请求转发
/// 到代理，代理无法连本地 mock 端口导致 Connect 失败。须在 ENV_LOCK guard
/// 内调用。
pub(crate) fn clear_proxy_env() {
    for v in &[
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "http_proxy",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        std::env::remove_var(v);
    }
}

//! 用户级外部 hooks（`[hooks]` 段）与任务质量钩子（`[quality]` 段）装配。
//! M7b 拆分：从 lib.rs 纯搬移，不修改行为/签名。

use std::sync::Arc;

use deepseeknova_config::Config;

/// 按配置为 Agent 挂载任务质量钩子（A 阶段：ToolHook 链 + 写后策略评估）：
/// `[quality] enabled=true` 时注册内置
/// [`QualityHook`](deepseeknova_agent::quality::QualityHook)（builtin 策略，含
/// no-commit-secret / no-forbidden-path / oversized-write 三条规则）。
/// `enabled=false` 时原样返回，Agent 行为零变化。
pub fn attach_quality_hook(
    agent: deepseeknova_agent::Agent,
    config: &Config,
) -> deepseeknova_agent::Agent {
    if !config.quality.enabled {
        return agent;
    }
    let hook = deepseeknova_agent::quality::QualityHook::new(
        deepseeknova_security::quality::QualityPolicy::builtin(),
    );
    agent.with_tool_hook(Arc::new(hook))
}

/// 按配置挂载用户级外部 hooks（`[hooks]` 段）：`enabled=true` 且任一事件挂载
/// 了有效命令（未 `disabled`）时，把 config 命令映射为
/// [`deepseeknova_core::tool_hook::UserHooks`] 并挂到 agent（`with_user_hooks`）。
/// `enabled=false` / 全空 / 全部 `disabled` 时原样返回——Agent 零进程开销
/// （不 spawn 任何进程）。
///
/// 事件语义（对齐配置注释）：tool_before 为 AND 链预检（任一失败 →
/// fail-closed 阻止执行），tool_after / session_start / session_end / failure
/// 为通知型（失败仅 warn，不阻断）。内部 tool_hook 治理链不受影响，用户
/// hooks 是额外一层。
pub fn attach_user_hooks(
    agent: deepseeknova_agent::Agent,
    config: &Config,
) -> deepseeknova_agent::Agent {
    let hooks = &config.hooks;
    if !hooks.enabled || hooks.is_empty() {
        return agent;
    }
    let user_hooks = user_hooks_from_config(hooks);
    if user_hooks.is_empty() {
        return agent;
    }
    agent.with_user_hooks(user_hooks)
}

/// 把配置命令映射为运行时规格（过滤 `disabled`、转换超时）。
pub(crate) fn user_hooks_from_config(
    cfg: &deepseeknova_config::HooksConfig,
) -> deepseeknova_core::tool_hook::UserHooks {
    fn map(
        list: &[deepseeknova_config::HookCommandConfig],
    ) -> Vec<deepseeknova_core::tool_hook::UserHookCommand> {
        list.iter()
            .filter(|c| !c.disabled)
            .map(|c| deepseeknova_core::tool_hook::UserHookCommand {
                command: c.command.clone(),
                args: c.args.clone(),
                timeout: c.timeout_secs.map(std::time::Duration::from_secs),
            })
            .collect()
    }
    deepseeknova_core::tool_hook::UserHooks {
        tool_before: map(&cfg.tool_before),
        tool_after: map(&cfg.tool_after),
        session_start: map(&cfg.session_start),
        session_end: map(&cfg.session_end),
        failure: map(&cfg.failure),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    #[test]
    fn user_hooks_from_config_maps_events_and_filters_disabled() {
        let cfg = deepseeknova_config::HooksConfig {
            enabled: true,
            tool_before: vec![
                marker_cmd("a", std::path::Path::new("/tmp/a")),
                deepseeknova_config::HookCommandConfig {
                    command: "disabled-cmd".into(),
                    args: vec![],
                    timeout_secs: None,
                    disabled: true,
                },
            ],
            tool_after: vec![marker_cmd("b", std::path::Path::new("/tmp/b"))],
            session_start: vec![],
            session_end: vec![],
            failure: vec![],
        };
        let hooks = user_hooks_from_config(&cfg);
        assert_eq!(hooks.tool_before.len(), 1, "disabled 命令必须被过滤");
        assert_eq!(hooks.tool_before[0].command, "sh");
        assert_eq!(
            hooks.tool_before[0].timeout,
            Some(std::time::Duration::from_secs(10)),
            "timeout_secs 必须转换为 Duration"
        );
        assert_eq!(hooks.tool_after.len(), 1);
        assert!(hooks.session_start.is_empty());
        assert!(hooks.session_end.is_empty());
        assert!(hooks.failure.is_empty());
        assert!(!hooks.is_empty());
    }

    #[test]
    fn attach_user_hooks_noop_when_all_commands_disabled() {
        // enabled=true 但命令全部 disabled → 映射后 UserHooks 为空 → 不挂载。
        let cfg = deepseeknova_config::HooksConfig {
            enabled: true,
            tool_before: vec![deepseeknova_config::HookCommandConfig {
                command: "audit".into(),
                args: vec![],
                timeout_secs: None,
                disabled: true,
            }],
            ..Default::default()
        };
        let hooks = user_hooks_from_config(&cfg);
        assert!(hooks.is_empty(), "全部 disabled 时必须映射为空");
    }

    // -----------------------------------------------------------------------
    // M8b：attach_quality_hook 装配正确性（enabled=true 挂钩 / false 零开销）
    // -----------------------------------------------------------------------

    use deepseeknova_core::tool::{Tool, ToolContext};
    use deepseeknova_core::types::ToolSchema;
    use deepseeknova_core::{RunInput, Runner};
    use futures::StreamExt;
    use std::sync::Mutex;

    /// 记录执行次数的假写工具（名字固定 write_file，覆盖内置）。
    struct RecordingWriteTool {
        calls: Arc<Mutex<usize>>,
    }

    #[async_trait::async_trait]
    impl Tool for RecordingWriteTool {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: "write_file".to_string(),
                description: "recording write tool".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }
        }
        fn read_only(&self) -> bool {
            false
        }
        async fn execute(
            &self,
            _ctx: &ToolContext,
            _args: &str,
        ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
            *self.calls.lock().unwrap() += 1;
            Ok("written".into())
        }
    }

    /// 驱动一轮「write .env」的 mock provider run（工具调用 → 文本回复）。
    async fn run_write_env(agent: deepseeknova_agent::Agent) {
        let mut stream = agent
            .run_stream(RunInput {
                prompt: "write .env".into(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}
    }

    /// quality 钩子装配：enabled=true 时挂载 QualityHook——写 `.env`（禁写
    /// 路径）在 before 阶段被 Deny，工具不执行（无写发生）。
    #[tokio::test]
    async fn attach_quality_hook_denies_forbidden_path_write_when_enabled() {
        let ws = std::env::temp_dir().join(format!("dnv-quality-on-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ws);
        std::fs::create_dir_all(&ws).unwrap();
        let calls = Arc::new(Mutex::new(0usize));
        let provider = deepseeknova_agent::test_utils::MockProvider::tool_call(
            "write_file",
            r#"{"path":".env","content":"SECRET=1"}"#,
            "ignored",
            "done",
        );
        let mut agent =
            deepseeknova_agent::Agent::new(Arc::new(provider), 5).with_workspace_root(ws.clone());
        agent.register_tool(Arc::new(RecordingWriteTool {
            calls: calls.clone(),
        }));
        let mut config = Config::default();
        config.quality.enabled = true;
        let agent = attach_quality_hook(agent, &config);
        run_write_env(agent).await;
        assert_eq!(
            *calls.lock().unwrap(),
            0,
            "quality hook 必须在执行前拒绝禁写路径写"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// quality 关闭时零开销：attach_quality_hook 原样返回 agent，写工具正常
    /// 执行（对照上例，证明 enabled=false 不挂钩）。
    #[tokio::test]
    async fn attach_quality_hook_disabled_leaves_write_unrestricted() {
        let ws = std::env::temp_dir().join(format!("dnv-quality-off-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ws);
        std::fs::create_dir_all(&ws).unwrap();
        let calls = Arc::new(Mutex::new(0usize));
        let provider = deepseeknova_agent::test_utils::MockProvider::tool_call(
            "write_file",
            r#"{"path":".env","content":"SECRET=1"}"#,
            "ignored",
            "done",
        );
        let mut agent =
            deepseeknova_agent::Agent::new(Arc::new(provider), 5).with_workspace_root(ws.clone());
        agent.register_tool(Arc::new(RecordingWriteTool {
            calls: calls.clone(),
        }));
        let mut config = Config::default();
        config.quality.enabled = false;
        let agent = attach_quality_hook(agent, &config);
        run_write_env(agent).await;
        assert_eq!(*calls.lock().unwrap(), 1, "quality 关闭时写工具应正常执行");
        let _ = std::fs::remove_dir_all(&ws);
    }
}

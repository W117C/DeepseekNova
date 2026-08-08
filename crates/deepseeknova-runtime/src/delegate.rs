//! 委派引擎与子代理运行器装配：预设合并、markdown 声明通道、禁递归、deny 冻结。
//! M7b 拆分：从 lib.rs 纯搬移，不修改行为/签名。

use std::sync::Arc;

use deepseeknova_config::Config;
use deepseeknova_permission::PermissionGate;
use deepseeknova_security::context::SecurityContext;

use crate::helpers::derive_compaction_threshold;
use crate::security::sandbox_writable_paths;

/// 合并内置委派预设与 `config.delegate.agents` 覆盖（按 name 匹配覆盖字段，
/// 未匹配则新增）。供委派引擎与 coordinator 的 SubAgentRunner 共用。
fn merged_delegate_presets(config: &Config) -> Vec<deepseeknova_agent::DelegatePreset> {
    use deepseeknova_agent::task_spec::InputValues;

    let mut presets = deepseeknova_agent::builtin_presets();
    for ov in &config.delegate.agents {
        if let Some(p) = presets.iter_mut().find(|p| p.name == ov.name) {
            if let Some(sp) = &ov.system_prompt {
                p.system_prompt = sp.clone();
            }
            if let Some(tools) = &ov.tools {
                p.spec.tools = tools.clone();
            }
            if let Some(ms) = ov.max_steps {
                p.spec.max_steps = ms;
            }
            if let Some(inputs) = &ov.inputs {
                p.config_inputs = InputValues::from(
                    inputs
                        .iter()
                        .map(|i| (i.name.clone(), i.value.clone()))
                        .collect::<std::collections::HashMap<_, _>>(),
                );
            }
        } else {
            let mut preset = deepseeknova_agent::DelegatePreset::simple(
                ov.name.clone(),
                ov.system_prompt.clone().unwrap_or_default(),
                ov.tools.clone().unwrap_or_default(),
                ov.max_steps.unwrap_or(10),
            );
            if let Some(inputs) = &ov.inputs {
                preset.config_inputs = InputValues::from(
                    inputs
                        .iter()
                        .map(|i| (i.name.clone(), i.value.clone()))
                        .collect::<std::collections::HashMap<_, _>>(),
                );
            }
            presets.push(preset);
        }
    }
    presets
}

/// M3：把 `.deepseeknova/agents/*.md` markdown 子代理声明并入委派预设集合
/// （AgentManifest 通道接线：`agent_manifest::load_dir` 解析 →
/// `to_delegate_preset`，与 TOML 预设合并）。目录缺失/解析失败仅 warn
/// （跳过），不阻断构建。同名冲突：markdown 声明后注册，覆盖内置/TOML 预设
/// （用户声明优先，与 `load_dir` 的"用户通道新于既有通道"定位一致）。
fn merged_delegate_presets_with_manifests(
    config: &Config,
    workspace_root: &std::path::Path,
) -> Vec<deepseeknova_agent::DelegatePreset> {
    let mut presets = merged_delegate_presets(config);
    let agents_dir = config
        .delegate
        .agents_dir
        .clone()
        .unwrap_or_else(|| workspace_root.join(deepseeknova_agent::DEFAULT_AGENT_DIR));
    match deepseeknova_agent::agent_manifest::load_dir(&agents_dir) {
        Ok(manifests) => {
            for m in manifests {
                if presets.iter().any(|p| p.name == m.name) {
                    tracing::warn!(
                        "markdown agent '{}' overrides an existing delegate preset (TOML/builtin)",
                        m.name
                    );
                }
                presets.push(m.to_delegate_preset());
            }
        }
        Err(e) => {
            tracing::warn!("agents dir '{}' skipped: {e}", agents_dir.display());
        }
    }
    presets
}

/// 构建委派引擎：合并内置预设 + 配置覆盖 + `.deepseeknova/agents/*.md`
/// markdown 声明（AgentManifest 通道），为每个预设造一个受限工具集的子 Agent
/// （共享主 agent 的 graph/memory 句柄与安全策略）。**禁递归**：剔除任何
/// "delegate" 工具（引擎路径无递归出口）。`config.delegate.max_depth` 透传给
/// 引擎（`with_max_depth`），作为深度守门上限备用；`attribution` 提供子代理
/// 失败归因重试（None = 旧行为，失败直接上抛）。
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_delegate_engine(
    config: &Config,
    provider: Arc<dyn deepseeknova_provider::Provider>,
    workspace_root: &std::path::Path,
    security: &SecurityContext,
    gate: Option<Arc<PermissionGate>>,
    graph_ext: Option<deepseeknova_tools::GraphHandle>,
    memory_ext: Option<deepseeknova_tools::MemoryHandle>,
    attribution: Option<Arc<deepseeknova_agent::attribution::AttributionSettings>>,
) -> Arc<deepseeknova_agent::DelegateEngine> {
    use deepseeknova_core::Tool;

    // 子代理工具源（沿用主 agent 的沙箱选择）。
    // 注：子代理刻意不接收 MCP/extra_tools——它们只从内置工具集派生受限子集，
    // 因此 MCP 工具天然只暴露给主 agent（等价于向 build_agent 传 vec![]）。
    let base: Vec<Arc<dyn Tool>> = if config.sandbox.enabled {
        let sandbox: Arc<dyn deepseeknova_sandbox::Sandbox> =
            Arc::from(deepseeknova_sandbox::platform_sandbox_with(
                &sandbox_writable_paths(config, workspace_root),
                config.sandbox.allow_network,
            ));
        deepseeknova_tools::all_builtin_tools_with_sandbox(sandbox)
    } else {
        deepseeknova_tools::all_builtin_tools()
    };

    // 合并内置预设 + 配置覆盖 + markdown 声明。
    let presets = merged_delegate_presets_with_manifests(config, workspace_root);

    let mut agents: std::collections::HashMap<String, Arc<deepseeknova_agent::Agent>> =
        std::collections::HashMap::new();
    for p in &presets {
        // 禁递归：即便配置误加 "delegate" 也剔除。
        let sub_tools: Vec<Arc<dyn Tool>> = base
            .iter()
            .filter(|t| {
                let n = t.schema().name;
                n != "delegate" && p.spec.tools.iter().any(|allow| allow == &n)
            })
            .cloned()
            .collect();
        let mut sub = deepseeknova_agent::Agent::new(Arc::clone(&provider), p.spec.max_steps)
            .with_workspace_root(workspace_root.to_path_buf())
            .with_security(security.clone())
            .with_system_prompt(p.system_prompt.clone());
        for t in sub_tools {
            sub.register_tool(t);
        }
        if let Some(g) = &graph_ext {
            sub = sub.with_extension(g.clone());
        }
        if let Some(m) = &memory_ext {
            sub = sub.with_extension(m.clone());
        }
        if let Some(gate) = &gate {
            sub = sub.with_permission_gate(gate.clone());
            // deny 冻结（prompt 层）：父级 deny 规则注入子代理 system prompt，
            // 让子代理模型在发起调用前即知晓禁止边界。执行层仍由共享 gate 强制。
            if let Some(frozen) = render_frozen_denies(gate.deny_rules()) {
                sub = sub.with_system_prompt(format!(
                    "{}\n\n## 禁止操作（父级冻结，不可执行）\n{frozen}",
                    p.system_prompt
                ));
            }
        }
        if config.quality.enabled {
            // C1 修复：delegate engine 的子 Agent 也需注入 QualityHook
            //（与主 agent 路径对称），此前此路径仅挂 gate 未挂 quality hook。
            let hook = deepseeknova_agent::quality::QualityHook::new(
                deepseeknova_security::quality::QualityPolicy::builtin(),
            );
            sub = sub.with_tool_hook(Arc::new(hook));
        }
        agents.insert(p.name.clone(), Arc::new(sub));
    }

    let mut engine = deepseeknova_agent::DelegateEngine::new(
        agents,
        config.delegate.max_concurrent,
        config.delegate.output_cap_tokens,
    );
    // M5：max_depth 透传——配置上限在生产路径生效（引擎按 max_depth 守门，
    // depth > max 拒绝；禁递归默认下根派发 depth=1 不受影响）。
    engine = engine.with_max_depth(config.delegate.max_depth);
    for p in &presets {
        engine.register_spec(p.name.clone(), p.spec.clone(), p.config_inputs.clone());
        // M3：markdown 声明/TOML 预设的 per-agent 模型覆盖 → RunInput.model_override
        //（内置/TOML 预设恒 None，无行为变化）。
        engine.register_model(p.name.clone(), p.model.clone());
    }
    if let Some(a) = attribution {
        engine = engine.with_attribution((*a).clone());
    }
    Arc::new(engine)
}

/// 把 deny 规则渲染为冻结清单文本（供子代理 system prompt 注入）。
/// 空规则返回 `None`（不产生追加）。
fn render_frozen_denies(rules: &[deepseeknova_permission::Rule]) -> Option<String> {
    if rules.is_empty() {
        return None;
    }
    let lines: Vec<String> = rules
        .iter()
        .map(|r| match &r.subject {
            Some(s) => format!("- {} {s}", r.tool),
            None => format!("- {}", r.tool),
        })
        .collect();
    Some(lines.join("\n"))
}

/// Build a [`SubAgentRunner`](deepseeknova_agent::SubAgentRunner) for
/// coordinator `Delegate` actions: sub-agent turns use `task_provider` (the
/// `task` model pointer), history compaction uses `compact_provider` (the
/// `compact` pointer), falling back to the task provider when `None`.
///
/// Presets and tool restrictions mirror the delegate engine: merged builtin
/// presets + `config.delegate.agents` overrides + `.deepseeknova/agents/*.md`
/// markdown declarations (AgentManifest channel), sandbox-aware builtin
/// tools, `"delegate"` always excluded (no recursion), no MCP tools.
pub fn build_sub_agent_runner(
    config: &Config,
    task_provider: Arc<dyn deepseeknova_provider::Provider>,
    compact_provider: Option<Arc<dyn deepseeknova_provider::Provider>>,
    frozen_denies: &[deepseeknova_permission::Rule],
    permission_gate: Option<Arc<deepseeknova_permission::PermissionGate>>,
    security: Option<deepseeknova_security::context::SecurityContext>,
    workspace_root: &std::path::Path,
) -> deepseeknova_agent::SubAgentRunner {
    use deepseeknova_core::Tool;

    // deny 冻结（prompt 层）：渲染一次，注入每个子代理 system prompt
    let frozen_lines: Vec<String> = render_frozen_denies(frozen_denies)
        .map(|s| s.lines().map(|l| l.to_string()).collect())
        .unwrap_or_default();

    // 子代理工具源（与委派引擎同款：沿用沙箱选择，不接 MCP 工具）。
    let base: Vec<Arc<dyn Tool>> = if config.sandbox.enabled {
        let sandbox: Arc<dyn deepseeknova_sandbox::Sandbox> =
            Arc::from(deepseeknova_sandbox::platform_sandbox_tiered(
                deepseeknova_sandbox::SandboxTier::WorkspaceWrite,
                &sandbox_writable_paths(config, workspace_root),
                config.sandbox.allow_network,
            ));
        deepseeknova_tools::all_builtin_tools_with_sandbox(sandbox)
    } else {
        deepseeknova_tools::all_builtin_tools()
    };

    let mut runner = deepseeknova_agent::SubAgentRunner::new(task_provider);
    let allow_recursion = config.delegate.allow_recursion;
    let max_depth = config.delegate.max_depth.max(1);
    for p in merged_delegate_presets_with_manifests(config, workspace_root) {
        let sub_tools: Vec<Arc<dyn Tool>> = if allow_recursion {
            // 递归开启：子代理可再派子代理（RecursiveDelegateTool 自带深度守门）。
            let mut tools: Vec<Arc<dyn Tool>> = base
                .iter()
                .filter(|t| {
                    let n = t.schema().name;
                    p.spec.tools.iter().any(|allow| allow == &n)
                })
                .cloned()
                .collect();
            tools.push(Arc::new(deepseeknova_agent::RecursiveDelegateTool::new(
                max_depth,
            )));
            tools
        } else {
            // 禁递归（默认）：即便配置误加 "delegate" 也剔除。
            base.iter()
                .filter(|t| {
                    let n = t.schema().name;
                    n != "delegate" && p.spec.tools.iter().any(|allow| allow == &n)
                })
                .cloned()
                .collect()
        };
        runner.register(
            deepseeknova_agent::SubAgentConfig::new(p.name.clone(), p.system_prompt.clone())
                // P2 修复：把预设的任务书（含 inputs 声明与 task 模板）接入
                // SubAgentRunner 渲染路径。当前 config 只能产出 simple spec
                // （无 inputs 声明 → render 返回空 → 行为与接线前完全一致）；
                // 未来 spec 声明源就位后此路径立即生效。with_tools/with_max_steps
                // 在其后调用，保持 spec.tools/spec.max_steps 与执行参数同步。
                .with_task_spec(p.spec.clone())
                .with_frozen_denies(frozen_lines.clone())
                .with_tools(sub_tools)
                .with_max_steps(p.spec.max_steps)
                // M3：markdown 声明的 per-agent 模型覆盖透传（TOML 预设恒 None；
                // 生效需装配 ModelResolver，未装配时 warn 并回退默认 provider）。
                .with_model(p.model.clone())
                .with_config_inputs(p.config_inputs.clone()),
        );
    }
    if allow_recursion {
        // 装配递归派发出口：子代理再派子代理时经本 runner 自身（深度守门）。
        runner.set_delegation_sink(Arc::new(runner.clone()));
    }
    if let Some(threshold) = derive_compaction_threshold(config) {
        runner = runner.with_compaction_threshold(threshold);
    }
    if let Some(compact) = compact_provider {
        runner = runner.with_compact_provider(compact);
    }
    if let Some(gate) = permission_gate {
        // 执行层权限强制：子代理工具调用在 execute 前经 gate 检查
        //（与 Agent 型 delegate engine 的 with_permission_gate 对齐）。
        runner = runner.with_permission_gate(gate);
    }
    if config.quality.enabled {
        // 任务质量闭环：子代理路径与主 agent 对称注入 QualityHook
        //（禁写路径 / secret 检测 / 写后策略评估）。
        // C1 修复：此前子代理工具执行段未挂任何钩子链，secret 写入与
        // 禁写路径策略在子代理路径整体失效。
        let hook = deepseeknova_agent::quality::QualityHook::new(
            deepseeknova_security::quality::QualityPolicy::builtin(),
        );
        runner = runner.with_tool_hook(Arc::new(hook));
    }
    if let Some(sec) = security {
        // 执行上下文装配：shell/fs/web 工具强依赖 SecurityContext
        //（缺失时 enforce_capability 直接报错，子代理工具面不可用）。
        runner = runner.with_security(sec);
    }
    runner = runner.with_workspace_root(workspace_root);
    runner
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    // --- build_sub_agent_runner (coordinator Delegate wiring) ---

    /// Counting stub: proves the compact provider (not the task provider)
    /// served the compaction request.
    struct CountingProvider {
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl deepseeknova_provider::Provider for CountingProvider {
        async fn generate(
            &self,
            _validated: deepseeknova_provider::ValidatedRequest<'_>,
        ) -> anyhow::Result<deepseeknova_core::Message> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(deepseeknova_core::Message {
                role: deepseeknova_core::Role::Assistant,
                content: "digest".into(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            })
        }
    }

    #[tokio::test]
    async fn sub_agent_runner_registers_presets_and_uses_compact_provider() {
        use deepseeknova_core::Runner;
        use futures::StreamExt;

        let mut config = Config::default();
        // 阈值 1 token → 首步必触发压缩。
        config.agent.compaction_threshold_tokens = Some(1);

        let task = std::sync::Arc::new(stub_provider());
        let compact = std::sync::Arc::new(CountingProvider {
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let runner = build_sub_agent_runner(
            &config,
            task,
            Some(compact.clone()),
            &[],
            None,
            None,
            &std::env::temp_dir(),
        );

        let mut stream = runner
            .run_stream(deepseeknova_core::RunInput {
                prompt: "sub_agent:explorer\ngoal: investigate something long enough".into(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        assert!(
            compact.calls.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "compaction should go through the compact provider"
        );
    }

    #[test]
    fn sub_agent_runner_builds_with_delegate_tool_override() {
        // "delegate" 恒被过滤（与 build_delegate_engine 同款谓词）；此处验证
        // 含 delegate 覆盖的配置能安全构造。SubAgentRunner 无公开工具观测
        // 接口，过滤逻辑由 build_delegate_engine 的既有测试共同守护。
        let mut config = Config::default();
        config
            .delegate
            .agents
            .push(deepseeknova_config::DelegateAgentOverride {
                name: "explorer".into(),
                system_prompt: None,
                tools: Some(vec!["delegate".into(), "read_file".into()]),
                max_steps: None,
                inputs: None,
            });
        let task = std::sync::Arc::new(stub_provider());
        let _runner =
            build_sub_agent_runner(&config, task, None, &[], None, None, &std::env::temp_dir());
    }

    // -----------------------------------------------------------------------
    // M3：`.deepseeknova/agents/*.md` markdown 子代理声明通道接线
    // -----------------------------------------------------------------------

    #[test]
    fn delegate_engine_registers_markdown_agents() {
        let root = std::env::temp_dir().join(format!("dnv-m3-engine-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let agents_dir = root.join(".deepseeknova/agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("researcher.md"),
            "---\nname: researcher\ndescription: Read-only research.\ntools: [read_file]\nmax_turns: 4\n---\nYou are a researcher sub-agent.\n",
        )
        .unwrap();
        // 非 md 文件忽略
        std::fs::write(agents_dir.join("notes.txt"), "not a manifest").unwrap();

        let config = Config::default();
        let security = SecurityContext::with_safe_defaults();
        let engine = build_delegate_engine(
            &config,
            std::sync::Arc::new(stub_provider()),
            &root,
            &security,
            None,
            None,
            None,
            None,
        );
        let names = engine.agent_names();
        assert!(
            names.iter().any(|n| n == "researcher"),
            "markdown agent 'researcher' 必须注册，实得: {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "explorer"),
            "内置预设必须仍在: {names:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn delegate_engine_skips_missing_agents_dir() {
        let root = std::env::temp_dir().join(format!("dnv-m3-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let config = Config::default();
        let security = SecurityContext::with_safe_defaults();
        let engine = build_delegate_engine(
            &config,
            std::sync::Arc::new(stub_provider()),
            &root,
            &security,
            None,
            None,
            None,
            None,
        );
        // 缺目录 → 仅内置 4 预设（跳过，不阻断）。
        assert_eq!(
            engine.agent_names(),
            vec!["coder", "explorer", "reviewer", "tester"]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn delegate_engine_reads_custom_agents_dir_from_config() {
        let root = std::env::temp_dir().join(format!("dnv-m3-custom-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let custom = root.join("custom-agents");
        std::fs::create_dir_all(&custom).unwrap();
        std::fs::write(
            custom.join("auditor.md"),
            "---\nname: auditor\ntools: []\n---\nYou audit.\n",
        )
        .unwrap();

        let mut config = Config::default();
        config.delegate.agents_dir = Some(custom);
        let security = SecurityContext::with_safe_defaults();
        let engine = build_delegate_engine(
            &config,
            std::sync::Arc::new(stub_provider()),
            &root,
            &security,
            None,
            None,
            None,
            None,
        );
        assert!(
            engine.agent_names().iter().any(|n| n == "auditor"),
            "自定义 agents_dir 的 markdown 声明必须注册"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// M3：SubAgentRunner（coordinator Delegate 路径）同样注册 markdown 声明。
    #[tokio::test]
    async fn sub_agent_runner_registers_markdown_agents() {
        use deepseeknova_core::{Message, Role, Runner};
        use futures::StreamExt;
        use std::sync::Mutex;

        struct ManifestCaptureProvider {
            seen: Arc<Mutex<Vec<String>>>,
        }
        #[async_trait::async_trait]
        impl deepseeknova_provider::Provider for ManifestCaptureProvider {
            async fn generate(
                &self,
                v: deepseeknova_provider::ValidatedRequest<'_>,
            ) -> anyhow::Result<Message> {
                let mut texts: Vec<String> = v.messages.iter().map(|m| m.content.clone()).collect();
                self.seen.lock().unwrap().append(&mut texts);
                Ok(Message {
                    role: Role::Assistant,
                    content: "ok".into(),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                })
            }
        }

        let root = std::env::temp_dir().join(format!("dnv-m3-runner-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let agents_dir = root.join(".deepseeknova/agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("researcher.md"),
            "---\nname: researcher\ndescription: Read-only research.\ntools: [read_file]\n---\nYou are a researcher sub-agent.\n",
        )
        .unwrap();

        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let config = Config::default();
        let task: Arc<dyn deepseeknova_provider::Provider> =
            Arc::new(ManifestCaptureProvider { seen: seen.clone() });
        let runner = build_sub_agent_runner(&config, task, None, &[], None, None, &root);

        let mut stream = runner
            .run_stream(deepseeknova_core::RunInput {
                prompt: "sub_agent:researcher\ngoal: investigate".into(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        let all: String = seen.lock().unwrap().join("\n");
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            all.contains("You are a researcher sub-agent."),
            "markdown 声明的 system prompt 必须被 SubAgentRunner 使用，实得: {all}"
        );
    }

    // -----------------------------------------------------------------------
    // M5：delegate 引擎路径 max_depth 守门（配置上限透传）
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn delegate_engine_respects_config_max_depth() {
        use deepseeknova_agent::task_spec::InputValues;

        let root = std::env::temp_dir().join(format!("dnv-m5-depth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut config = Config::default();
        config.delegate.max_depth = 2;
        let security = SecurityContext::with_safe_defaults();
        let engine = build_delegate_engine(
            &config,
            std::sync::Arc::new(stub_provider()),
            &root,
            &security,
            None,
            None,
            None,
            None,
        );

        // depth 2 = 配置上限 → 放行。
        let ok = engine
            .run_at_depth("explorer", "investigate", InputValues::new(), 2)
            .await
            .unwrap();
        assert!(!ok.is_empty(), "depth=上限 应放行: {ok}");

        // depth 3 > max 2 → 拒绝（守门生效）。
        let err = engine
            .run_at_depth("explorer", "investigate", InputValues::new(), 3)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("recursion depth exceeded"),
            "超深派发必须被引擎拒绝，实得: {err}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

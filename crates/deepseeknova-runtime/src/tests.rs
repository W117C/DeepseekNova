// 组合根集成测试：主 agent 构建（build_agent*）、Runtime、MCP 发现等。
use super::*;
use crate::test_support::*;
use deepseeknova_config::Config;
use deepseeknova_context::ContextEngine;
use deepseeknova_core::memory::skill::{SkillExtractionConfig, SkillManager, SkillState};

/// 参数化任务书在 SubAgentRunner 路径的渲染生效证明：spec 含 inputs 声明
/// 与 `${{ inputs.x }}` 占位符时，prompt 协议 `input:` 行传入的值必须渲染
/// 进子代理消息（task 追加 User、RULES 追加 System）。无 spec/无 input 行
/// 时渲染为空 = 行为不变（既有 sub_agent_runner_registers_presets 测试守护）。
#[tokio::test]
async fn sub_agent_task_spec_inputs_render_into_prompt() {
    use deepseeknova_agent::task_spec::{InputSpec, InputType, TaskSpec};
    use deepseeknova_core::Runner;
    use futures::StreamExt;
    use std::sync::Mutex;

    // 捕获 provider 收到的全部消息文本。不覆写 stream：默认 stream 回退
    // 到 generate，在此处截获 messages 即可覆盖子代理每次调用的输入。
    struct CapturingProvider {
        seen: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait::async_trait]
    impl deepseeknova_provider::Provider for CapturingProvider {
        async fn generate(
            &self,
            v: deepseeknova_provider::ValidatedRequest<'_>,
        ) -> Result<deepseeknova_core::Message, deepseeknova_core::DeepseeknovaError> {
            let mut texts: Vec<String> = v.messages.iter().map(|m| m.content.clone()).collect();
            self.seen.lock().unwrap().append(&mut texts);
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

    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let provider: Arc<dyn deepseeknova_provider::Provider> =
        Arc::new(CapturingProvider { seen: seen.clone() });
    let spec = TaskSpec {
        name: "reviewer".into(),
        task: "Review ${{ inputs.path }} carefully".into(),
        rules: vec!["Do not modify files".into()],
        inputs: vec![InputSpec {
            name: "path".into(),
            ty: InputType::String,
            required: true,
            default: None,
        }],
        tools: Vec::new(),
        max_steps: 2,
    };
    let mut runner = deepseeknova_agent::SubAgentRunner::new(provider);
    runner.register(
        deepseeknova_agent::SubAgentConfig::new("reviewer", "you are a reviewer")
            .with_task_spec(spec)
            .with_max_steps(2),
    );
    let runner = runner.with_default("reviewer");

    let mut stream = runner
        .run_stream(deepseeknova_core::RunInput {
            prompt: "sub_agent:reviewer\ninput:path=src/lib.rs\ngoal:review the change".into(),
            images: Vec::new(),
            model_override: None,
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    let texts = seen.lock().unwrap();
    let all: String = texts.join("\n");
    assert!(
        all.contains("Review src/lib.rs carefully"),
        "占位符必须被 input: 行渲染替换，实得: {all}"
    );
    assert!(
        all.contains("## RULES\n- Do not modify files"),
        "RULES 必须渲染进消息，实得: {all}"
    );
}

/// H4 端到端回归：remote embedder 配置下，起点召回闭包内的同步 embed
/// （真实 HTTP 往返，服务端延迟 500ms）不得阻塞 tokio worker。
/// 断言：embed 阻塞窗口 [server_started, server_responded] 内心跳必须
/// 持续推进（证明 worker 已被 block_in_place 释放）。
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn recall_embed_does_not_starve_the_worker_thread() {
    let _guard = ENV_LOCK.lock().await;
    clear_proxy_env();
    use deepseeknova_core::Runner;
    use futures::StreamExt;
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    // 本地一次性 embed 服务：请求到达 → 记录时间 → 延迟 500ms → 回复。
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server_started = Arc::new(Mutex::new(None::<std::time::Instant>));
    let server_responded = Arc::new(Mutex::new(None::<std::time::Instant>));
    {
        let (ss, sr) = (server_started.clone(), server_responded.clone());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            // 读至 headers 结束（\r\n\r\n）；单条小 body 随 headers 同包到达。
            while buf.windows(4).all(|w| w != b"\r\n\r\n") {
                let n = stream.read(&mut tmp).unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            *ss.lock().unwrap() = Some(std::time::Instant::now());
            std::thread::sleep(std::time::Duration::from_millis(500));
            *sr.lock().unwrap() = Some(std::time::Instant::now());
            let body = r#"{"object":"list","data":[{"object":"embedding","index":0,"embedding":[0.1,0.2,0.3]}]}"#;
            let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            let _ = stream.write_all(resp.as_bytes());
        });
    }
    let base = format!("http://{addr}/v1");

    // 保存/恢复 embed key 环境变量（remote embedder 装配必需）。
    let prev_key = std::env::var("DEEPSEEKNOVA_EMBED_API_KEY").ok();
    std::env::set_var("DEEPSEEKNOVA_EMBED_API_KEY", "sk-h4-test");
    let restore_env = || match &prev_key {
        Some(v) => std::env::set_var("DEEPSEEKNOVA_EMBED_API_KEY", v),
        None => std::env::remove_var("DEEPSEEKNOVA_EMBED_API_KEY"),
    };

    let root = std::env::temp_dir().join(format!("dnv-h4-embed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let mut config = Config::default();
    config.memory.embedder = "remote".into();
    config.memory.embed_model = "test-model".into();
    config.memory.embed_base_url = base;

    let agent = build_agent(
        &config,
        root.clone(),
        std::sync::Arc::new(stub_provider()),
        5,
        None,
        vec![],
    )
    .expect("build_agent with remote embedder should succeed");

    // 心跳：2ms 周期记录 tick 时间戳。
    let ticks = Arc::new(Mutex::new(Vec::<std::time::Instant>::new()));
    let stop = Arc::new(AtomicBool::new(false));
    {
        let (tk, st) = (ticks.clone(), stop.clone());
        let heartbeat = tokio::spawn(async move {
            while !st.load(Ordering::SeqCst) {
                tk.lock().unwrap().push(std::time::Instant::now());
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            }
        });
        // run 起点召回触发 embed（HTTP 500ms 阻塞窗口）。
        let mut stream = agent
            .run_stream(deepseeknova_core::RunInput {
                prompt: "h4 run".into(),
                images: Vec::new(),
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}
        stop.store(true, Ordering::SeqCst);
        heartbeat.await.unwrap();
    }

    restore_env();
    let start = server_started
        .lock()
        .unwrap()
        .expect("embed request must arrive");
    let end = server_responded
        .lock()
        .unwrap()
        .expect("embed must respond");
    let all_ticks = ticks.lock().unwrap().clone();
    let in_window = all_ticks.iter().any(|t| t >= &start && t <= &end);
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        in_window,
        "embed 阻塞窗口 {start:?}..{end:?} 内无心跳（{} 个 tick 均在外）：worker 被占用",
        all_ticks.len()
    );
}

#[tokio::test]
async fn build_agent_wires_graph_when_enabled() {
    let mut config = Config::default();
    config.graph.enabled = true;
    let root = std::env::temp_dir().join(format!("dnv-graph-wire-{}", std::process::id()));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/x.rs"), "pub fn foo() {}\n").unwrap();

    let provider = std::sync::Arc::new(stub_provider());
    let agent = build_agent(&config, root.clone(), provider, 5, None, vec![]).unwrap();
    let names = agent.tool_names();
    assert!(names.iter().any(|n| n == "search_code"));
    assert!(names.iter().any(|n| n == "traverse_graph"));
    assert!(names.iter().any(|n| n == "retrieve_entity"));
    assert!(names.iter().any(|n| n == "trace_code"));
    assert!(names.iter().any(|n| n == "impact_code"));
    assert!(names.iter().any(|n| n == "explore_code"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn build_agent_skips_graph_when_disabled() {
    let mut config = Config::default();
    config.graph.enabled = false;
    let provider = std::sync::Arc::new(stub_provider());
    let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
    assert!(!agent.tool_names().iter().any(|n| n == "search_code"));
    assert!(!agent.tool_names().iter().any(|n| n == "traverse_graph"));
    assert!(!agent.tool_names().iter().any(|n| n == "retrieve_entity"));
    assert!(!agent.tool_names().iter().any(|n| n == "trace_code"));
    assert!(!agent.tool_names().iter().any(|n| n == "impact_code"));
    assert!(!agent.tool_names().iter().any(|n| n == "explore_code"));
}

#[tokio::test]
async fn build_agent_registers_memory_tools_when_enabled() {
    let mut config = Config::default();
    config.memory.enabled = true;
    config.graph.enabled = false;
    let root = std::env::temp_dir().join(format!("dnv-mem-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let provider = std::sync::Arc::new(stub_provider());
    let agent = build_agent(&config, root.clone(), provider, 5, None, vec![]).unwrap();
    let names = agent.tool_names();
    assert!(names.iter().any(|n| n == "recall"));
    assert!(names.iter().any(|n| n == "remember"));
    assert!(
        names.iter().any(|n| n == "context7_docs"),
        "文档检索工具应常驻注册"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn build_agent_skips_memory_tools_when_disabled() {
    let mut config = Config::default();
    config.memory.enabled = false;
    config.graph.enabled = false;
    let provider = std::sync::Arc::new(stub_provider());
    let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
    assert!(!agent.tool_names().iter().any(|n| n == "recall"));
}

#[tokio::test]
async fn build_agent_wires_llm_distill_and_runs_without_panic() {
    use futures::StreamExt;
    let mut config = Config::default();
    config.memory.enabled = true;
    config.memory.llm_distill = true;
    config.graph.enabled = false;
    config.verify.enabled = false;
    config.review.enabled = false;
    let root = std::env::temp_dir().join(format!("dnv-llm-distill-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let provider = std::sync::Arc::new(stub_provider());
    let agent = build_agent(&config, root.clone(), provider, 5, None, vec![]).unwrap();

    // 跑一轮：stub 返回空文本，事件流正常结束（Done 或跑满步数 Paused）；
    // LLM 蒸馏不可解析 → 静默跳过，不 panic。
    let mut stream = agent
        .run_stream(RunInput {
            prompt: "hi".into(),
            images: vec![],
            model_override: None,
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    // 记忆引擎仍可打开并列出（蒸馏失败不影响记忆库可用性）。
    let engine = deepseeknova_core::memory::engine::MemoryEngine::open(
        root.join(".deepseeknova/memory.db"),
        true,
    )
    .unwrap();
    let _ = engine
        .list(deepseeknova_core::memory::store::MemoryCategory::Skill)
        .unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

/// 递归列出 skills 目录（诊断辅助）。
fn walk_skills_tree(dir: &std::path::Path) -> std::io::Result<()> {
    if !dir.exists() {
        eprintln!("[diag]   (skills dir does not exist)");
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            eprintln!("[diag]   dir: {}", path.display());
            let _ = walk_skills_tree(&path);
        } else {
            eprintln!("[diag]   file: {}", path.display());
        }
    }
    Ok(())
}

/// 设计 C 集成测试：蒸馏产出 → auto/ 落盘（frontmatter 含 source: distill）
/// → reload 后状态保持 → recall 匹配注入。
#[tokio::test]
async fn build_agent_distill_writes_auto_skill_and_recall_injects() {
    use futures::StreamExt;
    let mut config = Config::default();
    config.memory.enabled = true;
    config.memory.llm_distill = true;
    // stub provider 不产生工具调用 → 蒸馏门槛调低，保证 skill 分支落盘
    config.memory.min_tool_calls = 0;
    config.memory.min_steps = 0;
    config.graph.enabled = false;
    config.verify.enabled = false;
    config.review.enabled = false;
    let root = std::env::temp_dir().join(format!("dnv-skill-hot-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    // Provider 返回可解析的蒸馏 JSON（主循环视作普通 assistant 文本，
    // 回合结束蒸馏可解析 → 落盘 skill）。
    struct SkillProvider;
    #[async_trait::async_trait]
    impl deepseeknova_provider::Provider for SkillProvider {
        async fn generate(
            &self,
            _v: deepseeknova_provider::ValidatedRequest<'_>,
        ) -> Result<deepseeknova_core::Message, deepseeknova_core::DeepseeknovaError> {
            Ok(deepseeknova_core::Message {
                    role: deepseeknova_core::Role::Assistant,
                    content: r#"{"kind":"skill","title":"Fix Auth Flow","body":"Validate tokens first","tags":["auth"]}"#
                        .into(),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
reasoning_content: None,
reasoning_signature: None,
                })
        }
    }
    let provider: Arc<dyn deepseeknova_provider::Provider> = Arc::new(SkillProvider);
    let agent = build_agent(&config, root.clone(), provider, 5, None, vec![]).unwrap();
    let mut stream = agent
        .run_stream(RunInput {
            prompt: "hi".into(),
            images: vec![],
            model_override: None,
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    // 异步蒸馏 spawn 无法 join → 轮询等待落盘文件出现。
    let auto_path = root.join(".deepseeknova/skills/auto/fix-auth-flow.md");
    // 诊断：确认 run 后蒸馏目录与记忆库状态。
    let skills_dir = root.join(".deepseeknova/skills");
    let db_path = root.join(".deepseeknova/memory.db");
    eprintln!(
        "[diag] after run: skills_dir={:?} exists={} db={:?} exists={}",
        skills_dir,
        skills_dir.exists(),
        db_path,
        db_path.exists()
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !auto_path.exists() && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    if !auto_path.exists() {
        eprintln!("[diag] auto skill NOT written after 5s; skills tree:");
        let _ = walk_skills_tree(&skills_dir);
    }
    assert!(auto_path.exists(), "蒸馏 skill 应落盘 auto/ 子目录");
    let content = std::fs::read_to_string(&auto_path).unwrap();
    assert!(
        content.contains("source: distill"),
        "frontmatter 必须含 source: distill"
    );
    assert!(content.contains("state: draft"), "初始态必须是 draft");

    // reload 后状态保持 + recall 注入前置：全新实例重开同一目录
    let m = SkillManager::new(SkillExtractionConfig {
        skill_dir: root.join(".deepseeknova/skills"),
        ..Default::default()
    });
    assert_eq!(m.skill_state("fix-auth-flow"), Some(SkillState::Draft));
    let matched = m.find_matching_skills("auth");
    assert!(
        !matched.is_empty(),
        "reload 后 recall 应能匹配到该 distill skill"
    );
    assert!(
        matched
            .iter()
            .any(|s| s.frontmatter.name == "fix-auth-flow"),
        "匹配结果应含 fix-auth-flow"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn build_agent_wires_reflection_and_runs_without_panic() {
    use futures::StreamExt;
    let mut config = Config::default(); // reflect_on_failure 默认 true
    config.graph.enabled = false;
    config.verify.enabled = false;
    config.review.enabled = false;
    let root = std::env::temp_dir().join(format!("dnv-reflect-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let provider = std::sync::Arc::new(stub_provider());
    let agent = build_agent(&config, root.clone(), provider, 5, None, vec![]).unwrap();

    // 一轮 run 正常结束（无失败回炉则反思不触发，但装配路径必须不 panic）。
    let mut stream = agent
        .run_stream(RunInput {
            prompt: "hi".into(),
            images: vec![],
            model_override: None,
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    // 反思教训钩子挂在记忆引擎上，库仍可打开。
    let engine = deepseeknova_core::memory::engine::MemoryEngine::open(
        root.join(".deepseeknova/memory.db"),
        true,
    )
    .unwrap();
    let _ = engine
        .list(deepseeknova_core::memory::store::MemoryCategory::Skill)
        .unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn build_agent_registers_delegate_tool_when_enabled() {
    let mut config = Config::default();
    config.delegate.enabled = true;
    config.graph.enabled = false;
    config.memory.enabled = false;
    let provider = std::sync::Arc::new(stub_provider());
    let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
    assert!(agent.tool_names().iter().any(|n| n == "delegate"));
}

#[tokio::test]
async fn build_agent_with_task_provider_compiles_and_registers_delegate() {
    let mut config = Config::default();
    config.delegate.enabled = true;
    config.graph.enabled = false;
    config.memory.enabled = false;
    let main: Arc<dyn deepseeknova_provider::Provider> = Arc::new(stub_provider());
    let task: Arc<dyn deepseeknova_provider::Provider> = Arc::new(stub_provider());
    let agent = build_agent_with_task_provider(
        &config,
        std::env::temp_dir(),
        main,
        Some(task),
        0,
        None,
        vec![],
    )
    .unwrap();
    assert!(agent.tool_names().iter().any(|n| n == "delegate"));
}

#[test]
fn build_agent_skips_delegate_when_disabled() {
    let mut config = Config::default();
    config.delegate.enabled = false;
    config.graph.enabled = false;
    config.memory.enabled = false;
    let provider = std::sync::Arc::new(stub_provider());
    let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
    assert!(!agent.tool_names().iter().any(|n| n == "delegate"));
}

// A no-op tool with a caller-chosen name, used to exercise extra_tools.
struct NamedStubTool(&'static str);

#[async_trait::async_trait]
impl deepseeknova_core::Tool for NamedStubTool {
    fn schema(&self) -> deepseeknova_core::types::ToolSchema {
        deepseeknova_core::types::ToolSchema {
            name: self.0.to_string(),
            description: "stub".into(),
            parameters: serde_json::json!({"type": "object"}),
        }
    }
    async fn execute(
        &self,
        _ctx: &deepseeknova_core::tool::ToolContext,
        _args: &str,
    ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
        Ok(String::new())
    }
}

#[test]
fn build_agent_registers_extra_tools() {
    let mut config = Config::default();
    config.graph.enabled = false;
    config.memory.enabled = false;
    let provider = std::sync::Arc::new(stub_provider());
    let extra: Vec<Arc<dyn deepseeknova_core::Tool>> =
        vec![Arc::new(NamedStubTool("mcp__srv__do_thing"))];
    let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, extra).unwrap();
    assert!(agent.tool_names().iter().any(|n| n == "mcp__srv__do_thing"));
}

#[test]
fn build_agent_skips_extra_tool_disabled_via_overrides() {
    let mut config = Config::default();
    config.graph.enabled = false;
    config.memory.enabled = false;
    config.tools.overrides = vec![deepseeknova_config::ToolOverride {
        name: "mcp__srv__do_thing".into(),
        disabled: true,
        timeout_secs: None,
        max_file_size: None,
    }];
    let provider = std::sync::Arc::new(stub_provider());
    let extra: Vec<Arc<dyn deepseeknova_core::Tool>> =
        vec![Arc::new(NamedStubTool("mcp__srv__do_thing"))];
    let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, extra).unwrap();
    assert!(!agent.tool_names().iter().any(|n| n == "mcp__srv__do_thing"));
}

#[tokio::test]
async fn discover_mcp_tools_empty_config_returns_empty() {
    let config = Config::default();
    assert!(discover_mcp_tools(&config).await.is_empty());
}

#[test]
fn build_agent_applies_b2_config() {
    let mut config = Config::default();
    config.graph.enabled = false;
    config.memory.enabled = false;
    config.agent.on_max_steps = "error".into();
    config.agent.l3_compaction = false;
    config.budget.enabled = false;
    let provider = std::sync::Arc::new(stub_provider());
    // 只验证可构建不 panic（字段私有，行为断言在 agent 侧已覆盖）。
    let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
    let _ = agent;
}

#[test]
fn build_agent_with_review_enabled_constructs() {
    let mut config = Config::default();
    config.graph.enabled = false;
    config.memory.enabled = false;
    config.review.enabled = true; // review_model 空 → 复用主 provider
    let provider = std::sync::Arc::new(stub_provider());
    let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
    let _ = agent;
}

#[test]
fn role_providers_review_injection_wins_over_review_model() {
    // review 注入胜过 review_model 直连回退（同 compact 优先级语义）。
    let mut config = Config::default();
    config.graph.enabled = false;
    config.memory.enabled = false;
    config.review.enabled = true;
    config.review.review_model = "no-such-model".into();
    let main_p = std::sync::Arc::new(stub_provider());
    let review_p: std::sync::Arc<dyn deepseeknova_provider::Provider> =
        std::sync::Arc::new(stub_provider());
    let roles = AgentRoleProviders {
        review: Some(review_p),
        ..Default::default()
    };
    let agent = build_agent_with_role_providers(
        &config,
        std::env::temp_dir(),
        main_p,
        roles,
        5,
        None,
        vec![],
        None,
    )
    .unwrap();
    let _ = agent;
}

#[test]
fn role_providers_compact_injection_wins_over_compact_model() {
    let mut config = Config::default();
    config.graph.enabled = false;
    config.memory.enabled = false;
    // compact_model 指向一个不存在的模型名——若直连回退被错误执行，
    // resolve 失败仅告警不报错，因此用注入路径成功构建 + 后续分支
    // 测试共同界定优先级语义。
    config.agent.compact_model = "no-such-model".into();
    let main_p = std::sync::Arc::new(stub_provider());
    let compact_p: std::sync::Arc<dyn deepseeknova_provider::Provider> =
        std::sync::Arc::new(stub_provider());
    let roles = AgentRoleProviders {
        task: None,
        compact: Some(compact_p),
        ..Default::default()
    };
    let agent = build_agent_with_role_providers(
        &config,
        std::env::temp_dir(),
        main_p,
        roles,
        5,
        None,
        vec![],
        None,
    )
    .unwrap();
    let _ = agent; // 注入路径构建成功；Agent 侧字段私有，行为由 agent crate 测试覆盖
}

#[test]
fn role_providers_default_falls_back_to_compact_model_path() {
    // roles 全 None + compact_model 非空 → 走 B2 直连回退（构建不 panic，
    // 解析失败仅告警）。与旧 build_agent 行为等价。
    let mut config = Config::default();
    config.graph.enabled = false;
    config.memory.enabled = false;
    config.agent.compact_model = "no-such-model".into();
    let provider = std::sync::Arc::new(stub_provider());
    let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
    let _ = agent;
}

#[test]
fn runtime_builds_with_default_config() {
    let config = Config::default();
    // Use a temp dir to avoid scanning the full project tree
    let dir = std::env::temp_dir().join(format!("deepseeknova-rt-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let context = ContextEngine::new(dir.clone()).unwrap();
    let context: Arc<dyn ContextProvider> = Arc::new(context);

    let runtime = Runtime::new(config, context).unwrap();
    assert_eq!(runtime.events.receiver_count(), 0);

    let _ = std::fs::remove_dir_all(&dir);
}

fn mid_run_test_workspace(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "dnv-midrun-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn mid_run_config_off_leaves_agent_unwired() {
    let mut config = Config::default();
    config.graph.enabled = false;
    config.memory.mid_run_recall = false;
    let workspace = mid_run_test_workspace("off");
    let provider = std::sync::Arc::new(stub_provider());
    let agent = build_agent_with_role_providers(
        &config,
        workspace.clone(),
        provider,
        AgentRoleProviders::default(),
        5,
        None,
        vec![],
        None,
    )
    .unwrap();
    assert!(
        !agent.mid_run_retrieval_enabled(),
        "mid_run_recall=false must not wire mid-run retrieval"
    );
    let _ = std::fs::remove_dir_all(&workspace);
}

#[test]
fn mid_run_config_on_wires_retrieval() {
    let mut config = Config::default();
    config.graph.enabled = false;
    config.memory.mid_run_recall = true;
    let workspace = mid_run_test_workspace("on");
    let provider = std::sync::Arc::new(stub_provider());
    let agent = build_agent_with_role_providers(
        &config,
        workspace.clone(),
        provider,
        AgentRoleProviders::default(),
        5,
        None,
        vec![],
        None,
    )
    .unwrap();
    assert!(
        agent.mid_run_retrieval_enabled(),
        "mid_run_recall=true must wire mid-run retrieval"
    );
    let _ = std::fs::remove_dir_all(&workspace);
}

#[test]
fn graph_retrieval_hint_stays_english_and_graph_first() {
    for tool in ["search_code", "traverse_graph", "retrieve_entity"] {
        assert!(GRAPH_RETRIEVAL_HINT.contains(tool), "hint missing {tool}");
    }
    assert!(
        !GRAPH_RETRIEVAL_HINT.contains("检索"),
        "hint must be English, not Chinese"
    );
}

#[test]
fn build_agent_with_attribution_enabled_constructs() {
    // [attribution] enabled=true 时主 agent 与 delegate 引擎都装配归因
    // （字段私有，行为由 agent crate 测试覆盖；此处验证装配路径不 panic）。
    let mut config = Config::default();
    config.graph.enabled = false;
    config.memory.enabled = false;
    config.delegate.enabled = true;
    config.attribution.enabled = true;
    config.attribution.max_retries = 2;
    config.attribution.max_attributions = 5;
    config.attribution.degrade_map =
        std::collections::HashMap::from([("researcher".to_string(), "explorer".to_string())]);
    let provider = std::sync::Arc::new(stub_provider());
    let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
    let _ = agent;
}

#[test]
fn build_agent_with_attribution_disabled_matches_legacy() {
    // 默认（enabled=false）：不调用 with_attribution，行为零变化；装配路径不 panic。
    let mut config = Config::default();
    config.graph.enabled = false;
    config.memory.enabled = false;
    config.delegate.enabled = true;
    let provider = std::sync::Arc::new(stub_provider());
    let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
    let _ = agent;
}

/// 预置一个 distill draft skill（命中 recall 查询 "auth..."）。
fn seed_skill(skills_dir: &std::path::Path, title: &str, tags: Vec<&str>) {
    let mut sm = SkillManager::new(SkillExtractionConfig {
        skill_dir: skills_dir.to_path_buf(),
        ..Default::default()
    });
    sm.create_distilled_skill(
        title,
        "Validate tokens first",
        tags.into_iter().map(str::to_string).collect(),
        Some("seed"),
    )
    .unwrap();
}

/// env 快照守卫：测试结束恢复变量原值（防并行测试互相污染）。
struct EnvRestore(Vec<(&'static str, Option<String>)>);

impl Drop for EnvRestore {
    fn drop(&mut self) {
        for (key, value) in &self.0 {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}

/// 语义嵌入 fail-open：embedder=remote 但缺 key → 装配不炸，run 照常完成
/// （try_memory_embedder 返回 None，recall 回落纯 FTS）。
#[tokio::test]
async fn remote_embedder_without_key_falls_back_to_fts() {
    use deepseeknova_core::Runner;
    use futures::StreamExt;

    // 与 H4（recall_embed_does_not_starve_the_worker_thread）共享 ENV_LOCK：
    // 本测试会移除 embed key，若并行执行会把 H4 刚装配的 key 删掉，
    // 导致 embed 请求不发（embed request must arrive 失败）。
    let _guard = ENV_LOCK.lock().await;
    let _env = EnvRestore(vec![
        (
            "DEEPSEEKNOVA_EMBED_API_KEY",
            std::env::var("DEEPSEEKNOVA_EMBED_API_KEY").ok(),
        ),
        ("OPENAI_API_KEY", std::env::var("OPENAI_API_KEY").ok()),
    ]);
    std::env::remove_var("DEEPSEEKNOVA_EMBED_API_KEY");
    std::env::remove_var("OPENAI_API_KEY");

    let root = std::env::temp_dir().join(format!("dnv-embed-failopen-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let mut config = Config::default();
    config.memory.enabled = true;
    config.memory.embedder = "remote".to_string();
    config.memory.embed_model = "text-embedding-3-small".to_string();
    config.graph.enabled = false;
    config.review.enabled = false;
    config.verify.enabled = false;
    config.delegate.enabled = false;
    config.memory.llm_distill = false;
    let provider: Arc<dyn deepseeknova_provider::Provider> = Arc::new(stub_provider());
    let agent = build_agent(&config, root.clone(), provider, 5, None, vec![]).unwrap();
    let mut stream = agent
        .run_stream(deepseeknova_core::RunInput {
            prompt: "auth".into(),
            images: vec![],
            model_override: None,
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}
    let _ = std::fs::remove_dir_all(&root);
}

/// P2 回归：超 budget 场景下只对「实际注入 prompt」的 skill 计 use。
/// 匹配多项但字符预算只容得下第一项（verified 排 draft 前，顺序确定），
/// 断言注入项 use_count +1、未注入项 use_count 保持 0（draft 不因
/// 「匹配即计 use」被污染晋升）。
#[tokio::test]
async fn skill_recall_counts_only_injected_skills() {
    use deepseeknova_core::Runner;
    use futures::StreamExt;

    let root = std::env::temp_dir().join(format!("dnv-skill-budget-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let skills_dir = root.join(".deepseeknova/skills");
    {
        let mut sm = SkillManager::new(SkillExtractionConfig {
            skill_dir: skills_dir.clone(),
            ..Default::default()
        });
        // Alpha：先经 3 次 record_use 升为 verified（rank 0，注入时排最前）。
        sm.create_distilled_skill(
            "Auth Alpha",
            "fix auth",
            vec!["auth".to_string()],
            Some("seed"),
        )
        .unwrap();
        for _ in 0..deepseeknova_core::memory::skill::VERIFY_USE_THRESHOLD {
            sm.record_use("auth-alpha", true, Some("seed")).unwrap();
        }
        // Beta：draft（rank 2），描述超长 → 预算不足时排 Alpha 后 break。
        sm.create_distilled_skill(
            "Auth Beta",
            &"very long description ".repeat(6),
            vec!["auth".to_string()],
            Some("seed"),
        )
        .unwrap();
    }

    let mut config = Config::default();
    config.memory.enabled = true;
    // 预算收紧：cap_chars = 10*4 = 40 字符。header（20）+ Alpha 行（27）
    // 累计 47 > 40 → Alpha 注入后 Beta 的 check 必 break → 未注入。
    // （check 语义：注入前比较「已累计 lines.len() > budget」。）
    config.memory.recall_inject_tokens = 10;
    config.memory.recall_top_k = 3;
    config.graph.enabled = false;
    config.review.enabled = false;
    config.verify.enabled = false;
    config.delegate.enabled = false;
    config.memory.llm_distill = false;
    let provider: Arc<dyn deepseeknova_provider::Provider> = Arc::new(stub_provider());
    let agent = build_agent(&config, root.clone(), provider, 5, None, vec![]).unwrap();
    let mut stream = agent
        .run_stream(deepseeknova_core::RunInput {
            prompt: "auth".into(),
            images: vec![],
            model_override: None,
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    let m = SkillManager::new(SkillExtractionConfig {
        skill_dir: skills_dir.clone(),
        ..Default::default()
    });
    let find = |name: &str| -> usize {
        m.list_skills()
            .iter()
            .find(|s| s.frontmatter.name == name)
            .map(|s| s.frontmatter.use_count)
            .unwrap_or(0) as usize
    };
    assert_eq!(
        find("auth-alpha"),
        deepseeknova_core::memory::skill::VERIFY_USE_THRESHOLD as usize + 1,
        "注入项（Alpha）应计 1 次 use（3 次预置 + 1 次注入）"
    );
    assert_eq!(find("auth-beta"), 0, "未注入项（Beta）use_count 不得增长");
    // Beta 未达阈值 → 仍为 draft（未被污染晋升）。
    assert_eq!(m.skill_state("auth-beta"), Some(SkillState::Draft));
    let _ = std::fs::remove_dir_all(&root);
}

/// 设计 C 三态闭环集成测试：recall 命中 → record_use → 跨 build 会话推进
/// → draft → verified → active；会话边界（run 结束）prune 超额 draft。
#[tokio::test]
async fn skill_use_loop_closes_via_recall_record_use_and_prune() {
    use deepseeknova_core::Runner;
    use futures::StreamExt;

    let root = std::env::temp_dir().join(format!("dnv-skill-loop-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let skills_dir = root.join(".deepseeknova/skills");
    seed_skill(&skills_dir, "Fix Auth Flow", vec!["auth"]);

    let mut config = Config::default();
    config.memory.enabled = true;
    config.graph.enabled = false;
    config.review.enabled = false;
    config.verify.enabled = false;
    config.delegate.enabled = false;
    config.memory.llm_distill = false; // 避免异步蒸馏干扰，聚焦 recall 闭环
    let provider: Arc<dyn deepseeknova_provider::Provider> = Arc::new(stub_provider());

    // 三次独立 build_agent = 三个会话边界；每次 recall 命中即 record_use。
    // 注意 find_matching_skills 的 strong 匹配是「skill name/tag 包含 query」，
    // 故 query 用短词 "auth"（tag "auth" 命中）。
    for _ in 0..3 {
        let agent = build_agent(&config, root.clone(), provider.clone(), 5, None, vec![]).unwrap();
        let mut stream = agent
            .run_stream(deepseeknova_core::RunInput {
                prompt: "auth".into(),
                images: vec![],
                model_override: None,
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}
    }

    // use_count=3 → verified；sessions_seen=3（每 build 新会话 id）→ active
    let m = SkillManager::new(SkillExtractionConfig {
        skill_dir: skills_dir.clone(),
        ..Default::default()
    });
    assert_eq!(
        m.skill_state("fix-auth-flow"),
        Some(SkillState::Active),
        "三态推进必须到达 active"
    );
    let content = std::fs::read_to_string(skills_dir.join("auto/fix-auth-flow.md")).unwrap();
    assert!(content.contains("use_count: 3"), "use_count 必须落盘为 3");
    assert!(content.contains("state: active"), "state 必须落盘为 active");

    // 清理阶段：再灌 22 个不匹配的 draft，跑一轮 → 会话边界 prune 到 20 个
    // draft（active 豁免），auto/ 下共 21 个文件。
    {
        let mut sm = SkillManager::new(SkillExtractionConfig {
            skill_dir: skills_dir.clone(),
            ..Default::default()
        });
        for i in 0..22 {
            sm.create_distilled_skill(
                &format!("Noise Skill {i:02}"),
                "noise",
                vec![],
                Some("seed"),
            )
            .unwrap();
        }
    }
    let agent = build_agent(&config, root.clone(), provider, 5, None, vec![]).unwrap();
    let mut stream = agent
        .run_stream(deepseeknova_core::RunInput {
            prompt: "unrelated topic".into(),
            images: vec![],
            model_override: None,
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    let auto_dir = skills_dir.join("auto");
    let count = std::fs::read_dir(&auto_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .count();
    assert_eq!(
        count, 21,
        "会话边界应清理超额 draft：22 draft + 1 active(豁免) → 20 + 1 = 21，实得 {count}"
    );
    // active skill 未被清理
    assert!(auto_dir.join("fix-auth-flow.md").exists());
    let _ = std::fs::remove_dir_all(&root);
}

/// 配置装配：`[memory] max_auto_draft_skills` 覆盖默认 20，且用户手写与
/// verified 始终豁免清理。
#[tokio::test]
async fn skill_prune_honors_configured_max_auto_draft_skills() {
    use deepseeknova_core::Runner;
    use futures::StreamExt;

    let root = std::env::temp_dir().join(format!("dnv-skill-prune-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let skills_dir = root.join(".deepseeknova/skills");
    {
        let mut sm = SkillManager::new(SkillExtractionConfig {
            skill_dir: skills_dir.clone(),
            ..Default::default()
        });
        // 5 个 draft
        for i in 0..5 {
            sm.create_distilled_skill(&format!("Draft {i}"), "d", vec![], Some("seed"))
                .unwrap();
        }
        // 1 个 verified（豁免）
        sm.create_distilled_skill("Keep Verified", "v", vec![], Some("seed"))
            .unwrap();
        for _ in 0..deepseeknova_core::memory::skill::VERIFY_USE_THRESHOLD {
            sm.record_use("keep-verified", true, Some("seed")).unwrap();
        }
        // 1 个用户手写（豁免）
        sm.create_skill(deepseeknova_core::memory::skill::Skill {
            frontmatter: deepseeknova_core::memory::skill::SkillFrontmatter {
                name: "user-skill".into(),
                version: "1.0.0".into(),
                description: "user authored".into(),
                triggers: vec![],
                tags: vec![],
                created_at: "2026-01-01T00:00:00Z".into(),
                updated_at: "2026-01-01T00:00:00Z".into(),
                use_count: 0,
                success_count: 0,
                source_session: None,
            },
            body: "b".into(),
        })
        .unwrap();
    }

    let mut config = Config::default();
    config.memory.enabled = true;
    config.memory.max_auto_draft_skills = 2; // 覆盖默认 20
    config.graph.enabled = false;
    config.review.enabled = false;
    config.verify.enabled = false;
    config.delegate.enabled = false;
    config.memory.llm_distill = false;
    let provider: Arc<dyn deepseeknova_provider::Provider> = Arc::new(stub_provider());
    let agent = build_agent(&config, root.clone(), provider, 5, None, vec![]).unwrap();
    let mut stream = agent
        .run_stream(deepseeknova_core::RunInput {
            prompt: "no match here".into(),
            images: vec![],
            model_override: None,
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    // auto/ 下：2 draft + 1 verified = 3；用户手写文件仍在根目录
    let auto_dir = skills_dir.join("auto");
    let count = std::fs::read_dir(&auto_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .count();
    assert_eq!(
        count, 3,
        "应只保留 2 个 draft + 1 个 verified，实得 {count}"
    );
    assert!(
        skills_dir.join("user-skill.md").exists(),
        "用户手写 skill 必须豁免"
    );
    let m = SkillManager::new(SkillExtractionConfig {
        skill_dir: skills_dir.clone(),
        ..Default::default()
    });
    assert_eq!(m.skill_state("keep-verified"), Some(SkillState::Verified));
    let _ = std::fs::remove_dir_all(&root);
}

/// 任务书 P 任务 2（spec §13 #9 接线）：recall 注入侧收集器 → session_skills
/// → fitness record_use + record_result 全链路。预置技能文件，builder 传
/// `Some(session_skills)`，run 后：收集器含注入技能名；fitness.json 出现
/// 真实 use 记录（uses=1）与 result 记录（successes=1）；空集合场景（无
/// 注入）由 `fitness_empty_skills_skips_silently_and_writes_no_file` 覆盖。
#[tokio::test]
async fn recall_injection_collects_skills_and_fitness_records_use_and_result() {
    use deepseeknova_core::Runner;
    use futures::StreamExt;

    let root = std::env::temp_dir().join(format!("dsn-record-use-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    // 预置用户技能：name 含 "auth"（强匹配），body 弱匹配兜底。
    let skills_dir = root.join(".deepseeknova/skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(
            skills_dir.join("fix-auth.md"),
            "---\nname: fix-auth\nversion: 1.0.0\ndescription: Fix authentication flows\ntags: [auth]\n---\nValidate tokens before trusting them.\n",
        )
        .unwrap();
    let fitness_path = skills_dir.join("fitness.json");
    let metrics_dir = root.join(".deepseeknova/metrics");

    let mut config = Config::default();
    config.protocol.enabled = true;
    config.metrics.enabled = true;
    config.memory.enabled = true;
    config.graph.enabled = false;
    config.delegate.enabled = false;
    config.verify.enabled = false;
    config.review.enabled = false;

    let session_skills: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let agent = build_agent_with_role_providers(
        &config,
        root.clone(),
        Arc::new(stub_provider()),
        AgentRoleProviders::default(),
        5,
        None,
        vec![],
        Some(session_skills.clone()),
    )
    .unwrap();
    let agent = attach_metrics_hook_with_fitness(
        agent,
        &config,
        MetricsSink {
            ledger: Arc::new(deepseeknova_provider::cost::CostLedger::new()),
            prices: Default::default(),
            dir: metrics_dir,
        },
        &root,
        session_skills.clone(),
    );
    // prompt 含 "auth"：起点召回（unseeded 首轮）匹配 fix-auth →
    // 注入 prompt → 收集器写入技能名。
    let mut stream = agent
        .run_stream(deepseeknova_core::RunInput {
            prompt: "auth".into(),
            images: Vec::new(),
            model_override: None,
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    // 收集器含注入技能名。
    let collected = session_skills.lock().unwrap().clone();
    assert!(
        collected.iter().any(|s| s == "fix-auth"),
        "session_skills must contain injected skill, got {collected:?}"
    );
    // fitness.json：真实 use + result 记录（uses=1、successes=1）。
    assert!(
        fitness_path.exists(),
        "fitness.json must be written when skills were injected"
    );
    let store = deepseeknova_skills::fitness::FitnessStore::load(&fitness_path).unwrap();
    let snap = store.snapshot();
    assert_eq!(snap.len(), 1, "one skill recorded");
    assert_eq!(snap[0].skill, "fix-auth");
    assert_eq!(snap[0].uses, 1, "record_use must count the injection");
    assert_eq!(snap[0].successes, 1, "completed run → success");
    assert_eq!(snap[0].failures, 0);
    let _ = std::fs::remove_dir_all(&root);
}

/// 协议增强 §7.1 末条：task_rate 双端接线之「失败端」——Paused 路径
/// metrics hook 先触发（评分卡按保守 false/0 落盘），诊断回调随后按
/// failures 覆写：first_pass=false、retry_rounds=failures 条数（≥1）。
#[tokio::test]
async fn scorecard_task_rate_failed_run_backfilled_from_diagnose() {
    use deepseeknova_core::Runner;
    use futures::StreamExt;

    let root = std::env::temp_dir().join(format!("dsn-taskrate-fail-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let metrics_dir = root.join(".deepseeknova/metrics");

    let mut config = Config::default();
    config.protocol.enabled = true;
    config.metrics.enabled = true;
    config.graph.enabled = false;
    config.memory.enabled = false;
    config.delegate.enabled = false;
    let agent = attach_metrics_hook_with_fitness(
        deepseeknova_agent::Agent::new(Arc::new(EmptyProvider), 2),
        &config,
        MetricsSink {
            ledger: Arc::new(deepseeknova_provider::cost::CostLedger::new()),
            prices: Default::default(),
            dir: metrics_dir.clone(),
        },
        &root,
        Arc::new(std::sync::Mutex::new(Vec::new())),
    );
    // CLI 装配顺序：metrics → quality → diagnose → failure pattern → gates。
    // 此处按同序挂 diagnose（task_rate 回填依赖评分卡先落盘）。
    let agent = attach_diagnose_hook_with_ingest(agent, metrics_dir.clone(), Some(&config), &root);
    let mut stream = agent
        .run_stream(deepseeknova_core::RunInput {
            prompt: "hi".into(),
            images: Vec::new(),
            model_override: None,
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    // 诊断报告存在（Paused）且 failures 非空。
    let diag_dir = metrics_dir.join("diagnose");
    let diag_files: Vec<String> = std::fs::read_dir(&diag_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".json"))
        .collect();
    assert_eq!(diag_files.len(), 1, "one diagnose report written");
    let report: deepseeknova_agent::diagnose::DiagnoseReport =
        serde_json::from_str(&std::fs::read_to_string(diag_dir.join(&diag_files[0])).unwrap())
            .unwrap();
    assert!(!report.failures.is_empty(), "paused run must have failures");

    // 评分卡 task_rate 被诊断回调覆写为真实值。
    let files: Vec<String> = std::fs::read_dir(&metrics_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".scorecard.json"))
        .collect();
    assert_eq!(files.len(), 1, "one scorecard written");
    let card: deepseeknova_metrics::Scorecard =
        serde_json::from_str(&std::fs::read_to_string(metrics_dir.join(&files[0])).unwrap())
            .unwrap();
    assert!(!card.first_pass, "paused run must not be first_pass");
    assert_eq!(
        card.retry_rounds as usize,
        report.failures.len(),
        "retry_rounds must equal diagnose failures count"
    );
    assert!(card.retry_rounds >= 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn attach_user_hooks_fires_session_start_end_to_end() {
    use futures::StreamExt;
    let mut config = Config::default();
    config.graph.enabled = false;
    config.memory.enabled = false;
    config.verify.enabled = false;
    config.review.enabled = false;
    let root = std::env::temp_dir().join(format!("dnv-hooks-session-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let markers = root.join("session.log");
    config.hooks = deepseeknova_config::HooksConfig {
        enabled: true,
        session_start: vec![marker_cmd("start", &markers)],
        ..Default::default()
    };
    let provider = std::sync::Arc::new(stub_provider());
    let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
    let mut stream = agent
        .run_stream(RunInput {
            prompt: "hi".into(),
            images: vec![],
            model_override: None,
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}
    let text = std::fs::read_to_string(&markers).unwrap_or_default();
    assert!(
        text.contains("start"),
        "build_agent 装配的 session_start hook 必须触发: {text:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn attach_user_hooks_noop_when_disabled() {
    use futures::StreamExt;
    // enabled=false：即便配置了命令也不挂载（零开销，不 spawn 进程）。
    let mut config = Config::default();
    config.graph.enabled = false;
    config.memory.enabled = false;
    config.verify.enabled = false;
    config.review.enabled = false;
    let root = std::env::temp_dir().join(format!("dnv-hooks-disabled-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let markers = root.join("session.log");
    config.hooks = deepseeknova_config::HooksConfig {
        enabled: false,
        session_start: vec![marker_cmd("start", &markers)],
        ..Default::default()
    };
    let provider = std::sync::Arc::new(stub_provider());
    let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
    let mut stream = agent
        .run_stream(RunInput {
            prompt: "hi".into(),
            images: vec![],
            model_override: None,
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}
    assert!(
        !markers.exists(),
        "hooks 关闭时不得触发任何外部命令（零开销）"
    );
    let _ = std::fs::remove_dir_all(&root);
}

// -----------------------------------------------------------------------
// M8b：builder disabled-set 过滤补全 + 预算/验证接线
// -----------------------------------------------------------------------

/// 记忆关闭时必须把所有记忆工具（remember/recall/forget）从注册表剔除，
/// 模型看不到其 schema（与 graph 同款处理）。既有测试只查 recall，
/// 这里补 remember/forget 全覆盖。
#[test]
fn build_agent_skips_all_memory_tools_when_disabled() {
    let mut config = Config::default();
    config.memory.enabled = false;
    config.graph.enabled = false;
    let provider = std::sync::Arc::new(stub_provider());
    let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
    let names = agent.tool_names();
    for tool in ["remember", "recall", "forget"] {
        assert!(
            !names.iter().any(|n| n == tool),
            "{tool} 必须在 memory 关闭时被排除，实得: {names:?}"
        );
    }
}

#[tokio::test]
async fn build_agent_registers_remember_and_forget_when_memory_enabled() {
    let mut config = Config::default();
    config.memory.enabled = true;
    config.graph.enabled = false;
    let root = std::env::temp_dir().join(format!("dnv-mem-forget-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let provider = std::sync::Arc::new(stub_provider());
    let agent = build_agent(&config, root.clone(), provider, 5, None, vec![]).unwrap();
    let names = agent.tool_names();
    for tool in ["remember", "recall", "forget"] {
        assert!(
            names.iter().any(|n| n == tool),
            "{tool} 必须在 memory 开启时注册，实得: {names:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// tools.overrides 对内置工具同样生效：禁用 web_search 后模型看不到其
/// schema（与既有 extra_tools 覆盖同款 disabled-set 过滤）。
#[test]
fn build_agent_disables_builtin_tool_via_overrides() {
    let mut config = Config::default();
    config.graph.enabled = false;
    config.memory.enabled = false;
    config.tools.overrides = vec![deepseeknova_config::ToolOverride {
        name: "web_search".into(),
        disabled: true,
        timeout_secs: None,
        max_file_size: None,
    }];
    let provider = std::sync::Arc::new(stub_provider());
    let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
    let names = agent.tool_names();
    assert!(
        !names.iter().any(|n| n == "web_search"),
        "web_search 必须被 overrides 禁用，实得: {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "read_file"),
        "其他内置工具不受影响"
    );
}

/// B2 预算接线：`[budget] enabled=true` 时运行时挂 PromptBudgetController。
/// 极小 system prompt + 极低 max_total_tokens → 首步预算 Reject → 优雅
/// Paused（reason 含 "budget"），证明预算守门在生产路径真实生效。
#[tokio::test]
async fn build_agent_wires_token_budget_and_pauses_on_excess() {
    use deepseeknova_core::Runner;
    use futures::StreamExt;
    let mut config = Config::default();
    config.agent.system_prompt = Some("tiny".into()); // 极小 system prompt
    config.budget.enabled = true;
    config.budget.max_total_tokens = 64;
    config.budget.max_memory_tokens = 16;
    config.graph.enabled = false;
    config.memory.enabled = false;
    config.delegate.enabled = false;
    config.verify.enabled = false;
    config.review.enabled = false;
    config.agent.l3_compaction = false;
    let provider = std::sync::Arc::new(stub_provider());
    let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
    let mut stream = agent
        .run_stream(RunInput {
            prompt: "hi".into(),
            images: Vec::new(),
            model_override: None,
        })
        .await
        .unwrap();
    let mut paused_reason: Option<String> = None;
    while let Some(ev) = stream.next().await {
        if let Ok(deepseeknova_core::runner::RunEvent::Paused { reason, .. }) = ev {
            paused_reason = Some(reason);
        }
    }
    assert!(
        paused_reason.is_some(),
        "budget Reject 必须 Paused（而非跑满 max_steps）"
    );
    assert!(
        paused_reason
            .as_deref()
            .unwrap_or_default()
            .contains("budget"),
        "Paused reason 必须说明预算: {paused_reason:?}"
    );
}

/// P2-4 团队级花费上限接线：build_agent 后叠加 `with_cost_budget`（CLI
/// 同款装配），账本已超限时首步即 Paused（reason 含 "cost"）。
#[tokio::test]
async fn build_agent_wires_cost_budget_pausing_on_exceeded_spend() {
    use deepseeknova_core::Runner;
    use deepseeknova_provider::cost::{CostLedger, ModelPrices, ModelRole};
    use futures::StreamExt;

    // 预置账本：模型 "big" 有完整单价，1M prompt → 2.0 USD ≥ 上限 1.0。
    let ledger = Arc::new(CostLedger::new());
    let mut prices = deepseeknova_provider::cost::PriceTable::new();
    prices.insert(
        "big".to_string(),
        ModelPrices {
            input_per_mtok: Some(2.0),
            output_per_mtok: Some(8.0),
            cache_hit_per_mtok: Some(0.2),
        },
    );
    ledger.record(
        ModelRole::Main,
        "big",
        &deepseeknova_core::chunk::Usage {
            prompt_tokens: 1_000_000,
            completion_tokens: 0,
            total_tokens: 1_000_000,
            cache_hit_tokens: 0,
            cache_miss_tokens: 1_000_000,
            reasoning_tokens: 0,
        },
    );

    let mut config = Config::default();
    config.graph.enabled = false;
    config.memory.enabled = false;
    config.delegate.enabled = false;
    config.verify.enabled = false;
    config.review.enabled = false;
    let agent = build_agent(
        &config,
        std::env::temp_dir(),
        Arc::new(stub_provider()),
        5,
        None,
        vec![],
    )
    .unwrap()
    .with_cost_budget(deepseeknova_agent::budget::cost::CostBudget::new(
        ledger, prices, 1.0,
    ));
    let mut stream = agent
        .run_stream(RunInput {
            prompt: "hi".into(),
            images: Vec::new(),
            model_override: None,
        })
        .await
        .unwrap();
    let mut paused_reason: Option<String> = None;
    while let Some(ev) = stream.next().await {
        if let Ok(deepseeknova_core::runner::RunEvent::Paused { reason, .. }) = ev {
            paused_reason = Some(reason);
        }
    }
    assert!(paused_reason.is_some(), "成本超限必须 Paused");
    assert!(
        paused_reason
            .as_deref()
            .unwrap_or_default()
            .contains("cost"),
        "Paused reason 必须指出成本上限: {paused_reason:?}"
    );
}

/// P4 验证接线：`[verify] enabled=true` + commands 非空时装配验证链
/// （构建不 panic）；llm=false 不要求额外 provider 解析。
#[test]
fn build_agent_wires_verify_with_command() {
    let mut config = Config::default();
    config.graph.enabled = false;
    config.memory.enabled = false;
    config.verify.enabled = true;
    config.verify.commands = vec!["echo ok".into()];
    config.verify.llm = false;
    let provider = std::sync::Arc::new(stub_provider());
    let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
    let _ = agent;
}

/// disabled-set 过滤与 graph 开关叠加：graph 启用但 tools.overrides
/// 禁用 search_code 时，该工具仍被剔除（overrides 与功能开关两路
/// disabled 集合合并，模型看不到被禁 schema）。
#[tokio::test]
async fn build_agent_disables_graph_tool_via_overrides_even_when_graph_enabled() {
    let mut config = Config::default();
    config.graph.enabled = true;
    config.memory.enabled = false;
    config.tools.overrides = vec![deepseeknova_config::ToolOverride {
        name: "search_code".into(),
        disabled: true,
        timeout_secs: None,
        max_file_size: None,
    }];
    let provider = std::sync::Arc::new(stub_provider());
    let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
    let names = agent.tool_names();
    assert!(
        !names.iter().any(|n| n == "search_code"),
        "overrides 禁用必须叠加到 graph 启用的工具集，实得: {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "traverse_graph"),
        "其他图工具仍保留"
    );
}

/// A1 检查点接线：`[checkpoint] enabled=true` 时装配 CheckpointManager
///（构建不 panic；缺文件时 warn 后新建，行为与默认一致）。
#[test]
fn build_agent_wires_checkpoint_when_enabled() {
    let mut config = Config::default();
    config.checkpoint.enabled = true;
    config.graph.enabled = false;
    config.memory.enabled = false;
    let root = std::env::temp_dir().join(format!("dnv-cp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let provider = std::sync::Arc::new(stub_provider());
    let agent = build_agent(&config, root.clone(), provider, 5, None, vec![]).unwrap();
    let _ = agent;
    let _ = std::fs::remove_dir_all(&root);
}

//! # DeepseekNova 快速上手示例
//!
//! 运行: `cargo run --example quickstart -p deepseeknova-cli`
//!
//! 展示 DeepseekNova 核心概念，无需 LLM API key：
//!   1. 安全路径解析（secure_resolve / sanitize_path）
//!   2. 安全策略检查（路径/命令/域名权限）
//!   3. 资源限额配置
//!   4. GOAP 计划类型（Goal / Action / Plan 序列化）
//!   5. MCP 工具适配器命名约定

use std::path::Path;

fn main() -> anyhow::Result<()> {
    println!("═══ DPRonix Quickstart ═══\n");

    // ── 1. 安全路径解析 ──────────────────────────────────────────
    println!("▸ 1. Security — path resolution");
    let workspace = std::env::current_dir()?;
    let safe_path = Path::new("Cargo.toml");
    let resolved =
        deepseeknova_security::path::sanitize_path(&workspace, safe_path.to_str().unwrap())?;
    println!("   ✅ 安全路径: {}", resolved.display());

    let bad_result = deepseeknova_security::path::sanitize_path(&workspace, "../../etc/passwd");
    println!("   ⛔ 路径遍历被拒绝: {:?}", bad_result.is_err());
    assert!(bad_result.is_err());

    // ── 2. 安全策略 ─────────────────────────────────────────────
    println!("\n▸ 2. Security — policy checks");
    use deepseeknova_security::policy::SecurityPolicy;
    use std::path::PathBuf;

    let policy = SecurityPolicy {
        allowed_commands: vec!["cargo".into()],
        allowed_domains: vec!["api.example.com".into()],
        denied_paths: vec![PathBuf::from("/secret")],
        ..SecurityPolicy::new()
    };

    assert!(policy.is_command_allowed("cargo build"));
    assert!(!policy.is_command_allowed("rm -rf /"));
    assert!(!policy.is_path_allowed(Path::new("/secret/data")));
    assert!(policy.is_domain_allowed("api.example.com"));
    println!("   ✅ cargo build     → allowed");
    println!("   ✅ rm -rf /        → blocked");
    println!("   ✅ /secret/data    → blocked");
    println!("   ✅ api.example.com → allowed");

    // ── 3. 资源限额 ─────────────────────────────────────────────
    println!("\n▸ 3. Security — resource limits");
    let limits = deepseeknova_security::limits::ResourceLimits::default();
    println!(
        "   📦 默认: max_files={}, max_tool_calls={}",
        limits.max_files, limits.max_tool_calls
    );

    // ── 4. GOAP 计划 ────────────────────────────────────────────
    println!("\n▸ 4. Orchestration — Goal / Action / Plan");
    use deepseeknova_orch::types::*;

    let goal = Goal {
        description: "构建一个 Rust CLI 工具".into(),
        constraints: vec!["使用 clap".into()],
        criteria: vec!["编译通过".into(), "测试通过".into()],
    };

    let actions = vec![
        Action {
            id: "act-1".into(),
            name: "create_cargo_project".into(),
            description: "初始化 Cargo 项目".into(),
            preconditions: vec![],
            effects: vec!["项目已创建".into()],
            cost: 10.0,
            tool: Some("bash".into()),
            tool_args: None,
            delegatable: false,
            status: ActionStatus::Completed,
        },
        Action {
            id: "act-2".into(),
            name: "implement_cli".into(),
            description: "编写 CLI 入口".into(),
            preconditions: vec!["项目已创建".into()],
            effects: vec!["CLI 已实现".into()],
            cost: 50.0,
            tool: Some("edit_file".into()),
            tool_args: None,
            delegatable: true,
            status: ActionStatus::Pending,
        },
    ];

    let mut deps = std::collections::HashMap::new();
    deps.insert("act-2".into(), vec!["act-1".into()]);

    let plan = Plan {
        id: "plan-001".into(),
        goal,
        actions,
        dependencies: deps,
        status: PlanStatus::Draft,
        reasoning: Some("分两步：先创建项目再实现 CLI".into()),
        usage: Some(PlanUsage {
            prompt_tokens: 150,
            completion_tokens: 80,
            cache_hit_tokens: 0,
            cache_miss_tokens: 150,
        }),
    };

    println!(
        "   计划: {} | 动作: {} | 依赖: act-2→act-1",
        plan.id,
        plan.actions.len()
    );
    let json = serde_json::to_string_pretty(&plan)?;
    let _restored: Plan = serde_json::from_str(&json)?;
    println!("   📋 JSON 序列化 ✅");

    // ── 5. MCP 命名 ─────────────────────────────────────────────
    println!("\n▸ 5. MCP — tool adapter naming convention");
    println!("   mcp__<server>__<tool>  (e.g. mcp__my-server__read_file)");

    println!("\n═══ quickstart 完成 ✅ ═══");
    Ok(())
}

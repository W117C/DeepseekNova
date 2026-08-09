//! # DeepseekNova 快速上手示例
//!
//! 运行: `cargo run --example quickstart -p deepseeknova-cli`
//!
//! 展示 DeepseekNova 核心概念，无需 LLM API key：
//!   1. 安全路径解析（secure_resolve / sanitize_path）
//!   2. 安全策略检查（路径/命令/域名权限）
//!   3. 资源限额配置
//!   4. MCP 工具适配器命名约定
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

fn main() -> Result<(), deepseeknova_core::DeepseeknovaError> {
    println!("═══ DeepseekNova Quickstart ═══\n");

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

    // ── 4. MCP 命名 ─────────────────────────────────────────────
    println!("\n▸ 4. MCP — tool adapter naming convention");
    println!("   mcp__<server>__<tool>  (e.g. mcp__my-server__read_file)");

    println!("\n═══ quickstart 完成 ✅ ═══");
    Ok(())
}

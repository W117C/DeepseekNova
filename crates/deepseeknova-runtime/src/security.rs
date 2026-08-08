//! 安全装配：PermissionGate / SecurityContext 构建与配置解析。
//! M7b 拆分：从 lib.rs 纯搬移，不修改行为/签名。

use std::path::PathBuf;
use std::sync::Arc;

use deepseeknova_config::Config;
use deepseeknova_permission::{Decision, PermissionGate, Policy};
use deepseeknova_security::audit::JsonlAuditLogger;
use deepseeknova_security::capability::Capability;
use deepseeknova_security::context::SecurityContext;
use deepseeknova_security::limits::ResourceLimits;
use deepseeknova_security::policy::SecurityPolicy;

/// Build a PermissionGate from Config.
pub fn build_permission_gate(config: &Config) -> PermissionGate {
    let mut allow = Vec::new();
    let mut ask = Vec::new();
    let mut deny = Vec::new();

    for rule in &config.permissions.rules {
        let r = if let Some(ref subject) = rule.subject {
            deepseeknova_permission::Rule::with_subject(&rule.tool, subject)
        } else {
            deepseeknova_permission::Rule::new(&rule.tool)
        };

        match rule.mode {
            deepseeknova_config::PermissionMode::Allow => allow.push(r),
            deepseeknova_config::PermissionMode::Ask => ask.push(r),
            deepseeknova_config::PermissionMode::Deny => deny.push(r),
        }
    }

    let mode = match config.permissions.default_mode {
        deepseeknova_config::PermissionMode::Allow => Decision::Allow,
        deepseeknova_config::PermissionMode::Ask => Decision::Ask,
        deepseeknova_config::PermissionMode::Deny => Decision::Deny,
    };

    let gate = PermissionGate::new(Policy {
        mode,
        allow,
        ask,
        deny,
    })
    // 权限模式预设（`[permissions] mode`，缺省 None = 旧行为）。
    .with_mode(config.permissions.mode.map(Into::into))
    // 项目层 allow 规则标记（untrusted 工作区降级为 ask 的依据）。
    .with_allow_project_scoped(config.permissions.project_owns_rules);
    // 可选速率限制：滚动一分钟内超出上限的工具调用直接 Deny。
    match config.permissions.rate_limit_per_minute {
        Some(limit) => gate.with_rate_limit(limit),
        None => gate,
    }
}

/// Return an `Arc<PermissionGate>` when permission enforcement is enabled in
/// config (pinned to `workspace_root`), otherwise `None`. Shared by the agent
/// builder and the CLI coordinator so gate activation stays consistent.
pub fn permission_gate_for(
    config: &Config,
    workspace_root: &std::path::Path,
) -> Option<Arc<PermissionGate>> {
    if config.permissions.enabled {
        // 工作区信任：默认 untrusted（fail-closed），TrustStore 命中才 trusted。
        let trusted = deepseeknova_config::TrustStore::load().is_trusted(workspace_root);
        Some(Arc::new(
            build_permission_gate(config)
                .with_workspace_root(workspace_root.to_path_buf())
                .with_trusted(trusted)
                // M1 审计盲区修复：gate 拒绝（越界/危险/deny 规则/限流）持久化
                // 到 workspace 的审计 JSONL（缺 root 的库级 build_permission_gate
                // 保持无审计器，向后兼容）。
                .with_audit_logger(Arc::new(
                    deepseeknova_security::audit::JsonlAuditLogger::at_workspace(workspace_root),
                )),
        ))
    } else {
        None
    }
}

/// 沙箱可写根：配置的 `writable_paths` 加上工作区根。
///
/// 工作区默认可写（与 `build_security_context` 把 workspace root 加入
/// allow-list 的语义对齐）；已显式配置时不重复添加。
pub(crate) fn sandbox_writable_paths(
    config: &Config,
    workspace_root: &std::path::Path,
) -> Vec<String> {
    let root = workspace_root.to_string_lossy().into_owned();
    let mut paths: Vec<String> = config.sandbox.writable_paths.clone();
    if !paths.iter().any(|p| p == &root) {
        paths.push(root);
    }
    paths
}

/// Parse a capability name (case-insensitive) from config.
fn parse_capability(raw: &str) -> Option<Capability> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "file_read" | "fileread" => Some(Capability::FileRead),
        "file_write" | "filewrite" => Some(Capability::FileWrite),
        "command_execute" | "commandexecute" => Some(Capability::CommandExecute),
        "network_access" | "networkaccess" => Some(Capability::NetworkAccess),
        "mcp_invoke" | "mcpinvoke" => Some(Capability::McpInvoke),
        "memory_read" | "memoryread" => Some(Capability::MemoryRead),
        "memory_write" | "memorywrite" => Some(Capability::MemoryWrite),
        _ => None,
    }
}

/// Build a [`SecurityContext`] from the `[security]` section of [`Config`].
///
/// The `workspace_root` is always added to the allow-list so builtin file
/// tools can operate inside the project. When `config.security` is all
/// defaults this returns a context equivalent to
/// [`SecurityContext::with_safe_defaults()`] but with the workspace root
/// pinned to `workspace_root`.
pub fn build_security_context(
    config: &Config,
    workspace_root: &std::path::Path,
) -> anyhow::Result<SecurityContext> {
    let sec = &config.security;

    // Capabilities: start from safe defaults, then disable configured ones.
    let mut capabilities = std::collections::HashSet::new();
    capabilities.insert(Capability::FileRead);
    capabilities.insert(Capability::FileWrite);
    capabilities.insert(Capability::CommandExecute);
    capabilities.insert(Capability::NetworkAccess);
    capabilities.insert(Capability::McpInvoke);
    capabilities.insert(Capability::MemoryRead);
    capabilities.insert(Capability::MemoryWrite);
    for raw in &sec.disabled_capabilities {
        if let Some(cap) = parse_capability(raw) {
            capabilities.remove(&cap);
        }
    }

    // Paths: workspace root is always allowed; merge user allow/deny lists.
    let mut allowed_paths = vec![workspace_root.to_path_buf()];
    for p in &sec.allowed_paths {
        allowed_paths.push(PathBuf::from(p));
    }
    let denied_paths = sec.denied_paths.iter().map(PathBuf::from).collect();

    let policy = SecurityPolicy {
        allowed_paths,
        denied_paths,
        allowed_commands: sec.allowed_commands.clone(),
        allowed_domains: sec.allowed_domains.clone(),
    };

    // Resource limits: start from defaults, override where configured.
    let mut limits = ResourceLimits::default();
    let cfg = &sec.limits;
    if let Some(v) = cfg.max_files {
        limits.max_files = v;
    }
    if let Some(v) = cfg.max_file_size {
        limits.max_file_size = v;
    }
    if let Some(v) = cfg.max_total_read_bytes {
        limits.max_total_read_bytes = v;
    }
    if let Some(v) = cfg.max_execution_time_secs {
        limits.max_execution_time = std::time::Duration::from_secs(v);
    }
    if let Some(v) = cfg.max_output_bytes {
        limits.max_output_bytes = v;
    }
    if let Some(v) = cfg.max_tool_calls {
        limits.max_tool_calls = v;
    }

    Ok(SecurityContext {
        capabilities,
        limits,
        policy,
        // B1 遗留：审计后端从 TracingAuditLogger 切到 JSONL 落盘
        // （`.deepseeknova/security/audit.jsonl`）。写盘失败仅 warn、
        // 不影响安全判定（audit.rs 的 fail-closed 语义）。
        audit: Arc::new(JsonlAuditLogger::at_workspace(workspace_root)),
    })
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_writable_paths_includes_workspace_once() {
        let mut config = Config::default();
        config.sandbox.writable_paths = vec!["/tmp/work".into()];
        let root = std::path::Path::new("/ws");
        let paths = sandbox_writable_paths(&config, root);
        assert_eq!(paths, vec!["/tmp/work".to_string(), "/ws".to_string()]);

        // 已显式包含工作区根时不重复添加
        config.sandbox.writable_paths = vec!["/ws".into(), "/tmp/work".into()];
        let paths = sandbox_writable_paths(&config, root);
        assert_eq!(paths, vec!["/ws".to_string(), "/tmp/work".to_string()]);
    }

    #[test]
    fn build_security_context_default_grants_all_capabilities() {
        let config = Config::default();
        let root =
            std::env::temp_dir().join(format!("deepseeknova-sec-default-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();

        let ctx = super::build_security_context(&config, &root).unwrap();
        for cap in [
            Capability::FileRead,
            Capability::FileWrite,
            Capability::CommandExecute,
            Capability::NetworkAccess,
            Capability::McpInvoke,
            Capability::MemoryRead,
            Capability::MemoryWrite,
        ] {
            assert!(
                ctx.capabilities.contains(&cap),
                "expected {cap:?} granted by default"
            );
        }
        // 工作区根必须自动出现在允许路径里（即使配置无 allowed_paths）。
        assert!(ctx.policy.allowed_paths.iter().any(|p| p == &root));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn build_security_context_honors_disabled_capabilities_and_lists() {
        let mut config = Config::default();
        config.security.disabled_capabilities =
            vec!["command_execute".into(), "network_access".into()];
        config.security.allowed_commands = vec!["git".into()];
        config.security.allowed_domains = vec!["api.github.com".into()];
        config.security.denied_paths = vec!["/tmp/build/secret".into()];

        let root = std::env::temp_dir().join(format!(
            "deepseeknova-sec-restricted-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let ctx = super::build_security_context(&config, &root).unwrap();
        assert!(!ctx.capabilities.contains(&Capability::CommandExecute));
        assert!(!ctx.capabilities.contains(&Capability::NetworkAccess));
        assert!(ctx.capabilities.contains(&Capability::FileRead));
        assert_eq!(ctx.policy.allowed_commands, vec!["git".to_string()]);
        assert_eq!(
            ctx.policy.allowed_domains,
            vec!["api.github.com".to_string()]
        );
        assert!(ctx
            .policy
            .denied_paths
            .iter()
            .any(|p| p.to_string_lossy().contains("secret")));
        // 工作区根 join 在用户 allowed_paths 之前。
        assert!(ctx.policy.allowed_paths.first().unwrap() == &root);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn build_security_context_applies_resource_limits() {
        let mut config = Config::default();
        config.security.limits.max_files = Some(7);
        config.security.limits.max_execution_time_secs = Some(120);
        config.security.limits.max_output_bytes = Some(1024);

        let root =
            std::env::temp_dir().join(format!("deepseeknova-sec-limits-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();

        let ctx = super::build_security_context(&config, &root).unwrap();
        assert_eq!(ctx.limits.max_files, 7);
        assert_eq!(
            ctx.limits.max_execution_time,
            std::time::Duration::from_secs(120)
        );
        assert_eq!(ctx.limits.max_output_bytes, 1024);
        // 未覆盖的限额保留默认值。
        assert_eq!(ctx.limits.max_tool_calls, 100);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn security_context_persists_jsonl_audit_backend() {
        // B1 遗留：build_security_context 的审计后端必须落盘 JSONL 而非仅
        // tracing。写一条 SecurityEvent → 校验 `<ws>/.deepseeknova/security/
        // audit.jsonl` 出现对应行（写盘失败仅 warn、不影响判定）。
        let ws = std::env::temp_dir().join(format!("dnv-rt-audit-{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        let config = Config::default();
        let ctx = build_security_context(&config, &ws).unwrap();
        ctx.audit
            .record(&deepseeknova_security::audit::SecurityEvent {
                event_type: "capability_violation".to_string(),
                call_id: "call-1".to_string(),
                tool_name: "remember".to_string(),
                capability: None,
                path: Some("/etc/passwd".to_string()),
                allowed: false,
                reason: "capability disabled".to_string(),
            });
        let audit_file = ws.join(".deepseeknova/security/audit.jsonl");
        let content = std::fs::read_to_string(&audit_file)
            .unwrap_or_else(|e| panic!("audit.jsonl must be written: {e}"));
        assert!(
            content.contains("capability_violation") && content.contains("/etc/passwd"),
            "audit event should persist to JSONL, got: {content}"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }
}

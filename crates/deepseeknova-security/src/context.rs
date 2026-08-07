use crate::audit::{AuditLogger, JsonlAuditLogger, SecurityEvent};
use crate::capability::Capability;
use crate::limits::ResourceLimits;
use crate::policy::SecurityPolicy;
use deepseeknova_core::tool::ToolContext;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct SecurityContext {
    pub capabilities: HashSet<Capability>,
    pub limits: ResourceLimits,
    pub policy: SecurityPolicy,
    pub audit: Arc<dyn AuditLogger>,
}

impl Default for SecurityContext {
    fn default() -> Self {
        Self::with_safe_defaults()
    }
}

impl SecurityContext {
    /// 安全默认：全部能力放行；审计经 JSONL 持久化到
    /// `<cwd>/.deepseeknova/security/audit.jsonl`（写盘失败仅 warn，
    /// 不改变安全判定）。
    pub fn with_safe_defaults() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
        Self::with_audit_log(&cwd)
    }

    /// 安全默认 + 把审计日志固定到 `workspace_root` 下
    /// （`.deepseeknova/security/audit.jsonl`）。
    pub fn with_audit_log(workspace_root: &Path) -> Self {
        let mut capabilities = HashSet::new();
        capabilities.insert(Capability::FileRead);
        capabilities.insert(Capability::FileWrite);
        capabilities.insert(Capability::CommandExecute);
        capabilities.insert(Capability::NetworkAccess);
        capabilities.insert(Capability::McpInvoke);
        capabilities.insert(Capability::MemoryRead);
        capabilities.insert(Capability::MemoryWrite);

        Self {
            capabilities,
            limits: ResourceLimits::default(),
            policy: SecurityPolicy::new(),
            audit: Arc::new(JsonlAuditLogger::at_workspace(workspace_root)),
        }
    }

    /// 检查能力门禁：未授予时记录审计事件（含**真实工具名**）并拒绝。
    /// 写盘失败不影响拒绝判定（fail-closed），只产生 warn。
    pub fn require(
        &self,
        ctx: &ToolContext,
        tool_name: &str,
        cap: Capability,
    ) -> anyhow::Result<()> {
        if !self.capabilities.contains(&cap) {
            let event = SecurityEvent {
                event_type: "capability_violation".to_string(),
                call_id: ctx.call_id.clone(),
                tool_name: tool_name.to_string(),
                capability: Some(cap),
                path: None,
                allowed: false,
                reason: format!("Capability {:?} is not granted in the current context", cap),
            };
            self.audit.record(&event);
            anyhow::bail!("Security violation: capability {:?} is not granted", cap);
        }
        Ok(())
    }
}

/// 能力门禁入口。`tool_name` 由调用方传入工具真实名（`self.schema().name`），
/// 审计事件不再硬编码 `"unknown"`。
pub fn enforce_capability(
    ctx: &ToolContext,
    tool_name: &str,
    cap: Capability,
) -> anyhow::Result<()> {
    let security = ctx
        .extensions
        .get::<SecurityContext>()
        .ok_or_else(|| anyhow::anyhow!("SecurityContext extension not found in ToolContext"))?;
    security.require(ctx, tool_name, cap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn require_records_real_tool_name_in_jsonl_audit() {
        let dir = tempfile::tempdir().unwrap();
        let mut sec = SecurityContext::with_audit_log(dir.path());
        // 仅保留 FileRead，剥夺 MemoryWrite
        let mut caps = HashSet::new();
        caps.insert(Capability::FileRead);
        sec.capabilities = caps;

        let ctx = ToolContext::new("call-7");
        let err = sec.require(&ctx, "remember", Capability::MemoryWrite);
        assert!(err.is_err(), "missing capability must be denied");

        let log = dir.path().join(".deepseeknova/security/audit.jsonl");
        let content = std::fs::read_to_string(&log).expect("audit log must be written");
        assert!(
            content.contains(r#""tool_name":"remember""#),
            "audit must carry the real tool name, not 'unknown': {content}"
        );
        assert!(content.contains(r#""allowed":false"#), "got: {content}");
    }

    #[test]
    fn enforce_capability_denies_with_real_name() {
        let dir = tempfile::tempdir().unwrap();
        let mut sec = SecurityContext::with_audit_log(dir.path());
        let mut caps = HashSet::new();
        caps.insert(Capability::FileRead);
        sec.capabilities = caps;

        let ctx = ToolContext::new("call-8").with_extension(sec);
        let err = enforce_capability(&ctx, "recall", Capability::MemoryRead).unwrap_err();
        assert!(err.to_string().contains("MemoryRead"), "got: {err}");
        let log = dir.path().join(".deepseeknova/security/audit.jsonl");
        let content = std::fs::read_to_string(&log).unwrap();
        assert!(
            content.contains(r#""tool_name":"recall""#),
            "got: {content}"
        );
    }
}

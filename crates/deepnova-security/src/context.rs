use crate::audit::{AuditLogger, SecurityEvent, TracingAuditLogger};
use crate::capability::Capability;
use crate::limits::ResourceLimits;
use crate::policy::SecurityPolicy;
use deepnova_core::tool::ToolContext;
use std::collections::HashSet;
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
    pub fn with_safe_defaults() -> Self {
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
            audit: Arc::new(TracingAuditLogger),
        }
    }

    pub fn require(&self, ctx: &ToolContext, cap: Capability) -> anyhow::Result<()> {
        if !self.capabilities.contains(&cap) {
            let event = SecurityEvent {
                event_type: "capability_violation".to_string(),
                call_id: ctx.call_id.clone(),
                tool_name: "unknown".to_string(), // ToolContext doesn't carry tool name, but caller can customize or log
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

pub fn enforce_capability(ctx: &ToolContext, cap: Capability) -> anyhow::Result<()> {
    let security = ctx
        .extensions
        .get::<SecurityContext>()
        .ok_or_else(|| anyhow::anyhow!("SecurityContext extension not found in ToolContext"))?;
    security.require(ctx, cap)
}

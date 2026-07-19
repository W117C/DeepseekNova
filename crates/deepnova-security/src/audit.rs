use crate::capability::Capability;

#[derive(Debug, Clone)]
pub struct SecurityEvent {
    pub event_type: String,
    pub call_id: String,
    pub tool_name: String,
    pub capability: Option<Capability>,
    pub path: Option<String>,
    pub allowed: bool,
    pub reason: String,
}

pub trait AuditLogger: Send + Sync + std::fmt::Debug {
    fn record(&self, event: &SecurityEvent);
}

#[derive(Debug, Clone, Copy)]
pub struct TracingAuditLogger;

impl AuditLogger for TracingAuditLogger {
    fn record(&self, event: &SecurityEvent) {
        tracing::warn!(
            security_event = %event.event_type,
            call_id = %event.call_id,
            tool_name = %event.tool_name,
            capability = ?event.capability,
            path = ?event.path,
            allowed = %event.allowed,
            reason = %event.reason,
            "Security Event Audited"
        );
    }
}

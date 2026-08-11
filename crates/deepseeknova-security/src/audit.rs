use crate::capability::Capability;
use serde_json::json;
use std::path::PathBuf;

/// 一次安全判定事件：记录判定结果、原因与关联的工具调用信息。
#[derive(Debug, Clone)]
pub struct SecurityEvent {
    /// 事件类型（如 `capability_violation`）。
    pub event_type: String,
    /// 触发事件的工具调用 id。
    pub call_id: String,
    /// 触发事件的工具真实名。
    pub tool_name: String,
    /// 触发事件的能力，若事件与某能力相关。
    pub capability: Option<Capability>,
    /// 涉及的文件路径，若事件与路径相关。
    pub path: Option<String>,
    /// 是否放行（`false` = 拒绝）。
    pub allowed: bool,
    /// 判定原因说明。
    pub reason: String,
}

/// 审计日志器：接收并记录安全事件。
pub trait AuditLogger: Send + Sync + std::fmt::Debug {
    /// 记录一次安全事件。
    fn record(&self, event: &SecurityEvent);
}

/// 仅通过 `tracing` 输出的审计日志器（`security_event` 结构化字段）。
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

/// 落盘审计日志（JSONL）到 `<workspace>/.deepseeknova/security/audit.jsonl`。
///
/// 每个事件一行 JSON，字段：`event_type` / `call_id` / `tool_name` /
/// `capability` / `path` / `allowed` / `reason`。同时保留既有 tracing 输出，
/// 供日志流消费。
///
/// fail-closed 语义：写盘失败只 `warn`、**绝不改变安全判定**——拒绝/放行
/// 由调用方（能力门禁等）先决，审计持久化是尽力而为的记录通道。
#[derive(Debug)]
pub struct JsonlAuditLogger {
    path: PathBuf,
}

impl JsonlAuditLogger {
    /// 在 `workspace_root` 下创建日志器（`.deepseeknova/security/audit.jsonl`）。
    pub fn at_workspace(workspace_root: &std::path::Path) -> Self {
        Self::new(
            workspace_root
                .join(".deepseeknova")
                .join("security")
                .join("audit.jsonl"),
        )
    }

    /// 指定完整落盘路径创建日志器。
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn persist(&self, event: &SecurityEvent) {
        let line = json!({
            "event_type": event.event_type,
            "call_id": event.call_id,
            "tool_name": event.tool_name,
            "capability": event.capability.map(|c| format!("{c:?}")),
            "path": event.path,
            "allowed": event.allowed,
            "reason": event.reason,
        });
        let Ok(rendered) = serde_json::to_string(&line) else {
            tracing::warn!("audit log serialization failed; event dropped");
            return;
        };
        if let Some(parent) = self.path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!("audit log dir create failed ({}): {e}", parent.display());
                return;
            }
        }
        use std::io::Write;
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            Ok(mut f) => {
                if let Err(e) = writeln!(f, "{rendered}") {
                    tracing::warn!("audit log write failed ({}): {e}", self.path.display());
                }
            }
            Err(e) => tracing::warn!("audit log open failed ({}): {e}", self.path.display()),
        }
    }
}

impl AuditLogger for JsonlAuditLogger {
    fn record(&self, event: &SecurityEvent) {
        self.persist(event);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonl_audit_persists_event_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let logger = JsonlAuditLogger::at_workspace(dir.path());
        let event = SecurityEvent {
            event_type: "capability_violation".to_string(),
            call_id: "call-1".to_string(),
            tool_name: "remember".to_string(),
            capability: Some(Capability::MemoryWrite),
            path: None,
            allowed: false,
            reason: "Capability MemoryWrite is not granted in the current context".to_string(),
        };
        logger.record(&event);

        let log = dir.path().join(".deepseeknova/security/audit.jsonl");
        let content = std::fs::read_to_string(&log).expect("audit log must be written");
        assert!(
            content.contains(r#""tool_name":"remember""#),
            "audit must carry the real tool name: {content}"
        );
        assert!(
            content.contains(r#""capability":"MemoryWrite""#),
            "audit must carry the capability: {content}"
        );
        assert!(content.contains(r#""allowed":false"#), "got: {content}");
        assert!(content.contains("call-1"), "got: {content}");
        // 每事件一行
        assert_eq!(content.trim_end().lines().count(), 1);
    }

    #[test]
    fn jsonl_audit_write_failure_does_not_panic() {
        // fail-closed 语义的负例：写盘失败不得 panic、不得改变判定。
        // 用一个不可写路径（现有文件目录当作文件）验证最坏路径仅告警。
        let dir = tempfile::tempdir().unwrap();
        let dir_as_file = dir.path().join("audit.jsonl");
        std::fs::write(&dir_as_file, "x").unwrap();
        let logger = JsonlAuditLogger::new(dir_as_file.join("nested/audit.jsonl"));
        let event = SecurityEvent {
            event_type: "capability_violation".to_string(),
            call_id: "c".to_string(),
            tool_name: "t".to_string(),
            capability: None,
            path: None,
            allowed: false,
            reason: "r".to_string(),
        };
        logger.record(&event); // 不 panic 即通过
    }
}

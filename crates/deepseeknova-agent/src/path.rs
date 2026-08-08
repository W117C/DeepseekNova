//! 路径/缓存键提取辅助：触碰文件路径提取、工具目标路径提取、会话缓存键。
//!
//! 从 `agent.rs` 拆分（M7）：本模块保持纯搬移，不改行为/签名/逻辑。

/// P3.3 从写类工具调用参数提取触碰文件路径（write/edit 用 `path`，
/// move 用 `source`/`destination`）。解析失败返回空。
pub(crate) fn extract_touched_paths(name: &str, args: &str) -> Vec<String> {
    if !matches!(name, "write_file" | "edit_file" | "move_file") {
        return Vec::new();
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(args) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(p) = v.get("path").and_then(|x| x.as_str()) {
        out.push(p.to_string());
    }
    if let Some(s) = v.get("source").and_then(|x| x.as_str()) {
        out.push(s.to_string());
    }
    if let Some(d) = v.get("destination").and_then(|x| x.as_str()) {
        out.push(d.to_string());
    }
    out
}

/// 编辑后诊断用：从 write/edit/move 工具参数提取目标文件路径（`path` 字段）。
pub(crate) fn extract_tool_path(args: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(args).ok()?;
    v.get("path").and_then(|p| p.as_str()).map(str::to_string)
}

/// P2.3 工具缓存 key：(工具名, 参数) 的 SHA-256 前缀 64 位。
pub(crate) fn tool_cache_key(name: &str, args: &str) -> u64 {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(name.as_bytes());
    h.update([0u8]);
    h.update(args.as_bytes());
    let d = h.finalize();
    u64::from_le_bytes([d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_tool_path_reads_write_arguments() {
        assert_eq!(
            extract_tool_path(r#"{"path":"src/main.rs"}"#).as_deref(),
            Some("src/main.rs")
        );
        // move_file 用 source/destination，不触发编辑后诊断（避免对改名目标误诊）。
        assert_eq!(
            extract_tool_path(r#"{"source":"a.rs","destination":"b.rs"}"#),
            None
        );
        assert_eq!(extract_tool_path("not json"), None);
    }
}

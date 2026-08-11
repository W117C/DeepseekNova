//! Codex CLI 配置导入：解析 `~/.codex/config.toml` + `./.codex/config.toml`。
//!
//! Codex 配置为 TOML。支持：
//! - `[permission] allow / ask / deny`（表达式格式与 Claude 一致：`ToolName(args)`）
//! - `[[mcp_servers]]`（name / command / args / env）
//!
//! Codex 的 hooks 结构在版本间不统一，当前不解析，记入 `unmapped` 提示。
//! env 与 Claude 同：配置层无顶层 env 目标，仅报告。

use super::{ImportItem, ImportPlan, ImportSource};
use crate::{EnvEntry, PermissionMode};
use std::path::{Path, PathBuf};

/// 已知工具名映射（Codex 与 Claude 共用大写工具名，见 claude 模块）。
/// 此处独立实现，避免跨模块私有函数耦合。
fn map_codex_tool(name: &str) -> (String, bool) {
    let mapped = match name {
        "Bash" => "bash",
        "Read" => "read_file",
        "Write" => "write_file",
        "Edit" => "edit_file",
        "Grep" => "grep",
        "Glob" => "glob",
        "ListDir" => "ls",
        "WebFetch" => "web_fetch",
        "WebSearch" => "web_search",
        "*" | "**" => "*",
        other => return (other.to_ascii_lowercase(), false),
    };
    (mapped.to_string(), true)
}

/// 解析一条权限表达式（同 Claude 格式）。
fn parse_permission_expr(expr: &str) -> (String, Option<String>) {
    let expr = expr.trim();
    let (name, args) = match expr.find('(') {
        Some(idx) if expr.ends_with(')') => {
            let name = expr[..idx].trim();
            let args = expr[idx + 1..expr.len() - 1].trim();
            (name, Some(args))
        }
        _ => (expr, None),
    };
    let (tool, _) = map_codex_tool(name);
    let subject = args.filter(|s| !s.is_empty()).map(str::to_string);
    (tool, subject)
}

/// 解析 `config.toml` 内容 → 导入项 + 未映射警告。
fn parse_config_toml(raw: &str) -> (Vec<ImportItem>, Vec<String>) {
    let mut items = Vec::new();
    let mut unmapped = Vec::new();

    let value: toml::Value = match toml::from_str(raw) {
        Ok(v) => v,
        Err(e) => {
            unmapped.push(format!("config.toml 解析失败: {e}"));
            return (items, unmapped);
        }
    };

    // [permission] allow / ask / deny
    if let Some(perms) = value.get("permission").and_then(|p| p.as_table()) {
        for (mode_key, mode) in [
            ("allow", PermissionMode::Allow),
            ("ask", PermissionMode::Ask),
            ("deny", PermissionMode::Deny),
        ] {
            let Some(arr) = perms.get(mode_key).and_then(|v| v.as_array()) else {
                continue;
            };
            for expr in arr.iter().filter_map(|v| v.as_str()) {
                let (tool, subject) = parse_permission_expr(expr);
                if !map_codex_tool(expr.split('(').next().unwrap_or(expr).trim()).1 {
                    unmapped.push(format!("权限表达式 {expr:?}: 工具名未在映射表，保留原样"));
                }
                items.push(ImportItem::Permission {
                    tool,
                    subject,
                    mode,
                });
            }
        }
    }

    // [[mcp_servers]]
    if let Some(servers) = value.get("mcp_servers").and_then(|s| s.as_array()) {
        for server in servers.iter().filter_map(|s| s.as_table()) {
            let name = server
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let command = server
                .get("command")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            if name.is_empty() || command.is_empty() {
                unmapped.push("mcp_servers 条目缺 name 或 command，跳过".to_string());
                continue;
            }
            let args = server
                .get("args")
                .and_then(|a| a.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let env = server
                .get("env")
                .and_then(|e| e.as_table())
                .map(|e| {
                    e.iter()
                        .filter_map(|(k, v)| {
                            v.as_str().map(|s| EnvEntry {
                                name: k.clone(),
                                value: s.to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            items.push(ImportItem::McpServer {
                name,
                command,
                args,
                env,
            });
        }
    }

    // hooks：结构不统一，跳过。
    if value.get("hooks").is_some() {
        unmapped.push(
            "Codex [hooks] 结构未统一，未导入（请在 deepseeknova.toml 手动配置）".to_string(),
        );
    }

    (items, unmapped)
}

/// 扫描 Codex 配置文件，构建导入计划（canonical 路径去重，同 Claude 模块）。
pub fn build_plan(cwd: &Path) -> ImportPlan {
    let mut plan = ImportPlan {
        source: ImportSource::Codex,
        ..Default::default()
    };

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".codex/config.toml"));
    }
    candidates.push(cwd.join(".codex/config.toml"));

    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for p in candidates {
        if !p.exists() {
            continue;
        }
        let canon = p.canonicalize().unwrap_or_else(|_| p.clone());
        if !seen.insert(canon) {
            continue;
        }
        plan.sources.push(p.clone());
        match std::fs::read_to_string(&p) {
            Ok(raw) => {
                let (items, unmapped) = parse_config_toml(&raw);
                plan.items.extend(items);
                plan.unmapped.extend(unmapped);
            }
            Err(e) => {
                plan.unmapped
                    .push(format!("读取 {} 失败: {e}", p.display()));
            }
        }
    }

    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_parses_permissions_and_mcp() {
        let raw = r#"
model = "gpt-5"
[permission]
allow = ["Bash(npm run build)", "Read(src/**)"]
deny = ["Bash(rm -rf /)"]

[[mcp_servers]]
name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem"]
env = { FOO = "bar" }
"#;
        let (items, unmapped) = parse_config_toml(raw);
        let perms: Vec<_> = items
            .iter()
            .filter_map(|i| match i {
                ImportItem::Permission { tool, mode, .. } => Some((tool.as_str(), *mode)),
                _ => None,
            })
            .collect();
        assert!(perms.contains(&("bash", PermissionMode::Allow)));
        assert!(perms.contains(&("read_file", PermissionMode::Allow)));
        assert!(perms.contains(&("bash", PermissionMode::Deny)));
        assert_eq!(perms.len(), 3);

        assert!(items.iter().any(|i| matches!(
            i,
            ImportItem::McpServer { name, command, .. }
                if name == "filesystem" && command == "npx"
        )));
        assert!(unmapped.is_empty());
    }

    #[test]
    fn codex_unknown_tool_and_hooks_flagged() {
        let raw = r#"
[permission]
allow = ["WeirdTool(x)"]
[hooks]
pre_tool_use = [{ command = "gate" }]
"#;
        let (items, unmapped) = parse_config_toml(raw);
        assert_eq!(items.len(), 1, "未知工具保留原样，仍算一条 permission");
        assert!(unmapped.iter().any(|u| u.contains("WeirdTool")));
        assert!(unmapped.iter().any(|u| u.contains("hooks")));
    }

    #[test]
    fn codex_malformed_toml_reported() {
        let (items, unmapped) = parse_config_toml("not [ valid toml");
        assert!(items.is_empty());
        assert!(unmapped.iter().any(|u| u.contains("解析失败")));
    }
}

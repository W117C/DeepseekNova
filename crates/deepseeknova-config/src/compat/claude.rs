//! Claude Code 配置导入：解析 `settings.json`（权限规则 / hooks / env）+ `.mcp.json`。
//!
//! 扫描路径：
//! - `~/.claude/settings.json`（用户层）
//! - `./.claude/settings.json`（项目层）
//! - `./.mcp.json`（项目层 MCP 服务器）
//!
//! 权限表达式格式 `ToolName(args)`（如 `Bash(git status)`、`Read(src/**)`、`Edit(**)`），
//! 映射到内部工具名（`map_claude_tool` 表）。未知工具名转小写保留，并记入
//! 计划的 `unmapped` 报告。hooks 的 `matcher` 字段当前无内部对应（我们的
//! `tool_before` 等事件组不按工具过滤），导入时忽略并记一条警告。

use super::{HookEvent, ImportItem, ImportPlan, ImportSource};
use crate::{EnvEntry, PermissionMode};
use serde_json::Value as Json;
use std::path::{Path, PathBuf};

/// 已知工具名映射表：Claude 大写工具名 → 内部小写工具名。
fn map_claude_tool(name: &str) -> (String, bool) {
    let mapped = match name {
        "Bash" => "bash",
        "Read" => "read_file",
        "Write" => "write_file",
        "Edit" | "MultiEdit" => "edit_file",
        "Grep" => "grep",
        "Glob" => "glob",
        "ListDir" => "ls",
        "WebFetch" => "web_fetch",
        "WebSearch" => "web_search",
        "Task" => "delegate",
        "TodoRead" | "TodoWrite" => "todo",
        "*" | "**" => "*",
        other => {
            // 未知工具：转小写保留（尽量保真），由调用方记入 unmapped。
            return (other.to_ascii_lowercase(), false);
        }
    };
    (mapped.to_string(), true)
}

/// 解析一条权限表达式 `ToolName(args)` → `(tool, subject)`。
/// 无括号（`ToolName`）→ subject=None；`**` → tool="*"。
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
    let (tool, _known) = map_claude_tool(name);
    let subject = args.filter(|s| !s.is_empty()).map(str::to_string);
    (tool, subject)
}

/// 解析 `settings.json` 内容 → 导入项 + 未映射警告。
fn parse_settings_json(raw: &str) -> (Vec<ImportItem>, Vec<String>) {
    let mut items = Vec::new();
    let mut unmapped = Vec::new();

    let json: Json = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => {
            unmapped.push(format!("settings.json 解析失败: {e}"));
            return (items, unmapped);
        }
    };

    // permissions.allow / ask / deny
    if let Some(perms) = json.get("permissions").and_then(|p| p.as_object()) {
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
                if !map_claude_tool(tool_key_name(expr)).1 {
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

    // hooks：{Event: [{matcher, hooks: [{type, command}]}]}
    if let Some(hooks) = json.get("hooks").and_then(|h| h.as_object()) {
        for (event_key, entries) in hooks {
            let event = match claude_hook_event(event_key) {
                Some(e) => e,
                None => {
                    unmapped.push(format!("hooks.{event_key}: 无对应内部事件，跳过"));
                    continue;
                }
            };
            let Some(entries) = entries.as_array() else {
                continue;
            };
            for entry in entries {
                let matcher = entry.get("matcher").and_then(|m| m.as_str()).unwrap_or("");
                if !matcher.is_empty() {
                    unmapped.push(format!(
                        "hooks.{event_key} matcher {matcher:?} 未映射（将应用于全部该事件工具）"
                    ));
                }
                let Some(inner) = entry.get("hooks").and_then(|h| h.as_array()) else {
                    continue;
                };
                for h in inner {
                    let cmd = h.get("command").and_then(|c| c.as_str()).unwrap_or("");
                    if cmd.is_empty() {
                        unmapped.push(format!("hooks.{event_key}: 无 command 的条目，跳过"));
                        continue;
                    }
                    let timeout = h
                        .get("timeout")
                        .and_then(|t| t.as_u64())
                        .map(|s| s.min(3600));
                    items.push(ImportItem::Hook {
                        event,
                        command: cmd.to_string(),
                        timeout_secs: timeout,
                    });
                }
            }
        }
    }

    // env：当前配置层无顶层 env 目标，逐项列出会泄露敏感值（token 等），
    // 只给一条汇总提示。
    if let Some(env) = json.get("env").and_then(|e| e.as_object()) {
        if !env.is_empty() {
            unmapped.push(format!(
                "env 段发现 {} 个变量，未导入（配置层无顶层 env 目标，请在 shell 或 provider 环境配置）",
                env.len()
            ));
        }
    }

    (items, unmapped)
}

/// 权限表达式里取工具名（`Bash(git status)` → `Bash`），用于已知/未知判定。
fn tool_key_name(expr: &str) -> &str {
    let expr = expr.trim();
    match expr.find('(') {
        Some(idx) => &expr[..idx],
        None => expr,
    }
    .trim()
}

/// Claude hook 事件 → 内部事件。
fn claude_hook_event(key: &str) -> Option<HookEvent> {
    match key {
        "PreToolUse" => Some(HookEvent::ToolBefore),
        "PostToolUse" => Some(HookEvent::ToolAfter),
        "SessionStart" => Some(HookEvent::SessionStart),
        "SessionEnd" | "Stop" => Some(HookEvent::SessionEnd),
        _ => None,
    }
}

/// 解析 `.mcp.json` 内容 → MCP 导入项。
fn parse_mcp_json(raw: &str) -> Vec<ImportItem> {
    let mut items = Vec::new();
    let json: Json = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return items,
    };
    let Some(servers) = json.get("mcpServers").and_then(|s| s.as_object()) else {
        return items;
    };
    for (name, cfg) in servers {
        let command = cfg.get("command").and_then(|c| c.as_str()).unwrap_or("");
        if command.is_empty() {
            continue;
        }
        let args = cfg
            .get("args")
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let env = cfg
            .get("env")
            .and_then(|e| e.as_object())
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
            name: name.clone(),
            command: command.to_string(),
            args,
            env,
        });
    }
    items
}

/// 扫描 Claude 配置文件（settings.json + .mcp.json），构建导入计划。
///
/// 按 canonical 路径去重：user 层与 project 层指向同一文件（如 HOME == cwd）时
/// 只扫一次，避免重复导入。
pub fn build_plan(cwd: &Path) -> ImportPlan {
    build_plan_with_home(cwd, dirs::home_dir())
}

/// [`build_plan`] 的 home 注入版：测试用临时 HOME 隔离真实 `~/.claude/settings.json`。
/// `home = None` 表示无用户层来源（等价于 HOME 未设置）。
fn build_plan_with_home(cwd: &Path, home: Option<PathBuf>) -> ImportPlan {
    let mut plan = ImportPlan {
        source: ImportSource::Claude,
        ..Default::default()
    };

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(home) = home {
        candidates.push(home.join(".claude/settings.json"));
    }
    candidates.push(cwd.join(".claude/settings.json"));
    candidates.push(cwd.join(".mcp.json"));

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
                if p.extension().and_then(|e| e.to_str()) == Some("json")
                    && p.file_name().and_then(|f| f.to_str()) == Some(".mcp.json")
                {
                    plan.items.extend(parse_mcp_json(&raw));
                } else {
                    let (items, unmapped) = parse_settings_json(&raw);
                    plan.items.extend(items);
                    plan.unmapped.extend(unmapped);
                }
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
    fn permission_expr_parses_tool_and_subject() {
        let (tool, subject) = parse_permission_expr("Bash(git status)");
        assert_eq!(tool, "bash");
        assert_eq!(subject.as_deref(), Some("git status"));

        let (tool, subject) = parse_permission_expr("Read(src/**)");
        assert_eq!(tool, "read_file");
        assert_eq!(subject.as_deref(), Some("src/**"));

        let (tool, subject) = parse_permission_expr("Edit(**)");
        assert_eq!(tool, "edit_file");
        assert_eq!(subject.as_deref(), Some("**"));

        let (tool, subject) = parse_permission_expr("Bash");
        assert_eq!(tool, "bash");
        assert!(subject.is_none());

        let (tool, _) = parse_permission_expr("**");
        assert_eq!(tool, "*");
    }

    #[test]
    fn unknown_tool_lowercased_and_flagged() {
        let (tool, known) = map_claude_tool("SomeFutureTool");
        assert_eq!(tool, "somefuturetool");
        assert!(!known);
    }

    #[test]
    fn settings_parses_permissions_hooks_env() {
        let raw = r#"{
            "permissions": {
                "allow": ["Bash(npm run build)", "Read(src/**)"],
                "ask": ["Bash(npm install)"],
                "deny": ["Bash(rm -rf /)"]
            },
            "hooks": {
                "PreToolUse": [{"matcher": "Bash|Edit", "hooks": [{"type": "command", "command": "python gate.py"}]}]
            },
            "env": {"FOO": "bar"}
        }"#;
        let (items, unmapped) = parse_settings_json(raw);
        let perms: Vec<_> = items
            .iter()
            .filter_map(|i| match i {
                ImportItem::Permission { tool, mode, .. } => Some((tool.as_str(), *mode)),
                _ => None,
            })
            .collect();
        assert!(perms.contains(&("bash", PermissionMode::Allow)));
        assert!(perms.contains(&("read_file", PermissionMode::Allow)));
        assert!(perms.contains(&("bash", PermissionMode::Ask)));
        assert!(perms.contains(&("bash", PermissionMode::Deny)));
        assert_eq!(perms.len(), 4);

        assert!(items.iter().any(|i| matches!(
            i,
            ImportItem::Hook { event: HookEvent::ToolBefore, command, .. }
                if command == "python gate.py"
        )));
        // env 不逐项导入：只给汇总提示，避免泄露敏感值。
        assert!(!items.iter().any(|i| matches!(i, ImportItem::Env { .. })));
        assert!(unmapped.iter().any(|u| u.contains("matcher")));
        assert!(unmapped.iter().any(|u| u.contains("env")));
    }

    #[test]
    fn settings_handles_unknown_hook_event() {
        let raw = r#"{"hooks": {"UserPromptSubmit": [{"hooks": [{"type": "command", "command": "x"}]}]}}"#;
        let (items, unmapped) = parse_settings_json(raw);
        assert!(items.is_empty());
        assert!(unmapped.iter().any(|u| u.contains("UserPromptSubmit")));
    }

    #[test]
    fn mcp_parses_servers() {
        let raw = r#"{
            "mcpServers": {
                "filesystem": {
                    "command": "npx",
                    "args": ["-y", "@modelcontextprotocol/server-filesystem"],
                    "env": {"FOO": "bar"}
                }
            }
        }"#;
        let items = parse_mcp_json(raw);
        assert_eq!(items.len(), 1);
        match &items[0] {
            ImportItem::McpServer {
                name,
                command,
                args,
                env,
            } => {
                assert_eq!(name, "filesystem");
                assert_eq!(command, "npx");
                assert_eq!(
                    args,
                    &vec![
                        "-y".to_string(),
                        "@modelcontextprotocol/server-filesystem".to_string()
                    ]
                );
                assert_eq!(env.len(), 1);
                assert_eq!(env[0].name, "FOO");
            }
            _ => panic!("expected McpServer"),
        }
    }

    #[test]
    fn parse_permission_unknown_flagged_in_plan_unmapped() {
        // 通过 build_plan 的解析路径验证未知工具会进 unmapped：
        // 这里直接复用解析器（文件路径扫描由集成测试覆盖）。
        let raw = r#"{"permissions": {"allow": ["WeirdTool(x)"]}}"#;
        let (items, unmapped) = parse_settings_json(raw);
        assert_eq!(items.len(), 1);
        assert!(unmapped.iter().any(|u| u.contains("WeirdTool")));
    }

    #[test]
    fn build_plan_scans_project_settings_and_mcp_files() {
        // 真实文件路径扫描（补「文件路径扫描由集成测试覆盖」的空缺）：
        // 项目层 .claude/settings.json + .mcp.json 都应被发现并映射。
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap(); // 空 HOME，隔离真实 ~/.claude
        std::fs::create_dir_all(dir.path().join(".claude")).unwrap();
        std::fs::write(
            dir.path().join(".claude/settings.json"),
            r#"{
                "permissions": {"allow": ["Bash(npm run build)", "Read(src/**)"]},
                "env": {"FOO": "bar"}
            }"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join(".mcp.json"),
            r#"{"mcpServers": {"filesystem": {"command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem"]}}}"#,
        )
        .unwrap();

        let plan = build_plan_with_home(dir.path(), Some(home.path().to_path_buf()));
        assert_eq!(
            plan.sources.len(),
            2,
            "settings.json + .mcp.json 都应被扫到"
        );

        let perms: Vec<_> = plan
            .items
            .iter()
            .filter_map(|i| match i {
                ImportItem::Permission { tool, subject, .. } => {
                    Some((tool.as_str(), subject.as_deref()))
                }
                _ => None,
            })
            .collect();
        assert!(perms.contains(&("bash", Some("npm run build"))));
        assert!(perms.contains(&("read_file", Some("src/**"))));

        let mcp = plan
            .items
            .iter()
            .find(|i| matches!(i, ImportItem::McpServer { name, .. } if name == "filesystem"));
        assert!(mcp.is_some(), "mcpServers 应映射为 McpServer 项");

        // env 项出现在 unmapped（配置层无 env 目标，应用层跳过）。
        assert!(plan.unmapped.iter().any(|u| u.contains("env")));
    }

    #[test]
    fn build_plan_ignores_missing_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap(); // 空 HOME
        let plan = build_plan_with_home(dir.path(), Some(home.path().to_path_buf()));
        assert!(plan.sources.is_empty());
        assert!(plan.items.is_empty());
    }
}

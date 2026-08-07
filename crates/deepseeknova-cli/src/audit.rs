//! # exec 审计模式：预执行安全决策预览
//!
//! 把 security 的只读分类器（四层只读 + 危险注入）与 permission 的 gate
//! 决策链（allow/ask/deny + 命中规则）暴露为可读输出，让"一条命令会被怎么
//! 处理"在真实执行前就可见、可审——与真实执行路径同源（分类器与 gate 均
//! 复用实际决策代码），预览即所见、所见即所得。
//!
//! 用法（CLI 层）：
//! - `audit <shell-command>`：审计一条 shell 命令
//! - `audit <tool> <json-args>`：审计任意工具调用
//! - `audit --rules`：显示当前全部权限规则
//! - `--format md|json`、`--workspace <path>`

use deepseeknova_config::Config;
use deepseeknova_permission::{Decision, GatePreview};
use deepseeknova_security::readonly::{CommandAudit, ReadOnlyKind, ReadonlyForm};
use serde::Serialize;
use std::path::Path;

/// audit 目标：shell 命令 或 工具调用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditTarget {
    /// 一条 shell 命令（如 `rm -rf /tmp/x`）。
    ShellCommand(String),
    /// 工具名 + JSON 参数（如 `bash` + `{"command":"git status"}`）。
    ToolCall { tool_name: String, args: String },
}

/// 从 CLI 位置参数解析 audit 目标。
///
/// 双参数且第二个参数是合法 JSON（对象或字符串）时判定为
/// `<tool> <json-args>` 形态；其余一律按 shell 命令空格连接。
pub fn parse_audit_target(args: &[String]) -> anyhow::Result<AuditTarget> {
    if args.is_empty() {
        anyhow::bail!("audit 需要一条 shell 命令，或 <tool> <json-args>");
    }
    if args.len() == 2 {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&args[1]) {
            if v.is_object() || v.is_string() {
                return Ok(AuditTarget::ToolCall {
                    tool_name: args[0].clone(),
                    args: args[1].clone(),
                });
            }
        }
    }
    Ok(AuditTarget::ShellCommand(args.join(" ")))
}

/// exec 审计报表：只读分类 + 权限门控决策链。
#[derive(Debug, Clone, Serialize)]
pub struct AuditReport {
    /// 被审计的 shell 命令（工具调用形态下取自 args.command；非 shell 为 None）。
    pub command: Option<String>,
    /// 只读分类预览（shell 命令；非 shell 工具为 None）。
    pub readonly: Option<CommandAudit>,
    /// 权限门控决策链预览（始终计算——即使门控未启用，也展示"若启用会怎么
    /// 判"；`gate_enabled=false` 时需结合 note 阅读）。
    pub gate: GatePreview,
    /// 权限门控是否启用（config `[permissions] enabled`）。
    pub gate_enabled: bool,
    /// 门控未启用时的说明。
    pub note: Option<String>,
}

/// 构建 exec 审计报表（纯计算，不打印、不写缓存/审计）。
///
/// gate 与真实运行路径 `deepseeknova_runtime::permission_gate_for` 同源
/// 构建（build_permission_gate + workspace_root + TrustStore 信任状态），
/// 保证预览决策与真实执行决策一致。
pub fn build_report(config: &Config, workspace_root: &Path, target: AuditTarget) -> AuditReport {
    let (tool_name, args_json, shell_command, read_only) = match target {
        AuditTarget::ShellCommand(cmd) => {
            let args = serde_json::json!({ "command": cmd });
            ("bash".to_string(), args.to_string(), Some(cmd), false)
        }
        AuditTarget::ToolCall { tool_name, args } => {
            // 从 JSON 参数中提取 shell 命令（若有）——供只读分类预览。
            let command = serde_json::from_str::<serde_json::Value>(&args)
                .ok()
                .and_then(|v| v.get("command").and_then(|c| c.as_str()).map(String::from));
            let read_only = tool_read_only(&tool_name);
            (tool_name, args, command, read_only)
        }
    };

    let readonly = shell_command.as_deref().map(CommandAudit::from_command);

    // 与真实运行路径同源：build_permission_gate + workspace root + 信任状态
    // （镜像 deepseeknova_runtime::permission_gate_for 的构造）。
    let trusted = deepseeknova_config::TrustStore::load().is_trusted(workspace_root);
    let gate = deepseeknova_runtime::build_permission_gate(config)
        .with_workspace_root(workspace_root.to_path_buf())
        .with_trusted(trusted);
    let gate_preview = gate.preview(&tool_name, read_only, &args_json);

    let gate_enabled = config.permissions.enabled;
    let note = (!gate_enabled).then(|| {
        "permission gate disabled in config ([permissions] enabled=false); the decision \
         below is what the gate WOULD decide if enabled"
            .to_string()
    });

    AuditReport {
        command: shell_command,
        readonly,
        gate: gate_preview,
        gate_enabled,
        note,
    }
}

/// 工具名 → 只读标志：内置工具按真实 `read_only()`；未知工具保守视为写工具
/// （`false`，fail-closed——不确定时按可能写处理）。
fn tool_read_only(tool_name: &str) -> bool {
    deepseeknova_tools::all_builtin_tools()
        .iter()
        .find(|t| t.schema().name == tool_name)
        .map(|t| t.read_only())
        .unwrap_or(false)
}

/// 输出 `--rules`（全部权限规则）。
pub fn render_rules(config: &Config, format: &str) -> anyhow::Result<()> {
    if format == "json" {
        let mut deny = Vec::new();
        let mut ask = Vec::new();
        let mut allow = Vec::new();
        for r in &config.permissions.rules {
            let rule = serde_json::json!({ "tool": r.tool, "subject": r.subject });
            match r.mode {
                deepseeknova_config::PermissionMode::Deny => deny.push(rule),
                deepseeknova_config::PermissionMode::Ask => ask.push(rule),
                deepseeknova_config::PermissionMode::Allow => allow.push(rule),
            }
        }
        let out = serde_json::json!({
            "enabled": config.permissions.enabled,
            "default_mode": config.permissions.default_mode,
            "mode": config.permissions.mode,
            "deny": deny,
            "ask": ask,
            "allow": allow,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!("# 权限规则 (deny > ask > allow)");
    println!(
        "门控: {} | 默认模式: {:?} | 预设: {:?}",
        if config.permissions.enabled {
            "启用"
        } else {
            "禁用"
        },
        config.permissions.default_mode,
        config.permissions.mode,
    );
    println!();
    let grouped = [
        ("deny", deepseeknova_config::PermissionMode::Deny),
        ("ask", deepseeknova_config::PermissionMode::Ask),
        ("allow", deepseeknova_config::PermissionMode::Allow),
    ];
    for (label, mode) in grouped {
        println!("[{label}]");
        let rules: Vec<_> = config
            .permissions
            .rules
            .iter()
            .filter(|r| r.mode == mode)
            .collect();
        if rules.is_empty() {
            println!("  （无）");
        } else {
            for r in rules {
                match &r.subject {
                    Some(s) => println!("  {} `{}`", r.tool, s),
                    None => println!("  {}", r.tool),
                }
            }
        }
        println!();
    }
    Ok(())
}

/// 输出一次 audit 的报表。
pub fn render_report(report: &AuditReport, format: &str) -> anyhow::Result<()> {
    match format {
        "json" => println!("{}", serde_json::to_string_pretty(report)?),
        _ => println!("{}", render_markdown(report)),
    }
    Ok(())
}

/// Markdown 渲染。
fn render_markdown(r: &AuditReport) -> String {
    let mut out = String::new();
    out.push_str("# 执行审计 (exec audit)\n\n");

    match &r.command {
        Some(cmd) => out.push_str(&format!("命令: `{cmd}`\n\n")),
        None => out.push_str(&format!(
            "工具: `{}` args: `{}`\n\n",
            r.gate.tool_name, r.gate.args
        )),
    }

    // 只读分类
    out.push_str("## 只读分类\n");
    match &r.readonly {
        Some(ca) => {
            out.push_str(&format!("分类: {}\n", kind_label(ca.kind)));
            out.push_str(&format!(
                "免询问: {}\n",
                if ca.allow_without_prompt {
                    "是"
                } else {
                    "否"
                }
            ));
            out.push_str(&format!("命中形态: `{}`\n", form_label(ca.form)));
            out.push_str(&format!("说明: {}\n", ca.explanation));
        }
        None => out.push_str("（非 shell 命令，不适用）\n"),
    }
    out.push('\n');

    // 权限门控决策
    out.push_str("## 权限门控决策\n");
    if !r.gate_enabled {
        out.push_str("⚠ 权限门控未启用（[permissions] enabled=false）——以下为\"若启用会怎么判\"\n");
        if let Some(n) = &r.note {
            out.push_str(&format!("  {n}\n"));
        }
    }
    let g = &r.gate;
    out.push_str(&format!("判定: {}\n", decision_label(g.decision, g.hard)));
    if !g.reason.is_empty() {
        out.push_str(&format!("原因: {}\n", g.reason));
    }
    if g.matched_rules.is_empty() {
        out.push_str("命中规则: 无（规则回退）\n");
    } else {
        out.push_str("命中规则:\n");
        for hit in &g.matched_rules {
            let subject = hit.rule.subject.as_deref().unwrap_or("*");
            out.push_str(&format!(
                "  - [{}] {} `{}`\n",
                hit.source, hit.rule.tool, subject
            ));
        }
    }
    if let Some(p) = &g.capability.path_outside_workspace {
        out.push_str(&format!("能力检查: ✗ 路径越界 `{p}`（硬拒）\n"));
    } else if g.capability.malformed_args {
        out.push_str("能力检查: ✗ 畸形参数 JSON（fail-closed 硬拒）\n");
    } else {
        let root = g
            .capability
            .workspace_root
            .as_deref()
            .unwrap_or("（未配置）");
        out.push_str(&format!("能力检查: ✓ 路径守卫通过（workspace={root}）\n"));
    }
    if !g.suggestions.is_empty() {
        out.push_str("建议:\n");
        for s in &g.suggestions {
            let subject = s.rule.subject.as_deref().unwrap_or("*");
            out.push_str(&format!(
                "  - allow `{}` `{}` (destination: {})\n",
                s.rule.tool, subject, s.destination
            ));
        }
    }
    out
}

fn kind_label(k: ReadOnlyKind) -> String {
    match k {
        ReadOnlyKind::ReadOnly => "ReadOnly（只读放行）".to_string(),
        ReadOnlyKind::NotReadOnly => "NotReadOnly（非只读）".to_string(),
        ReadOnlyKind::Dangerous => "Dangerous（危险注入）".to_string(),
    }
}

fn form_label(f: ReadonlyForm) -> String {
    match f {
        ReadonlyForm::Dangerous => "dangerous（危险注入预检）",
        ReadonlyForm::Allowlist => "allowlist（任意参数安全）",
        ReadonlyForm::SubcommandAllowlist => "subcommand_allowlist（子命令+flag 白名单）",
        ReadonlyForm::Exact => "exact（精确形式）",
        ReadonlyForm::NoArgs => "noargs（零参数）",
        ReadonlyForm::Fallback => "fallback（规则回退）",
    }
    .to_string()
}

fn decision_label(d: Decision, hard: bool) -> String {
    let base = match d {
        Decision::Allow => "Allow（放行）",
        Decision::Ask => "Ask（需审批）",
        Decision::Deny => "Deny（拒绝）",
    };
    if hard {
        format!("{base} [硬拒，不可规则覆盖]")
    } else {
        base.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_config::Config;

    #[test]
    fn parse_shell_command_joins_args() {
        assert_eq!(
            parse_audit_target(&["rm".into(), "-rf".into(), "/tmp/x".into()]).unwrap(),
            AuditTarget::ShellCommand("rm -rf /tmp/x".to_string())
        );
        // 双参数但第二参数非 JSON → 仍为 shell 命令
        assert_eq!(
            parse_audit_target(&["git".into(), "status".into()]).unwrap(),
            AuditTarget::ShellCommand("git status".to_string())
        );
        // 单参数
        assert_eq!(
            parse_audit_target(&["pwd".into()]).unwrap(),
            AuditTarget::ShellCommand("pwd".to_string())
        );
    }

    #[test]
    fn parse_tool_call_with_json_args() {
        assert_eq!(
            parse_audit_target(&["bash".into(), r#"{"command":"git status"}"#.into()]).unwrap(),
            AuditTarget::ToolCall {
                tool_name: "bash".to_string(),
                args: r#"{"command":"git status"}"#.to_string()
            }
        );
        assert_eq!(
            parse_audit_target(&["read_file".into(), r#"{"path":"a.rs"}"#.into()]).unwrap(),
            AuditTarget::ToolCall {
                tool_name: "read_file".to_string(),
                args: r#"{"path":"a.rs"}"#.to_string()
            }
        );
    }

    #[test]
    fn parse_empty_reports_usage() {
        assert!(parse_audit_target(&[]).is_err());
    }

    #[test]
    fn audit_readonly_command_is_allowed() {
        let config = Config::default();
        let ws = tempfile::tempdir().unwrap();
        let report = build_report(
            &config,
            ws.path(),
            AuditTarget::ShellCommand("git status".to_string()),
        );
        // 只读分类：ReadOnly + 免询问
        let ca = report.readonly.as_ref().unwrap();
        assert_eq!(ca.kind, ReadOnlyKind::ReadOnly);
        assert!(ca.allow_without_prompt);
        // 门控决策：Allow
        assert_eq!(report.gate.decision, Decision::Allow);
        assert!(!report.gate.hard);
        assert!(report.gate_enabled, "default config 门控启用");
    }

    #[test]
    fn audit_writer_command_asks_in_plan_mode() {
        let config = Config::default();
        let ws = tempfile::tempdir().unwrap();
        let report = build_report(
            &config,
            ws.path(),
            AuditTarget::ShellCommand("rm -rf /tmp/x".to_string()),
        );
        assert_eq!(
            report.readonly.as_ref().unwrap().kind,
            ReadOnlyKind::NotReadOnly
        );
        assert!(!report.readonly.as_ref().unwrap().allow_without_prompt);
        assert_eq!(report.gate.decision, Decision::Ask);
        // Ask 附"拒绝即教育"建议
        assert_eq!(report.gate.suggestions.len(), 1);
        let s = &report.gate.suggestions[0];
        assert_eq!(s.rule.tool, "bash");
        assert_eq!(s.rule.subject.as_deref(), Some("rm -rf /tmp/x"));
        assert!(s.rule.exact, "建议规则必须精确匹配");
    }

    #[test]
    fn audit_dangerous_command_hard_denies() {
        let config = Config::default();
        let ws = tempfile::tempdir().unwrap();
        let report = build_report(
            &config,
            ws.path(),
            AuditTarget::ShellCommand("git -c core.pager='cat /etc/passwd' log".to_string()),
        );
        assert_eq!(
            report.readonly.as_ref().unwrap().kind,
            ReadOnlyKind::Dangerous
        );
        assert_eq!(report.gate.decision, Decision::Deny);
        assert!(report.gate.hard, "危险命令为硬拒");
        assert!(report.gate.suggestions.is_empty(), "硬拒不附建议");
    }

    #[test]
    fn audit_tool_call_with_readonly_flag() {
        let config = Config::default();
        let ws = tempfile::tempdir().unwrap();
        // read_file 是内置只读工具 → 放行（与真实执行路径 read_only 一致）
        let report = build_report(
            &config,
            ws.path(),
            AuditTarget::ToolCall {
                tool_name: "read_file".to_string(),
                args: r#"{"path": "src/main.rs"}"#.to_string(),
            },
        );
        assert!(report.readonly.is_none(), "非 shell 工具无只读分类");
        assert_eq!(report.gate.decision, Decision::Allow);
        // 未知工具保守视为写工具 → 默认 Ask
        let report = build_report(
            &config,
            ws.path(),
            AuditTarget::ToolCall {
                tool_name: "some_mcp_tool".to_string(),
                args: r#"{"q": "x"}"#.to_string(),
            },
        );
        assert_eq!(report.gate.decision, Decision::Ask);
    }

    #[test]
    fn audit_markdown_output_is_readable() {
        let config = Config::default();
        let ws = tempfile::tempdir().unwrap();
        let report = build_report(
            &config,
            ws.path(),
            AuditTarget::ShellCommand("git status".to_string()),
        );
        let md = render_markdown(&report);
        assert!(md.contains("git status"), "{md}");
        assert!(md.contains("ReadOnly"), "{md}");
        assert!(md.contains("Allow"), "{md}");
        assert!(md.contains("只读分类"), "{md}");
        assert!(md.contains("权限门控决策"), "{md}");
    }

    #[test]
    fn audit_json_output_serializes_decision_chain() {
        let config = Config::default();
        let ws = tempfile::tempdir().unwrap();
        let report = build_report(
            &config,
            ws.path(),
            AuditTarget::ShellCommand("rm -rf /tmp/x".to_string()),
        );
        let json = serde_json::to_string_pretty(&report).unwrap();
        assert!(json.contains("\"decision\": \"ask\""), "{json}");
        assert!(
            json.contains("\"readonly_kind\": \"not_read_only\""),
            "{json}"
        );
        assert!(json.contains("\"form\": \"fallback\""), "{json}");
        assert!(json.contains("\"suggestions\""), "{json}");
    }

    #[test]
    fn audit_disabled_gate_still_reports_hypothetical() {
        let mut config = Config::default();
        config.permissions.enabled = false;
        let ws = tempfile::tempdir().unwrap();
        let report = build_report(
            &config,
            ws.path(),
            AuditTarget::ShellCommand("git status".to_string()),
        );
        assert!(!report.gate_enabled);
        assert!(report.note.is_some());
        // 仍展示只读分类与"若启用会怎么判"
        assert_eq!(
            report.readonly.as_ref().unwrap().kind,
            ReadOnlyKind::ReadOnly
        );
        assert_eq!(report.gate.decision, Decision::Allow);
    }

    #[test]
    fn audit_reports_deny_rule_in_chain() {
        // deny 规则覆盖只读免询问：链上展示命中 deny 规则
        let mut config = Config::default();
        config
            .permissions
            .rules
            .push(deepseeknova_config::PermissionRule {
                tool: "bash".to_string(),
                subject: Some("git *".to_string()),
                mode: deepseeknova_config::PermissionMode::Deny,
            });
        let ws = tempfile::tempdir().unwrap();
        let report = build_report(
            &config,
            ws.path(),
            AuditTarget::ShellCommand("git status".to_string()),
        );
        assert_eq!(report.gate.decision, Decision::Deny);
        assert_eq!(report.gate.matched_rules.len(), 1);
        assert_eq!(report.gate.matched_rules[0].source, "deny");
        assert!(
            report.gate.reason.contains("git *"),
            "{}",
            report.gate.reason
        );
    }

    #[test]
    fn tool_read_only_known_and_unknown() {
        // 内置只读工具（read_file）与写工具（bash）区分
        assert!(tool_read_only("read_file"));
        assert!(tool_read_only("grep"));
        assert!(!tool_read_only("bash"));
        assert!(!tool_read_only("write"));
        // 未知工具保守 false（fail-closed）
        assert!(!tool_read_only("totally_unknown_tool"));
    }
}

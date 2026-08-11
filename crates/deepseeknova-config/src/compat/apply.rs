//! 导入项写回：把 [`ImportPlan`] 应用到目标配置层（user / project）。
//!
//! 写回基于 `toml::Value` 级操作：读现有目标文件 → 在对应 key 下追加导入项 →
//! 写回。不经过 [`crate::Config::merge`]（其 `mcp_servers` 是整体替换，会丢
//! 用户已有 MCP）。`env` 项因配置层无顶层 env 目标被跳过（由调用方提示）。

use super::{ImportItem, ImportPlan, ImportScope};
use deepseeknova_core::DeepseeknovaError;
use std::path::{Path, PathBuf};
use toml::Value as Toml;

/// 应用导入计划到指定配置层。
///
/// 返回 `(写入路径, 实际写入项数, 跳过项数)`；`env` 项与已有同名项跳过。
pub fn apply(
    plan: &ImportPlan,
    scope: ImportScope,
    cwd: &Path,
) -> Result<(PathBuf, usize, usize), DeepseeknovaError> {
    let path = scope.path(cwd)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut table: toml::map::Map<String, Toml> = if path.exists() {
        let raw = std::fs::read_to_string(&path).map_err(|e| {
            DeepseeknovaError::Io(std::io::Error::new(
                e.kind(),
                format!("failed to read {}: {e}", path.display()),
            ))
        })?;
        toml::from_str::<Toml>(&raw)
            .map_err(|e| {
                DeepseeknovaError::config(format!("failed to parse {}: {e}", path.display()))
            })?
            .as_table()
            .cloned()
            .unwrap_or_default()
    } else {
        toml::map::Map::new()
    };

    let mut applied = 0usize;
    let mut skipped = 0usize;
    // 是否写入了任何 Hook 项：若是，需置 `[hooks] enabled = true`，
    // 否则导入的 hook 因总开关缺省 false 永不执行（HooksConfig.enabled 默认 false）。
    let mut wrote_hook = false;

    for item in &plan.items {
        match item {
            ImportItem::Permission {
                tool,
                subject,
                mode,
            } => {
                let mut rule = toml::map::Map::new();
                rule.insert("tool".into(), Toml::String(tool.clone()));
                if let Some(s) = subject {
                    rule.insert("subject".into(), Toml::String(s.clone()));
                }
                rule.insert(
                    "mode".into(),
                    Toml::String(
                        match mode {
                            crate::PermissionMode::Allow => "allow",
                            crate::PermissionMode::Ask => "ask",
                            crate::PermissionMode::Deny => "deny",
                        }
                        .to_string(),
                    ),
                );
                push_array(&mut table, &["permissions", "rules"], Toml::Table(rule));
                applied += 1;
            }
            ImportItem::McpServer {
                name,
                command,
                args,
                env,
            } => {
                // 同名已有 MCP 跳过，避免重复。
                if mcp_exists(&table, name) {
                    skipped += 1;
                    continue;
                }
                let mut srv = toml::map::Map::new();
                srv.insert("name".into(), Toml::String(name.clone()));
                srv.insert("command".into(), Toml::String(command.clone()));
                srv.insert(
                    "args".into(),
                    Toml::Array(args.iter().map(|a| Toml::String(a.clone())).collect()),
                );
                srv.insert(
                    "env".into(),
                    Toml::Array(
                        env.iter()
                            .map(|e| {
                                let mut m = toml::map::Map::new();
                                m.insert("name".into(), Toml::String(e.name.clone()));
                                m.insert("value".into(), Toml::String(e.value.clone()));
                                Toml::Table(m)
                            })
                            .collect(),
                    ),
                );
                srv.insert("enabled".into(), Toml::Boolean(true));
                push_array(&mut table, &["mcp_servers"], Toml::Table(srv));
                applied += 1;
            }
            ImportItem::Hook {
                event,
                command,
                timeout_secs,
            } => {
                let mut hook = toml::map::Map::new();
                hook.insert("command".into(), Toml::String(command.clone()));
                hook.insert("args".into(), Toml::Array(vec![]));
                if let Some(t) = timeout_secs {
                    hook.insert("timeout_secs".into(), Toml::Integer(*t as i64));
                }
                hook.insert("disabled".into(), Toml::Boolean(false));
                push_array(
                    &mut table,
                    &["hooks", event.config_key()],
                    Toml::Table(hook),
                );
                wrote_hook = true;
                applied += 1;
            }
            ImportItem::Env { .. } => {
                // 配置层无顶层 env 目标，跳过（调用方提示）。
                skipped += 1;
            }
        }
    }

    // 全部项都被跳过时（如只有 env），不写盘，避免造出空配置段。
    if applied == 0 {
        return Ok((path, 0, skipped));
    }

    // 写入了 Hook 项 → 确保 `[hooks] enabled = true`，否则装配层不挂载。
    if wrote_hook {
        ensure_hooks_enabled(&mut table);
    }

    let content = toml::to_string_pretty(&Toml::Table(table)).map_err(|e| {
        DeepseeknovaError::config(format!("failed to serialize imported config: {e}"))
    })?;
    std::fs::write(&path, content)?;
    Ok((path, applied, skipped))
}

/// 在嵌套 key 路径（如 `["permissions", "rules"]`）下的数组末尾追加一项。
/// 路径上的中间表自动创建；末级不存在时创建数组。
fn push_array(table: &mut toml::map::Map<String, Toml>, keys: &[&str], value: Toml) {
    let last = keys.len() - 1;
    let mut cursor = table;
    for key in &keys[..last] {
        let entry = cursor
            .entry((*key).to_string())
            .or_insert_with(|| Toml::Table(toml::map::Map::new()));
        match entry {
            Toml::Table(t) => cursor = t,
            _ => {
                // 类型冲突（键已存在且非表）：用新表覆盖。
                *entry = Toml::Table(toml::map::Map::new());
                if let Toml::Table(t) = entry {
                    cursor = t;
                } else {
                    return;
                }
            }
        }
    }
    let arr = cursor
        .entry(keys[last].to_string())
        .or_insert_with(|| Toml::Array(vec![]));
    if let Toml::Array(a) = arr {
        a.push(value);
    }
}

/// 判断 mcp_servers 里是否已有同名 server（避免重复导入）。
fn mcp_exists(table: &toml::map::Map<String, Toml>, name: &str) -> bool {
    table
        .get("mcp_servers")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .any(|s| s.get("name").and_then(|n| n.as_str()) == Some(name))
        })
        .unwrap_or(false)
}

/// 确保 `[hooks] enabled = true`（幂等）：已显式设置则不覆盖。
fn ensure_hooks_enabled(table: &mut toml::map::Map<String, Toml>) {
    let hooks = table
        .entry("hooks".to_string())
        .or_insert_with(|| Toml::Table(toml::map::Map::new()));
    if let Toml::Table(hooks) = hooks {
        hooks
            .entry("enabled".to_string())
            .or_insert_with(|| Toml::Boolean(true));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compat::{HookEvent, ImportItem, ImportSource};
    use crate::PermissionMode;
    use std::path::PathBuf;

    fn sample_plan() -> ImportPlan {
        ImportPlan {
            source: ImportSource::Claude,
            items: vec![
                ImportItem::Permission {
                    tool: "bash".into(),
                    subject: Some("npm run build".into()),
                    mode: PermissionMode::Allow,
                },
                ImportItem::McpServer {
                    name: "filesystem".into(),
                    command: "npx".into(),
                    args: vec!["-y".into()],
                    env: vec![],
                },
                ImportItem::Hook {
                    event: HookEvent::ToolBefore,
                    command: "gate.sh".into(),
                    timeout_secs: Some(5),
                },
            ],
            unmapped: vec![],
            sources: vec![],
        }
    }

    #[test]
    fn apply_writes_permissions_mcp_and_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let plan = sample_plan();
        let (path, applied, skipped) = apply(&plan, ImportScope::Project, dir.path()).unwrap();
        assert_eq!(applied, 3);
        assert_eq!(skipped, 0);
        assert!(path.exists());

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("tool = \"bash\""), "含权限规则: {raw}");
        assert!(raw.contains("mode = \"allow\""));
        assert!(raw.contains("name = \"filesystem\""));
        assert!(raw.contains("command = \"gate.sh\""));
        assert!(raw.contains("tool_before"), "hooks 事件组: {raw}");
        assert!(
            raw.contains("[hooks]") && raw.contains("enabled = true"),
            "写入了 hook 必须置 [hooks] enabled = true: {raw}"
        );
    }

    #[test]
    fn apply_without_hooks_does_not_force_hooks_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let mut plan = sample_plan();
        plan.items.retain(|i| !matches!(i, ImportItem::Hook { .. }));
        let (path, applied, _) = apply(&plan, ImportScope::Project, dir.path()).unwrap();
        assert_eq!(applied, 2);
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            !raw.contains("[hooks]"),
            "无 hook 项时不应写 [hooks] 段: {raw}"
        );
    }

    #[test]
    fn apply_does_not_override_explicit_hooks_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("deepseeknova.toml");
        std::fs::write(&cfg, "[hooks]\nenabled = false\n").unwrap();
        let plan = sample_plan();
        apply(&plan, ImportScope::Project, dir.path()).unwrap();
        let raw = std::fs::read_to_string(&cfg).unwrap();
        assert!(
            raw.contains("enabled = false"),
            "用户显式 enabled=false 不得被覆盖: {raw}"
        );
    }

    #[test]
    fn apply_preserves_existing_mcp_and_skips_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("deepseeknova.toml");
        std::fs::write(
            &cfg,
            "[[mcp_servers]]\nname = \"existing\"\ncommand = \"uvx\"\n",
        )
        .unwrap();

        // 第一次导入 filesystem。
        let plan = sample_plan();
        apply(&plan, ImportScope::Project, dir.path()).unwrap();
        // 再次导入（同一 plan）→ filesystem 应跳过。
        let (_, applied, skipped) = apply(&plan, ImportScope::Project, dir.path()).unwrap();
        assert_eq!(applied, 2, "MCP 重复跳过，其余仍写入");
        assert_eq!(skipped, 1);

        let raw = std::fs::read_to_string(&cfg).unwrap();
        assert!(raw.contains("existing"), "既有 MCP 保留");
        assert_eq!(
            raw.matches("name = \"filesystem\"").count(),
            1,
            "filesystem 不应重复: {raw}"
        );
    }

    #[test]
    fn apply_env_only_plan_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let plan = ImportPlan {
            source: ImportSource::Claude,
            items: vec![ImportItem::Env {
                key: "FOO".into(),
                value: "bar".into(),
            }],
            unmapped: vec![],
            sources: vec![],
        };
        let (path, applied, skipped) = apply(&plan, ImportScope::Project, dir.path()).unwrap();
        assert_eq!(applied, 0);
        assert_eq!(skipped, 1);
        assert!(!path.exists(), "全部跳过时不写盘");
    }

    #[test]
    fn user_scope_path_points_to_home_config() {
        let scope = ImportScope::User;
        let p: PathBuf = scope.path(std::path::Path::new("/tmp")).unwrap();
        assert!(p.ends_with(".deepseeknova/config.toml"), "{p:?}");
    }

    #[test]
    fn project_scope_path_resolves_under_cwd() {
        let scope = ImportScope::Project;
        let p = scope.path(std::path::Path::new("/tmp/foo")).unwrap();
        assert_eq!(p, std::path::Path::new("/tmp/foo/deepseeknova.toml"));
    }
}

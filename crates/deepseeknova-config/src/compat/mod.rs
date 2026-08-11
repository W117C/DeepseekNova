//! 外部 Agent 工具配置导入（Claude Code / Codex → deepseeknova 分层配置）。
//!
//! 设计对齐 Grok 的 `claude_import`：**默认只预览**（`discover` + `build_plan`），
//! 用户确认后 `apply` 才写入目标配置层（user `~/.deepseeknova/config.toml` /
//! project `deepseeknova.toml`）。写回基于 `toml::Value` 级操作，保留目标文件
//! 已有的段（`Config::merge` 对 `mcp_servers` 是整体替换，直接覆盖会丢已有 MCP）。

pub mod apply;
pub mod claude;
pub mod codex;

use deepseeknova_core::DeepseeknovaError;

/// 导入来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImportSource {
    /// Claude Code（`settings.json` + `.mcp.json`）。
    #[default]
    Claude,
    /// Codex CLI（`config.toml`）。
    Codex,
}

impl ImportSource {
    /// 命令行显示名。
    pub fn label(self) -> &'static str {
        match self {
            ImportSource::Claude => "claude",
            ImportSource::Codex => "codex",
        }
    }
}

/// 导入写入的目标配置层。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportScope {
    /// 用户层 `~/.deepseeknova/config.toml`（默认）。
    User,
    /// 项目层 `deepseeknova.toml`。
    Project,
}

impl ImportScope {
    /// 目标配置文件路径（`cwd` 为项目层基准）。
    pub fn path(self, cwd: &std::path::Path) -> std::path::PathBuf {
        match self {
            ImportScope::User => crate::user_config_path()
                .unwrap_or_else(|| std::path::PathBuf::from("~/.deepseeknova/config.toml")),
            ImportScope::Project => cwd.join("deepseeknova.toml"),
        }
    }
}

/// 内部 hook 事件（对齐 [`crate::HooksConfig`] 的事件组）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    /// 工具调用前预检（Claude `PreToolUse`）。
    ToolBefore,
    /// 工具调用后通知（Claude `PostToolUse`）。
    ToolAfter,
    /// 会话启动（Claude `SessionStart`）。
    SessionStart,
    /// 会话结束（Claude `SessionEnd` / `Stop`）。
    SessionEnd,
    /// 失败收尾（Claude `Stop` 的失败近似）。
    Failure,
}

impl HookEvent {
    /// HooksConfig 里对应的事件组字段名（TOML key）。
    pub fn config_key(self) -> &'static str {
        match self {
            HookEvent::ToolBefore => "tool_before",
            HookEvent::ToolAfter => "tool_after",
            HookEvent::SessionStart => "session_start",
            HookEvent::SessionEnd => "session_end",
            HookEvent::Failure => "failure",
        }
    }
}

/// 一条导入项：外部配置 → 内部配置的映射。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportItem {
    /// 权限规则（Claude/Codex `permissions.allow/ask/deny` 表达式）。
    Permission {
        /// 内部工具名（已映射，如 `bash` / `read_file`）。
        tool: String,
        /// subject 模式（如 `git status`、`src/**`）。
        subject: Option<String>,
        /// 裁决模式。
        mode: crate::PermissionMode,
    },
    /// MCP 服务器（Claude `.mcp.json` `mcpServers` / Codex `[[mcp_servers]]`）。
    McpServer {
        /// 逻辑名。
        name: String,
        /// 启动命令。
        command: String,
        /// 命令参数。
        args: Vec<String>,
        /// 环境变量。
        env: Vec<crate::EnvEntry>,
    },
    /// Hook 命令（Claude `hooks` / Codex `[hooks]`）。
    Hook {
        /// 内部事件组。
        event: HookEvent,
        /// 外部命令。
        command: String,
        /// 超时秒数（可选）。
        timeout_secs: Option<u64>,
    },
    /// 环境变量（Claude `env`）。当前配置层无顶层 env 目标，仅报告不写入。
    Env {
        /// 键。
        key: String,
        /// 值。
        value: String,
    },
}

impl ImportItem {
    /// 一行预览（`发现条目 → 映射到内部配置`）。
    pub fn preview_line(&self) -> String {
        match self {
            ImportItem::Permission {
                tool,
                subject,
                mode,
            } => {
                let s = subject.as_deref().unwrap_or("*");
                let mode_str = match mode {
                    crate::PermissionMode::Allow => "allow",
                    crate::PermissionMode::Ask => "ask",
                    crate::PermissionMode::Deny => "deny",
                };
                format!("[permission] {tool} <{s}>  ({mode_str})")
            }
            ImportItem::McpServer { name, command, .. } => {
                format!("[mcp] {name} ← {command}")
            }
            ImportItem::Hook { event, command, .. } => {
                format!("[hook] {} ← {}", event.config_key(), command)
            }
            ImportItem::Env { key, value } => format!("[env] {key}={value}"),
        }
    }
}

/// 导入计划：分组预览 + 未映射项报告。
#[derive(Debug, Clone, Default)]
pub struct ImportPlan {
    /// 来源。
    pub source: ImportSource,
    /// 可应用项。
    pub items: Vec<ImportItem>,
    /// 未能映射的原始条目（不丢弃，报告给用户）。
    pub unmapped: Vec<String>,
    /// 原始来源文件路径（诊断用）。
    pub sources: Vec<std::path::PathBuf>,
}

impl ImportPlan {
    /// 总发现数（含未映射）。
    pub fn total(&self) -> usize {
        self.items.len() + self.unmapped.len()
    }

    /// 是否没有可应用项。
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// 目标配置文件路径的解析结果，供 CLI 预览。
pub fn import_scope_from_arg(arg: &str) -> Result<ImportScope, DeepseeknovaError> {
    match arg {
        "user" => Ok(ImportScope::User),
        "project" => Ok(ImportScope::Project),
        other => Err(DeepseeknovaError::config(format!(
            "invalid import scope '{other}' (expected 'user' | 'project')"
        ))),
    }
}

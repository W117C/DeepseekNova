//! # AgentManifest — markdown 前端文件声明的子代理
//!
//! 借鉴 Claude Code subagents / opencode agents / Codex skills 的 markdown
//! 前端文件能力：一个 `.md` 文件以 `---` 头块声明元数据（name / description /
//! tools / model / gate / capabilities / max_turns），正文即该子代理的系统提示。
//!
//! 与既有 TOML 预设（[`DelegatePreset`] / [`SubAgentConfig`]）的关系：
//! 本模块提供**声明源**与到两套消费结构（`DelegatePreset`、`SubAgentConfig`）
//! 的转换，保持预设兼容——markdown 文件是声明源的新增通道，不替代既有结构。
//!
//! 头块格式（front-matter 风格，`key: value` 或 `key = value` 均可）：
//!
//! ```text
//! ---
//! name: research
//! description: Read-only codebase investigation.
//! tools = ["read_file", "grep", "search_code"]
//! model = "deepseek-v4-flash"
//! gate = "fail_closed"
//! capabilities = ["read", "memory_read"]
//! max_turns = 8
//! ---
//! You are a research sub-agent...
//! ```
//!
//! `name` 必填且须为合法标识符（`[A-Za-z0-9][A-Za-z0-9_-]*`，@-mention 可寻址）。
//! `tools` / `capabilities` 为逗号分隔列表；`max_turns` 别名 `max_steps`。
//! 正文（第二个 `---` 之后）trim 后作为系统提示。

use crate::delegate::DelegatePreset;
use crate::sub_agent::SubAgentConfig;
use crate::task_spec::TaskSpec;
use deepseeknova_core::Tool;
use deepseeknova_security::capability::Capability;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 未显式声明时的默认步数上限。
pub const DEFAULT_MAX_TURNS: usize = 10;
/// 默认 agents 目录（`.deepseeknova/agents/`）。
pub const DEFAULT_AGENT_DIR: &str = ".deepseeknova/agents";

/// 前端头块支持的元数据键。
const FRONTMATTER_DELIM: &str = "---";

// ---------------------------------------------------------------------------
// 声明式元数据模型
// ---------------------------------------------------------------------------

/// 子代理权限门模式（per-agent gate）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentGateMode {
    /// 继承共享 PermissionGate（默认）。
    #[default]
    Inherit,
    /// 无门（绕过共享 gate，工具直接执行）。
    None,
    /// Fail-closed：无共享 gate 时不执行任何工具；有共享 gate 时
    /// Ask/Deny 均拒绝，仅 Allow 放行（子代理本就无审批通道）。
    FailClosed,
}

impl AgentGateMode {
    /// 从声明文本解析：`inherit` / `none` / `fail_closed`（大小写不敏感）。
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "inherit" => Some(AgentGateMode::Inherit),
            "none" | "bypass" => Some(AgentGateMode::None),
            "fail_closed" | "failclosed" => Some(AgentGateMode::FailClosed),
            _ => None,
        }
    }

    /// 反序列化为声明层字符串（`inherit` / `none` / `fail_closed`）。
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentGateMode::Inherit => "inherit",
            AgentGateMode::None => "none",
            AgentGateMode::FailClosed => "fail_closed",
        }
    }
}

/// 子代理能力（映射到 [`Capability`]）。字符串名面向声明层，
/// [`Self::to_capability`] 对接 security crate 的执行层门禁。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentCapability {
    /// 读文件/读目录。
    FileRead,
    /// 写/编辑文件。
    FileWrite,
    /// 执行 shell 命令。
    CommandExecute,
    /// 网络访问（fetch/web）。
    NetworkAccess,
    /// 调用 MCP 工具。
    McpInvoke,
    /// 读取记忆/上下文。
    MemoryRead,
    /// 写入记忆/技能。
    MemoryWrite,
}

impl AgentCapability {
    /// 从声明文本解析：`read` / `write` / `execute` / `network` / `mcp` /
    /// `memory_read` / `memory_write` 及常见别名（大小写不敏感）。
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "read" | "file_read" | "fileread" => Some(AgentCapability::FileRead),
            "write" | "file_write" | "filewrite" => Some(AgentCapability::FileWrite),
            "execute" | "command_execute" | "commandexecute" | "bash" => {
                Some(AgentCapability::CommandExecute)
            }
            "network" | "network_access" | "networkaccess" | "web" => {
                Some(AgentCapability::NetworkAccess)
            }
            "mcp" | "mcp_invoke" | "mcpinvoke" => Some(AgentCapability::McpInvoke),
            "memory_read" | "memoryread" => Some(AgentCapability::MemoryRead),
            "memory_write" | "memorywrite" => Some(AgentCapability::MemoryWrite),
            _ => None,
        }
    }

    /// 反序列化为声明层字符串（`read` / `write` / `execute` / `network` /
    /// `mcp` / `memory_read` / `memory_write`）。
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentCapability::FileRead => "read",
            AgentCapability::FileWrite => "write",
            AgentCapability::CommandExecute => "execute",
            AgentCapability::NetworkAccess => "network",
            AgentCapability::McpInvoke => "mcp",
            AgentCapability::MemoryRead => "memory_read",
            AgentCapability::MemoryWrite => "memory_write",
        }
    }

    /// 转换为 security crate 的执行层门禁 [`Capability`]。
    pub fn to_capability(&self) -> Capability {
        match self {
            AgentCapability::FileRead => Capability::FileRead,
            AgentCapability::FileWrite => Capability::FileWrite,
            AgentCapability::CommandExecute => Capability::CommandExecute,
            AgentCapability::NetworkAccess => Capability::NetworkAccess,
            AgentCapability::McpInvoke => Capability::McpInvoke,
            AgentCapability::MemoryRead => Capability::MemoryRead,
            AgentCapability::MemoryWrite => Capability::MemoryWrite,
        }
    }
}

/// per-agent 权限声明：门模式 + 能力白名单。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentPermission {
    /// 权限门模式（inherit / none / fail_closed）。
    pub gate: AgentGateMode,
    /// 能力白名单；空 = 继承完整能力集（不做裁剪）。
    pub capabilities: Vec<AgentCapability>,
}

impl AgentPermission {
    /// 空权限声明（默认门模式 + 空能力白名单）。
    pub fn new() -> Self {
        Self::default()
    }
}

/// 单个 markdown 前端文件解析出的子代理声明。
#[derive(Debug, Clone)]
pub struct AgentManifest {
    /// 子代理名（合法标识符，@-mention 可寻址）。
    pub name: String,
    /// 工具/调用方展示用的一句话描述。
    pub description: String,
    /// 正文（`---` 头块之后），即系统提示。
    pub system_prompt: String,
    /// 工具 schema 名白名单；空 = 无工具（纯推理/文本子代理）。
    pub tools: Vec<String>,
    /// per-agent 模型覆盖（None = 使用默认 provider）。
    pub model: Option<String>,
    /// per-agent 权限声明。
    pub permission: AgentPermission,
    /// 执行步数上限（`max_turns`，别名 `max_steps`）。
    pub max_turns: usize,
}

/// 头块解析失败 / 校验失败。
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// 文件顶部缺少 `---` 头块起始标记。
    #[error(
        "file is not a markdown agent manifest: expected a `---` front-matter block at the top"
    )]
    NoFrontmatter,
    /// 头块缺少闭合的 `---`。
    #[error("front-matter block is not closed (missing closing `---`)")]
    UnclosedFrontmatter,
    /// 头块缺少必填字段（如 `name`）。
    #[error("front-matter is missing required field `{0}`")]
    MissingField(&'static str),
    /// agent 名不合法（须匹配 `[A-Za-z0-9][A-Za-z0-9_-]*`）。
    #[error("`{0}` is not a valid agent name (use [A-Za-z0-9][A-Za-z0-9_-]*)")]
    InvalidName(String),
    /// 声明了未知 capability。
    #[error("unknown capability `{0}` (valid: read, write, execute, network, mcp, memory_read, memory_write)")]
    InvalidCapability(String),
    /// 声明了未知 gate 模式。
    #[error("unknown gate `{0}` (valid: inherit, none, fail_closed)")]
    InvalidGate(String),
    /// `max_turns` / `max_steps` 不是正整数。
    #[error("`max_turns` must be a positive integer, got `{0}`")]
    InvalidMaxTurns(String),
    /// 多个 manifest 文件声明了同名 agent。
    #[error("duplicate agent name `{0}` across manifest files")]
    DuplicateName(String),
    /// 读取 manifest 文件失败。
    #[error("failed to read `{path}`: {source}")]
    Io {
        /// 出错的 manifest 文件路径。
        path: PathBuf,
        /// 底层 IO 错误。
        #[source]
        source: std::io::Error,
    },
    /// 解析 manifest 失败（内层错误见 `source`）。
    #[error("failed to parse manifest `{path}`: {source}")]
    Parse {
        /// 出错的 manifest 文件路径。
        path: PathBuf,
        /// 内层解析错误。
        #[source]
        source: Box<ManifestError>,
    },
}

/// 把 [`ManifestError`] 转换为 [`deepseeknova_core::DeepseeknovaError`]。
///
/// orphan rule：impl 放在拥有 `ManifestError` 的本 crate。`?` 可直接把
/// `Result<_, ManifestError>` 用于返回 `Result<_, DeepseeknovaError>` 的函数。
impl From<ManifestError> for deepseeknova_core::DeepseeknovaError {
    fn from(err: ManifestError) -> Self {
        deepseeknova_core::DeepseeknovaError::Agent {
            message: err.to_string(),
            source: Some(Box::new(err)),
        }
    }
}

// ---------------------------------------------------------------------------
// 头块解析
// ---------------------------------------------------------------------------

/// 解析原始键值对头块（`key: value` 或 `key = value`）。
fn parse_header_lines(header: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in header.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // `:` 分隔优先（front-matter 惯例）；无冒号再尝试 `=`
        let kv = if let Some((k, v)) = trimmed.split_once(':') {
            Some((k, v))
        } else if let Some((k, v)) = trimmed.split_once('=') {
            Some((k, v))
        } else {
            None
        };
        if let Some((k, v)) = kv {
            let key = k.trim();
            let value = v.trim();
            if !key.is_empty() {
                out.push((key.to_string(), value.to_string()));
            }
        }
    }
    out
}

/// 去掉首尾引号（`"` / `'`）。
fn unquote(s: &str) -> String {
    s.trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
        .to_string()
}

/// 解析列表值：`[a, b, c]`（可带引号），或裸逗号分隔；空 → 空列表。
fn parse_list(value: &str) -> Vec<String> {
    let trimmed = value.trim();
    let inner = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(trimmed);
    inner
        .split(',')
        .map(unquote)
        .filter(|s| !s.is_empty())
        .collect()
}

/// 判断字符串是否为合法 agent 名（`[A-Za-z0-9][A-Za-z0-9_-]*`）。
pub fn is_valid_agent_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// 把一个 `---` 前端文件解析为 [`AgentManifest`]。正文即系统提示。
pub fn parse_manifest(text: &str) -> Result<AgentManifest, ManifestError> {
    let (header, body) = split_frontmatter(text)?;

    let kv = parse_header_lines(header);
    let get = |key: &str| kv.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());

    let name = get("name").ok_or(ManifestError::MissingField("name"))?;
    let name = unquote(&name);
    if !is_valid_agent_name(&name) {
        return Err(ManifestError::InvalidName(name));
    }

    let description = unquote(&get("description").unwrap_or_default())
        .trim()
        .to_string();
    let tools = get("tools").map(|v| parse_list(&v)).unwrap_or_default();
    let model = get("model").map(|v| unquote(&v)).filter(|v| !v.is_empty());

    let gate = match get("gate") {
        None => AgentGateMode::Inherit,
        Some(v) => {
            let v = unquote(&v);
            AgentGateMode::parse(&v).ok_or(ManifestError::InvalidGate(v))?
        }
    };
    let capabilities = match get("capabilities") {
        None => Vec::new(),
        Some(v) => parse_list(&v)
            .into_iter()
            .map(|c| AgentCapability::parse(&c).ok_or(ManifestError::InvalidCapability(c.clone())))
            .collect::<Result<Vec<_>, _>>()?,
    };

    let max_turns = match get("max_turns").or_else(|| get("max_steps")) {
        None => DEFAULT_MAX_TURNS,
        Some(v) => {
            let v = unquote(&v);
            let parsed: usize = v
                .parse()
                .map_err(|_| ManifestError::InvalidMaxTurns(v.clone()))?;
            if parsed == 0 {
                return Err(ManifestError::InvalidMaxTurns(v));
            }
            parsed
        }
    };

    Ok(AgentManifest {
        name,
        description,
        system_prompt: body.trim().to_string(),
        tools,
        model,
        permission: AgentPermission { gate, capabilities },
        max_turns,
    })
}

/// 拆分 `---` 头块与正文。正文可为空。
fn split_frontmatter(text: &str) -> Result<(&str, &str), ManifestError> {
    let first_line = text.lines().next().unwrap_or("");
    if first_line.trim() != FRONTMATTER_DELIM {
        return Err(ManifestError::NoFrontmatter);
    }
    // 从头块起始行之后找闭合 `---`
    let rest = &text[first_line.len()..];
    let Some(end) = rest.find(&format!("\n{FRONTMATTER_DELIM}")) else {
        return Err(ManifestError::UnclosedFrontmatter);
    };
    // 头块内容 = 起始 `---` 之后到闭合 `---` 之前（去掉首行换行）
    let header = &rest[..end];
    let body_start = end + FRONTMATTER_DELIM.len() + 1; // + 换行
    let body = &rest[body_start.min(rest.len())..];
    Ok((header, body))
}

// ---------------------------------------------------------------------------
// 目录扫描
// ---------------------------------------------------------------------------

/// 扫描目录下全部 `*.md` 文件并解析。目录不存在 → 空列表；
/// 任一文件解析失败或出现重名 → 整体报错（fail-fast，防静默错配）。
pub fn load_dir(dir: &Path) -> Result<Vec<AgentManifest>, ManifestError> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let read = std::fs::read_dir(dir).map_err(|source| ManifestError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    let mut files: Vec<PathBuf> = read
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "md").unwrap_or(false))
        .collect();
    files.sort();

    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for path in files {
        let text = std::fs::read_to_string(&path).map_err(|source| ManifestError::Io {
            path: path.clone(),
            source,
        })?;
        let manifest = parse_manifest(&text).map_err(|source| ManifestError::Parse {
            path: path.clone(),
            source: Box::new(source),
        })?;
        if !seen.insert(manifest.name.clone()) {
            return Err(ManifestError::DuplicateName(manifest.name.clone()));
        }
        out.push(manifest);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// 转换到既有消费结构
// ---------------------------------------------------------------------------

impl AgentManifest {
    /// 转换为任务书（tools/max_steps 供渲染与注册用；task 为空串，因
    /// 系统提示独立承载）。
    pub fn to_task_spec(&self) -> TaskSpec {
        TaskSpec::simple(
            self.name.clone(),
            "",
            self.tools.clone(),
            self.max_turns.max(1),
        )
    }

    /// 转换为委派引擎预设（模型/权限随 preset 携带）。
    pub fn to_delegate_preset(&self) -> DelegatePreset {
        DelegatePreset {
            name: self.name.clone(),
            system_prompt: self.system_prompt.clone(),
            spec: self.to_task_spec(),
            config_inputs: Default::default(),
            model: self.model.clone(),
            permission: self.permission.clone(),
            allow_recursion: false,
        }
    }

    /// 转换为 `SubAgentConfig`。`tools` 为按名字白名单解析后的工具对象。
    pub fn to_sub_agent_config(&self, tools: Vec<Arc<dyn Tool>>) -> SubAgentConfig {
        SubAgentConfig::new(self.name.clone(), self.system_prompt.clone())
            .with_task_spec(self.to_task_spec())
            .with_tools(tools)
            .with_max_steps(self.max_turns.max(1))
            .with_model(self.model.clone())
            .with_permission(self.permission.clone())
    }

    /// 从内置工具源按白名单挑出工具对象（镜像 runtime 的过滤逻辑，
    /// 供 `SubAgentRunner` / `DelegateEngine` 构造时复用）。
    pub fn tools_from_registry<'a, I>(&self, base: I) -> Vec<Arc<dyn Tool>>
    where
        I: IntoIterator<Item = &'a Arc<dyn Tool>>,
    {
        base.into_iter()
            .filter(|t| self.tools.iter().any(|allow| allow == &t.schema().name))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_core::ToolContext;
    use serde_json::json;

    const VALID_MD: &str = r#"---
name: research
description: Read-only investigation.
tools = ["read_file", "grep", "search_code"]
model = "deepseek-v4-flash"
gate = "fail_closed"
capabilities = ["read", "memory_read"]
max_turns = 8
---
You are a research sub-agent. Investigate read-only.
"#;

    #[test]
    fn parses_full_manifest() {
        let m = parse_manifest(VALID_MD).unwrap();
        assert_eq!(m.name, "research");
        assert_eq!(m.description, "Read-only investigation.");
        assert_eq!(m.tools, vec!["read_file", "grep", "search_code"]);
        assert_eq!(m.model.as_deref(), Some("deepseek-v4-flash"));
        assert_eq!(m.permission.gate, AgentGateMode::FailClosed);
        assert_eq!(
            m.permission.capabilities,
            vec![AgentCapability::FileRead, AgentCapability::MemoryRead]
        );
        assert_eq!(m.max_turns, 8);
        assert!(m.system_prompt.contains("research sub-agent"));
    }

    #[test]
    fn colon_syntax_and_defaults() {
        let md = r#"---
name: review
tools: [read_file, grep]
---
Review code.
"#;
        let m = parse_manifest(md).unwrap();
        assert_eq!(m.name, "review");
        assert_eq!(m.tools, vec!["read_file", "grep"]);
        assert_eq!(m.max_turns, DEFAULT_MAX_TURNS);
        assert_eq!(m.permission.gate, AgentGateMode::Inherit);
        assert!(m.permission.capabilities.is_empty());
        assert_eq!(m.model, None);
        assert_eq!(m.system_prompt, "Review code.");
    }

    #[test]
    fn max_steps_alias_works() {
        let md = r#"---
name: coder
max_steps = 5
---
Code.
"#;
        assert_eq!(parse_manifest(md).unwrap().max_turns, 5);
    }

    #[test]
    fn missing_name_errors() {
        let md = "---\ntools: [read_file]\n---\nbody";
        let err = parse_manifest(md).unwrap_err();
        assert!(matches!(err, ManifestError::MissingField("name")), "{err}");
    }

    #[test]
    fn invalid_name_errors() {
        let md = "---\nname: bad name!\n---\nbody";
        assert!(matches!(
            parse_manifest(md).unwrap_err(),
            ManifestError::InvalidName(_)
        ));
    }

    #[test]
    fn invalid_capability_errors() {
        let md = "---\nname: x\ncapabilities: [read, teleport]\n---\nbody";
        let err = parse_manifest(md).unwrap_err();
        assert!(matches!(err, ManifestError::InvalidCapability(_)), "{err}");
    }

    #[test]
    fn invalid_gate_errors() {
        let md = "---\nname: x\ngate: maybe\n---\nbody";
        assert!(matches!(
            parse_manifest(md).unwrap_err(),
            ManifestError::InvalidGate(_)
        ));
    }

    #[test]
    fn zero_max_turns_errors() {
        let md = "---\nname: x\nmax_turns = 0\n---\nbody";
        assert!(matches!(
            parse_manifest(md).unwrap_err(),
            ManifestError::InvalidMaxTurns(_)
        ));
    }

    #[test]
    fn no_frontmatter_errors() {
        let err = parse_manifest("just a prompt").unwrap_err();
        assert!(matches!(err, ManifestError::NoFrontmatter), "{err}");
    }

    #[test]
    fn unclosed_frontmatter_errors() {
        let md = "---\nname: x\n";
        assert!(matches!(
            parse_manifest(md).unwrap_err(),
            ManifestError::UnclosedFrontmatter
        ));
    }

    #[test]
    fn load_dir_scans_and_dedupes() {
        let dir = tempfile::tempdir().unwrap();
        let agents = dir.path().join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(agents.join("a.md"), "---\nname: alpha\n---\nAlpha body").unwrap();
        std::fs::write(
            agents.join("b.md"),
            "---\nname: beta\ntools: [read_file]\n---\nBeta body",
        )
        .unwrap();
        // 非 md 文件被忽略
        std::fs::write(agents.join("notes.txt"), "not a manifest").unwrap();

        let loaded = load_dir(&agents).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].name, "alpha"); // 排序稳定
        assert_eq!(loaded[1].name, "beta");
        assert_eq!(loaded[1].tools, vec!["read_file"]);
    }

    #[test]
    fn load_dir_absent_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_dir(&dir.path().join("nope")).unwrap().is_empty());
    }

    #[test]
    fn load_dir_duplicate_name_errors() {
        let dir = tempfile::tempdir().unwrap();
        let agents = dir.path().join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(agents.join("a.md"), "---\nname: dup\n---\n1").unwrap();
        std::fs::write(agents.join("b.md"), "---\nname: dup\n---\n2").unwrap();
        assert!(matches!(
            load_dir(&agents).unwrap_err(),
            ManifestError::DuplicateName(_)
        ));
    }

    #[test]
    fn load_dir_invalid_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        let agents = dir.path().join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(agents.join("bad.md"), "no frontmatter").unwrap();
        assert!(load_dir(&agents).is_err());
    }

    struct DummyTool(&'static str);
    #[async_trait::async_trait]
    impl Tool for DummyTool {
        fn schema(&self) -> deepseeknova_core::ToolSchema {
            deepseeknova_core::ToolSchema {
                name: self.0.to_string(),
                description: "dummy".into(),
                parameters: json!({}),
            }
        }
        async fn execute(
            &self,
            _ctx: &ToolContext,
            _args: &str,
        ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
            Ok("done".into())
        }
    }

    #[test]
    fn converts_to_preset_and_sub_config() {
        let m = parse_manifest(VALID_MD).unwrap();

        let preset = m.to_delegate_preset();
        assert_eq!(preset.name, "research");
        assert_eq!(preset.model.as_deref(), Some("deepseek-v4-flash"));
        assert_eq!(preset.spec.tools, vec!["read_file", "grep", "search_code"]);
        assert_eq!(preset.spec.max_steps, 8);

        let t1: Arc<dyn Tool> = Arc::new(DummyTool("read_file"));
        let t2: Arc<dyn Tool> = Arc::new(DummyTool("grep"));
        let t3: Arc<dyn Tool> = Arc::new(DummyTool("bash"));
        let tools = m.tools_from_registry(&[t1.clone(), t2.clone(), t3.clone()]);
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].schema().name, "read_file");

        let cfg = m.to_sub_agent_config(tools);
        assert_eq!(cfg.name, "research");
        assert_eq!(cfg.model.as_deref(), Some("deepseek-v4-flash"));
        assert_eq!(cfg.spec.max_steps, 8);
        assert_eq!(cfg.permission.gate, AgentGateMode::FailClosed);
    }

    #[test]
    fn capability_mapping_covers_all() {
        for (name, cap) in [
            ("read", Capability::FileRead),
            ("write", Capability::FileWrite),
            ("execute", Capability::CommandExecute),
            ("network", Capability::NetworkAccess),
            ("mcp", Capability::McpInvoke),
            ("memory_read", Capability::MemoryRead),
            ("memory_write", Capability::MemoryWrite),
        ] {
            let a = AgentCapability::parse(name).unwrap();
            assert_eq!(a.to_capability(), cap);
            assert_eq!(AgentCapability::parse(a.as_str()).unwrap(), a);
        }
    }

    #[test]
    fn gate_mode_parse_roundtrip() {
        for (text, mode) in [
            ("inherit", AgentGateMode::Inherit),
            ("none", AgentGateMode::None),
            ("bypass", AgentGateMode::None),
            ("fail_closed", AgentGateMode::FailClosed),
        ] {
            assert_eq!(AgentGateMode::parse(text), Some(mode), "{text}");
            assert_eq!(AgentGateMode::parse(mode.as_str()), Some(mode));
        }
        assert_eq!(AgentGateMode::parse("banana"), None);
    }

    #[test]
    fn test_name_validation() {
        assert!(is_valid_agent_name("coder"));
        assert!(is_valid_agent_name("sec-audit_2"));
        assert!(is_valid_agent_name("0start"));
        assert!(!is_valid_agent_name("bad name"));
        assert!(!is_valid_agent_name("-lead"));
        assert!(!is_valid_agent_name(""));
        assert!(!is_valid_agent_name("含中文"));
    }
}

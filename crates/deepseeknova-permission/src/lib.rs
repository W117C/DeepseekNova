//! # Permission — Policy-based tool execution gating
//!
//! Implements allow/ask/deny permission gates for every tool invocation.
//! Supports per-tool rules, user confirmation prompts, and session-level
//! permission caching.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented,
        clippy::dbg_macro
    )
)]

use deepseeknova_core::tool::Tool;
use deepseeknova_security::audit::{AuditLogger, SecurityEvent};
use deepseeknova_security::capability::Capability;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::sync::{atomic::Ordering, Arc};

// ---------------------------------------------------------------------------
// Permission Gate — intercept layer
// ---------------------------------------------------------------------------

/// 单次工具调用的三态权限裁决：`Allow` 放行、`Ask` 询问用户、`Deny` 拒绝。
///
/// 用于策略裁决结果（[`Policy::decide_effective`]）、会话缓存值与
/// [`CheckVerdict::decision`] 的兼容层。规则优先级 deny > ask > allow。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    /// 放行：允许工具执行（只读工具默认、显式 allow 规则命中或用户批准）。
    Allow,
    /// 询问：需要用户确认后再执行（ask 规则命中或模式预设回退）。
    Ask,
    /// 拒绝：禁止工具执行（deny 规则命中、安全硬拒或用户拒绝）。
    Deny,
}

// ---------------------------------------------------------------------------
// PermissionMode — 权限模式预设（PermissionGate 运行时状态，可切换）
// ---------------------------------------------------------------------------

/// 权限模式预设（对齐 Codex sandbox_mode / Claude Code 权限模式循环）。
///
/// 作为 [`PermissionGate`] 的运行时状态：`None`（缺省）保持旧行为（写工具
/// 回退到 [`Policy::mode`]），显式设置后按工具类别决定写工具的默认裁决。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// plan：写工具（文件编辑 + shell 写形态）默认询问；只读放行（最安全）。
    #[default]
    Plan,
    /// accept_edits：文件编辑（write/edit/move）放行，shell 写形态询问。
    AcceptEdits,
    /// auto：写工具全部放行（用户显式选择信任）。
    Auto,
}

impl PermissionMode {
    /// 当前预设下，无规则命中时写工具的默认裁决。
    pub fn fallback_for(self, tool_name: &str) -> Decision {
        match self {
            PermissionMode::Plan => Decision::Ask,
            PermissionMode::AcceptEdits => {
                if is_file_edit_tool(tool_name) {
                    Decision::Allow
                } else {
                    Decision::Ask
                }
            }
            PermissionMode::Auto => Decision::Allow,
        }
    }
}

impl From<deepseeknova_config::ModePreset> for PermissionMode {
    fn from(p: deepseeknova_config::ModePreset) -> Self {
        match p {
            deepseeknova_config::ModePreset::Plan => PermissionMode::Plan,
            deepseeknova_config::ModePreset::AcceptEdits => PermissionMode::AcceptEdits,
            deepseeknova_config::ModePreset::Auto => PermissionMode::Auto,
        }
    }
}

/// 文件编辑类工具（accept_edits 模式放行的集合）。
///
/// 覆盖 snake_case 工具名；别名（`patch`/`apply_patch` 等）一并包含，
/// 未知名称不命中（落入 Ask）。
fn is_file_edit_tool(tool_name: &str) -> bool {
    matches!(
        tool_name.to_ascii_lowercase().as_str(),
        "write"
            | "write_file"
            | "edit"
            | "edit_file"
            | "move"
            | "move_file"
            | "todo_write"
            | "patch"
            | "apply_patch"
            | "applypatch"
            | "create_file"
    )
}

// ---------------------------------------------------------------------------
// CheckVerdict — decision contract (behavior + reason + suggestions)
// ---------------------------------------------------------------------------

/// 规则建议：拒绝即教育。Ask 时附带"添加此规则即可自动放行"的建议，
/// 供上层 UI 直接展示或一键采纳；规则 Deny 不生成建议（deny 优先于 allow，
/// 该建议无效）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleSuggestion {
    /// 建议添加的规则行为（通常为 `Allow`，将 ask/deny 转为自动放行）。
    pub behavior: Decision,
    /// 建议的规则本体（tool + 可选 subject）。
    pub rule: Rule,
    /// 建议落位（如 "user" / "project" 配置层；当前为信息性字段）。
    pub destination: String,
}

impl RuleSuggestion {
    /// 构造一条规则建议。
    ///
    /// `behavior` 为建议采纳后的裁决（通常为 [`Decision::Allow`]），`rule`
    /// 为建议添加的规则本体，`destination` 为建议落位（如 `"user"` /
    /// `"project"` 配置层，当前为信息性字段）。
    pub fn new(behavior: Decision, rule: Rule, destination: impl Into<String>) -> Self {
        Self {
            behavior,
            rule,
            destination: destination.into(),
        }
    }
}

/// 一次权限检查的完整裁决：决策 + 解释。
///
/// 对应 Claude Code 的决策契约 `{behavior, reason, classifierApprovable, suggestions}`：
/// - `decision`：行为三态（allow / ask / deny）
/// - `hard`：安全硬拒标志。硬拒（危险命令、越界路径、限流）不可通过
///   添加规则覆盖，不附带 suggestions
/// - `reason`：人可读的拒绝/询问原因
/// - `suggestions`：拒绝即教育——添加后可将该操作转为自动放行的规则建议
#[derive(Debug, Clone)]
pub struct CheckVerdict {
    decision: Decision,
    hard: bool,
    reason: String,
    suggestions: Vec<RuleSuggestion>,
}

impl CheckVerdict {
    /// 构造放行裁决（无原因、无建议）。
    pub fn allow() -> Self {
        Self {
            decision: Decision::Allow,
            hard: false,
            reason: String::new(),
            suggestions: Vec::new(),
        }
    }

    /// 构造询问裁决：需要用户批准，附带人可读的询问原因。
    pub fn ask(reason: impl Into<String>) -> Self {
        Self {
            decision: Decision::Ask,
            hard: false,
            reason: reason.into(),
            suggestions: Vec::new(),
        }
    }

    /// 规则拒绝：deny 优先于 allow，不能靠添加 allow 规则覆盖；
    /// 需调整 deny 规则本身。
    pub fn deny(reason: impl Into<String>) -> Self {
        Self {
            decision: Decision::Deny,
            hard: false,
            reason: reason.into(),
            suggestions: Vec::new(),
        }
    }

    /// 安全硬拒：不可通过规则覆盖，不生成建议。
    pub fn hard_deny(reason: impl Into<String>) -> Self {
        Self {
            decision: Decision::Deny,
            hard: true,
            reason: reason.into(),
            suggestions: Vec::new(),
        }
    }

    /// 追加一条"拒绝即教育"规则建议（链式，消费 self）。
    pub fn with_suggestion(mut self, s: RuleSuggestion) -> Self {
        self.suggestions.push(s);
        self
    }

    /// 行为三态（兼容旧的 `Decision` 消费方）。
    pub fn decision(&self) -> Decision {
        self.decision
    }

    /// 是否为安全硬拒（不可通过规则覆盖）。
    pub fn is_hard_deny(&self) -> bool {
        self.hard
    }

    /// 人可读的拒绝/询问原因。
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// 附加的规则建议（采纳后可将该操作转为自动放行）。
    pub fn suggestions(&self) -> &[RuleSuggestion] {
        &self.suggestions
    }
}

// ---------------------------------------------------------------------------
// GatePreview — 决策链预览（exec 审计模式）
// ---------------------------------------------------------------------------

/// 命中的规则（预览/审计用）：来源表 + 规则本体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleHit {
    /// 规则来源表：`deny` / `ask` / `allow`（按优先级排列）。
    pub source: String,
    /// 规则模式（tool + 可选 subject）。
    pub rule: Rule,
}

/// 能力检查结果（预览用，只计算不执行）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CapabilityCheck {
    /// 生效的工作区根（`None` = 未配置，路径守卫不启用）。
    pub workspace_root: Option<String>,
    /// 越界路径（写工具 + 有工作区根时触发硬拒）。
    pub path_outside_workspace: Option<String>,
    /// 参数为畸形 JSON（写工具 + 有工作区根时 fail-closed 硬拒）。
    pub malformed_args: bool,
}

/// 一次权限检查的完整决策链预览（exec 审计模式）。
///
/// 与真实执行路径 [`PermissionGate::check`] 同源（共用 preflight + finalize
/// 决策链），保证"预览到的决策"与"实际执行时的决策"一致（一致性测试见
/// `preview_matches_check_decision`）。**只计算不执行、不改变状态**——
/// 不写会话缓存、不触发限流计数、不记审计。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatePreview {
    /// 被审计的工具名。
    pub tool_name: String,
    /// 被审计的工具参数（原始 JSON 字符串）。
    pub args: String,
    /// 最终决策：allow / ask / deny。
    pub decision: Decision,
    /// 安全硬拒标志（不可通过规则覆盖）。
    pub hard: bool,
    /// 决策原因。
    pub reason: String,
    /// 命中的规则链（deny > ask > allow，每表取首个命中；无规则命中为空）。
    pub matched_rules: Vec<RuleHit>,
    /// shell 工具的只读分类（非 shell 工具为 `None`）。
    pub readonly_kind: Option<deepseeknova_security::readonly::ReadOnlyKind>,
    /// 能力检查（工作区路径守卫等）。
    pub capability: CapabilityCheck,
    /// 拒绝即教育建议（Ask 时附带"添加 allow 规则即可自动放行"）。
    pub suggestions: Vec<RuleSuggestion>,
}

// ---------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------

/// Policy is built from config. Precedence: deny > ask > allow > fallback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    /// Fallback for writer tools when no rule matches.
    pub mode: Decision,
    /// Allow rules: matching calls are permitted (deny/ask rules still win).
    pub allow: Vec<Rule>,
    /// Ask rules: matching calls prompt the user for approval.
    pub ask: Vec<Rule>,
    /// Deny rules: matching calls are blocked; deny always wins.
    pub deny: Vec<Rule>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// 一条 allow/ask/deny 权限规则：工具名模式 + 可选的 subject 模式
/// （glob 或精确匹配，作用于调用参数）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    /// Tool name, e.g. "Bash", "read_file", or "*" for all tools.
    pub tool: String,
    /// Optional subject pattern, e.g. "rm *", "docs/**", "go test:*".
    /// Uses simple glob matching. Only applies when tool name matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// 精确匹配标志：为 true 时 subject 按字面量相等匹配，不做 glob 解释。
    /// 仅由"拒绝即教育"建议生成（避免 `rm *` 被解释成前缀规则放行
    /// `rm -rf /`）；用户配置的规则保持 glob 语义（默认 false）。
    #[serde(default, skip_serializing_if = "is_false")]
    pub exact: bool,
}

impl Rule {
    /// 构造工具级规则：匹配该工具的任何调用（无 subject 约束）。
    pub fn new(tool: impl Into<String>) -> Self {
        Self {
            tool: tool.into(),
            subject: None,
            exact: false,
        }
    }

    /// 构造带 subject 模式的规则：subject 按 glob 语义匹配调用参数。
    pub fn with_subject(tool: impl Into<String>, subject: impl Into<String>) -> Self {
        Self {
            tool: tool.into(),
            subject: Some(subject.into()),
            exact: false,
        }
    }

    /// 构造精确匹配规则：subject 只匹配字面量相等的调用参数。
    pub fn with_subject_exact(tool: impl Into<String>, subject: impl Into<String>) -> Self {
        Self {
            tool: tool.into(),
            subject: Some(subject.into()),
            exact: true,
        }
    }
}

impl Policy {
    /// Evaluate the policy for a given tool call.
    ///
    /// 旧契约：写工具回退到 `self.mode`，allow 规则恒生效。等价于
    /// `decide_effective(..., None, false)`——无模式预设、无信任降级。
    pub fn decide(&self, tool_name: &str, read_only: bool, args: &Value) -> Decision {
        self.decide_effective(tool_name, read_only, args, None, false)
    }

    /// 统一裁决链（模式预设 + 信任降级感知）。
    ///
    /// 规则优先级不变：deny > ask > allow > 回退。
    /// - `preset`（Some）：覆盖写工具默认回退（按工具类别，见
    ///   [`PermissionMode::fallback_for`]）；`None` 用 `self.mode`（旧行为）。
    /// - `degrade_allow`：为真时 allow 规则命中降级为 `Ask`——untrusted
    ///   工作区的项目层 allow 规则不得静默放行陌生项目的自配置规则。
    pub fn decide_effective(
        &self,
        tool_name: &str,
        read_only: bool,
        args: &Value,
        preset: Option<PermissionMode>,
        degrade_allow: bool,
    ) -> Decision {
        // Deny always wins
        if self.matches_any(tool_name, args, &self.deny) {
            return Decision::Deny;
        }
        // Ask overrides allow
        if self.matches_any(tool_name, args, &self.ask) {
            return Decision::Ask;
        }
        // Explicit allow
        if self.matches_any(tool_name, args, &self.allow) {
            return if degrade_allow {
                Decision::Ask
            } else {
                Decision::Allow
            };
        }
        // Fallback: reader tools are allowed, writers follow mode/preset
        if read_only {
            Decision::Allow
        } else {
            match preset {
                Some(m) => m.fallback_for(tool_name),
                None => self.mode,
            }
        }
    }

    fn matches_any(&self, tool_name: &str, args: &Value, rules: &[Rule]) -> bool {
        rules.iter().any(|r| {
            if !tool_matches(&r.tool, tool_name) {
                return false;
            }
            if let Some(ref subject) = r.subject {
                if r.exact {
                    exact_subject_matches(subject, args)
                } else {
                    subject_matches(subject, args)
                }
            } else {
                true // No subject constraint → matches any args
            }
        })
    }

    /// 在指定规则表中查找首个命中的规则（供"拒绝即教育"建议生成）。
    pub fn matching_rule<'a>(
        &'a self,
        tool_name: &str,
        args: &Value,
        rules: &'a [Rule],
    ) -> Option<&'a Rule> {
        rules.iter().find(|r| {
            tool_matches(&r.tool, tool_name)
                && match r.subject {
                    Some(ref s) => {
                        if r.exact {
                            exact_subject_matches(s, args)
                        } else {
                            subject_matches(s, args)
                        }
                    }
                    None => true,
                }
        })
    }

    /// Load a Policy from a JSON or TOML file.
    pub fn from_file(path: &Path) -> Result<Self, deepseeknova_core::DeepseeknovaError> {
        let content = std::fs::read_to_string(path)?;
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

        match ext {
            "toml" => {
                let policy: Policy = toml::from_str(&content).map_err(|e| {
                    deepseeknova_core::DeepseeknovaError::config(format!("toml parse: {e}"))
                })?;
                Ok(policy)
            }
            "json" => {
                let policy: Policy = serde_json::from_str(&content)?;
                Ok(policy)
            }
            other => Err(deepseeknova_core::DeepseeknovaError::config(format!(
                "unsupported policy format: .{other}"
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// PolicyBuilder — fluent API for building policies
// ---------------------------------------------------------------------------

/// 策略的流式构建器（fluent API）：默认模式为 `Ask`，可逐步追加规则。
pub struct PolicyBuilder {
    mode: Decision,
    allow: Vec<Rule>,
    ask: Vec<Rule>,
    deny: Vec<Rule>,
}

impl PolicyBuilder {
    /// 构造空的构建器：默认模式 `Ask`，不含任何规则。
    pub fn new() -> Self {
        Self {
            mode: Decision::Ask,
            allow: Vec::new(),
            ask: Vec::new(),
            deny: Vec::new(),
        }
    }

    /// 设置写工具无规则命中时的默认回退裁决（`mode`）。
    pub fn default_mode(mut self, mode: Decision) -> Self {
        self.mode = mode;
        self
    }

    /// 追加一条 allow 规则（放行命中的调用）。
    pub fn allow(mut self, rule: Rule) -> Self {
        self.allow.push(rule);
        self
    }

    /// 追加一条 ask 规则（命中时询问用户）。
    pub fn ask(mut self, rule: Rule) -> Self {
        self.ask.push(rule);
        self
    }

    /// 追加一条 deny 规则（命中时拒绝，优先于 allow）。
    pub fn deny(mut self, rule: Rule) -> Self {
        self.deny.push(rule);
        self
    }

    /// 安全默认：写工具询问（mode=Ask）、读工具放行。
    ///
    /// 注意：这里**不**添加 `allow("*")` 规则——那会让所有工具（含写工具）
    /// 命中 allow 列表，使 `mode=Ask` 形同虚设。读工具放行由
    /// [`Policy::decide`] 的 `read_only` fallback 保证。
    pub fn safe_defaults(mut self) -> Self {
        self.mode = Decision::Ask;
        self
    }

    /// 构建最终的 [`Policy`]（默认模式已由 [`Self::safe_defaults`] 保证为 `Ask`）。
    pub fn build(self) -> Policy {
        Policy {
            mode: self.mode,
            allow: self.allow,
            ask: self.ask,
            deny: self.deny,
        }
    }
}

impl Default for PolicyBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Permission Gate
// ---------------------------------------------------------------------------

/// PermissionGate is called by Runtime before every tool execution.
/// Supports session-level caching of user decisions.
pub struct PermissionGate {
    policy: Policy,
    /// Session-level cache: (tool_name, subject_hash) → Decision.
    /// Once a user approves/denies a specific tool+args combo, we remember
    /// for the rest of the session to avoid repeated prompts.
    session_cache: std::sync::Mutex<std::collections::HashMap<u64, Decision>>,
    /// Workspace root for path-based rules.
    workspace_root: Option<std::path::PathBuf>,
    /// Optional rate limit: max gated tool calls per rolling 60s window.
    /// Exceeding the limit denies further calls until the window drains.
    rate_limit_per_minute: Option<u32>,
    /// Timestamps of recent gated calls (rolling 60s window).
    call_times: std::sync::Mutex<std::collections::VecDeque<std::time::Instant>>,
    /// 权限模式预设（运行时状态，可切换）。`None` = 旧行为（回退
    /// [`Policy::mode`]）；`Some` = 按工具类别决定写工具默认裁决。
    mode: std::sync::Mutex<Option<PermissionMode>>,
    /// 工作区信任状态：`false`（默认，fail-closed）时项目层 allow 规则降级。
    trusted: std::sync::Mutex<bool>,
    /// allow 规则是否源自项目层（untrusted 时降级为 ask）。
    allow_project_scoped: bool,
    /// 可选的 gate 拒绝审计器（`None` = 不记录，向后兼容）。
    /// 由 runtime/cli 装配点注入；写盘失败仅 warn、不阻断判定（fail-open）。
    audit_logger: Option<Arc<dyn AuditLogger>>,
    /// gate 拒绝事件的自增序号（生成 call_id）。
    audit_seq: std::sync::atomic::AtomicU64,
}

impl PermissionGate {
    /// Create a gate for the given policy with no workspace root, no rate
    /// limit, no permission-mode preset, and no audit logger.
    pub fn new(policy: Policy) -> Self {
        Self {
            policy,
            session_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
            workspace_root: None,
            rate_limit_per_minute: None,
            call_times: std::sync::Mutex::new(std::collections::VecDeque::new()),
            mode: std::sync::Mutex::new(None),
            trusted: std::sync::Mutex::new(false),
            allow_project_scoped: false,
            audit_logger: None,
            audit_seq: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Set the workspace root for path-based permission checks.
    pub fn with_workspace_root(mut self, root: impl Into<std::path::PathBuf>) -> Self {
        self.workspace_root = Some(root.into());
        self
    }

    /// 设置初始权限模式预设（config `[permissions] mode` 注入）。
    pub fn with_mode(self, mode: Option<PermissionMode>) -> Self {
        if let Ok(mut m) = self.mode.lock() {
            *m = mode;
        }
        self
    }

    /// 标记 allow 规则是否源自项目层（trust 降级开关）。
    pub fn with_allow_project_scoped(mut self, scoped: bool) -> Self {
        self.allow_project_scoped = scoped;
        self
    }

    /// 设置初始工作区信任状态（`TrustStore` 解析结果注入）。
    pub fn with_trusted(self, trusted: bool) -> Self {
        if let Ok(mut t) = self.trusted.lock() {
            *t = trusted;
        }
        self
    }

    /// 切换权限模式预设（TUI Ctrl+P / `/mode` 运行时调用）。
    pub fn set_mode(&self, mode: Option<PermissionMode>) {
        if let Ok(mut m) = self.mode.lock() {
            *m = mode;
        }
    }

    /// 当前权限模式预设（`None` = 旧行为）。
    pub fn mode(&self) -> Option<PermissionMode> {
        self.mode.lock().map(|m| *m).unwrap_or(None)
    }

    /// 切换工作区信任状态（TUI 信任确认后调用）。
    pub fn set_trusted(&self, trusted: bool) {
        if let Ok(mut t) = self.trusted.lock() {
            *t = trusted;
        }
    }

    /// 当前工作区信任状态。
    pub fn trusted(&self) -> bool {
        self.trusted.lock().map(|t| *t).unwrap_or(false)
    }

    /// allow 规则是否源自项目层。
    pub fn allow_project_scoped(&self) -> bool {
        self.allow_project_scoped
    }

    /// Enable rate limiting: at most `limit` gated tool calls per rolling minute.
    pub fn with_rate_limit(mut self, limit: u32) -> Self {
        self.rate_limit_per_minute = Some(limit.max(1));
        self
    }

    /// 注入审计日志器：gate 拒绝（越界路径/危险命令/deny 规则/限流/缓存
    /// 拒绝）写入安全审计（JSONL），消除对抗性场景下拒绝取证的盲区
    /// （AUDIT M1）。缺省 `None` 时不记录，完全向后兼容。审计持久化
    /// fail-open：写盘失败仅 warn、绝不改变安全判定。
    pub fn with_audit_logger(mut self, logger: Arc<dyn AuditLogger>) -> Self {
        self.audit_logger = Some(logger);
        self
    }

    /// Record the current call and return true when the rolling-minute
    /// window already holds `limit` calls (i.e. this call must be denied).
    fn rate_limited(&self) -> bool {
        let Some(limit) = self.rate_limit_per_minute else {
            return false;
        };
        let now = std::time::Instant::now();
        let Ok(mut times) = self.call_times.lock() else {
            return false;
        };
        while times
            .front()
            .is_some_and(|t| now.duration_since(*t).as_secs() >= 60)
        {
            times.pop_front();
        }
        if times.len() >= limit as usize {
            return true;
        }
        times.push_back(now);
        false
    }

    /// Check whether a tool call should be allowed.
    /// Uses session cache to avoid repeated prompts for the same operation.
    ///
    /// 返回完整裁决（行为 + 原因 + 规则建议）；硬拒（限流/越界路径/危险命令）
    /// 不可通过规则覆盖。旧调用方可经 [`CheckVerdict::decision`] 取三态。
    ///
    /// 审计：全部拒绝分支（限流/越界/危险/畸形参数/缓存拒绝/deny 规则）
    /// 在注入审计器时记录 `gate_deny` 事件（AUDIT M1），供对抗性取证；
    /// 写盘失败仅 warn、不阻断判定（fail-open）。`preview` 不记审计。
    pub fn check(&self, tool: &dyn Tool, args: &str) -> CheckVerdict {
        // Rate limit first: a hard cap independent of per-tool decisions.
        // （预览 API 不触发限流计数——见 [`PermissionGate::preview`]。）
        if self.rate_limited() {
            let v = CheckVerdict::hard_deny("rate limit exceeded");
            self.audit_denial(tool, args, None, v.reason());
            return v;
        }

        let tool_name = &tool.schema().name;
        let read_only = tool.read_only();

        // 预检：参数解析 + 路径守卫 + 只读分类（与 preview 同源）。
        let preflight = self.preflight(tool_name, read_only, args);
        if let Some(v) = preflight.hard_deny {
            // 越界路径/危险命令/畸形参数硬拒：附带越界路径取证字段。
            let path = preflight.capability.path_outside_workspace.clone();
            self.audit_denial(tool, args, path, v.reason());
            return v;
        }

        // Check session cache (user decisions take precedence over readonly auto-allow)
        let cache_key = compute_cache_key(tool_name, args);
        if let Ok(cache) = self.session_cache.lock() {
            if let Some(cached) = cache.get(&cache_key) {
                return match *cached {
                    Decision::Allow => CheckVerdict::allow(),
                    Decision::Ask => CheckVerdict::ask("cached: requires approval"),
                    Decision::Deny => {
                        let v = CheckVerdict::deny("cached: denied by user");
                        self.audit_denial(tool, args, None, v.reason());
                        v
                    }
                };
            }
        }

        // 策略裁决（与 preview 同源）。
        let outcome = self.finalize(tool_name, read_only, &preflight);
        let v = CheckVerdict {
            decision: outcome.decision,
            hard: outcome.hard,
            reason: outcome.reason,
            suggestions: outcome.suggestions,
        };
        if v.decision() == Decision::Deny {
            // deny 规则（或 Deny 回退）：记录拒绝取证。
            self.audit_denial(tool, args, None, v.reason());
        }
        v
    }

    /// 在 gate 拒绝分支记录审计事件（AUDIT M1）。
    ///
    /// fail-open：无审计器或写盘失败都只是跳过/告警，绝不改变拒绝判定。
    /// `path` 为越界路径等取证字段；能力类别由工具名 + 只读标志推断
    /// （审计事件 schema 需要 capability 字段）。审计 reason 附加**原始
    /// 调用参数**——对抗性取证需要看到被拒的命令/路径原文（如危险命令
    /// 具体是什么），而非仅通用原因。
    fn audit_denial(&self, tool: &dyn Tool, args: &str, path: Option<String>, reason: &str) {
        let Some(logger) = &self.audit_logger else {
            return;
        };
        let tool_name = &tool.schema().name;
        let event = SecurityEvent {
            event_type: "gate_deny".to_string(),
            call_id: format!("gate-{}", self.audit_seq.fetch_add(1, Ordering::Relaxed)),
            tool_name: tool_name.clone(),
            capability: Some(gate_capability(tool_name, tool.read_only())),
            path,
            allowed: false,
            reason: if args.is_empty() {
                reason.to_string()
            } else {
                format!("{reason} | args: {args}")
            },
        };
        logger.record(&event);
    }

    /// 预执行决策预览（exec 审计模式）。
    ///
    /// 给定工具名 + 参数（+ 可选工作区根），返回完整决策链：命中规则
    /// （id/模式/来源）、最终 Allow/Ask/Deny、只读分类、能力检查结果、
    /// 建议。**只计算不执行、不改变状态**——不写会话缓存、不触发限流
    /// 计数、不记审计。
    ///
    /// 与 [`PermissionGate::check`] 共用 `preflight` + `finalize` 决策链，
    /// 保证预览与真实执行决策一致（一致性测试见 `preview_matches_check_decision`）。
    /// 无 `&dyn Tool` 依赖：由调用方提供工具名与只读标志，便于 CLI/库在
    /// 不实例化工具的情况下审计任意工具调用。
    pub fn preview(&self, tool_name: &str, read_only: bool, args: &str) -> GatePreview {
        let preflight = self.preflight(tool_name, read_only, args);
        let (decision, hard, reason, suggestions, matched_rules) = match &preflight.hard_deny {
            Some(v) => (
                v.decision(),
                v.is_hard_deny(),
                v.reason().to_string(),
                v.suggestions().to_vec(),
                Vec::new(),
            ),
            None => {
                let o = self.finalize(tool_name, read_only, &preflight);
                (o.decision, o.hard, o.reason, o.suggestions, o.matched_rules)
            }
        };
        GatePreview {
            tool_name: tool_name.to_string(),
            args: args.to_string(),
            decision,
            hard,
            reason,
            matched_rules,
            readonly_kind: preflight.readonly_kind,
            capability: preflight.capability,
            suggestions,
        }
    }

    /// 预检：参数解析 + 工作区路径守卫 + shell 只读分类。
    ///
    /// 真实执行路径 [`PermissionGate::check`] 与预览 [`PermissionGate::preview`]
    /// 共用，保证两端决策同源。预检阶段硬拒（畸形参数/越界路径/危险命令）
    /// 记录在 `hard_deny`，由调用方决定处理。**不写缓存、不触发限流**。
    fn preflight(&self, tool_name: &str, read_only: bool, args: &str) -> Preflight {
        let args_value: Value = match serde_json::from_str(args) {
            Ok(v) => v,
            Err(_) => {
                // 参数无法解析（畸形 JSON）：写工具 + 有工作区根时无法验证
                // 路径，fail-closed 硬拒，避免畸形输入静默跳过工作区守卫。
                // （Windows 下未转义反斜杠路径即会命中此分支。）
                if !read_only && self.workspace_root.is_some() {
                    return Preflight {
                        args_value: Value::Null,
                        readonly_kind: None,
                        readonly_cmd: false,
                        capability: CapabilityCheck {
                            workspace_root: self
                                .workspace_root
                                .as_ref()
                                .map(|p| p.display().to_string()),
                            malformed_args: true,
                            ..Default::default()
                        },
                        hard_deny: Some(CheckVerdict::hard_deny(
                            "malformed tool arguments: cannot verify path",
                        )),
                    };
                }
                Value::Null
            }
        };

        let mut capability = CapabilityCheck {
            workspace_root: self
                .workspace_root
                .as_ref()
                .map(|p| p.display().to_string()),
            ..Default::default()
        };

        // Path-based guard: deny writes outside workspace (hard deny).
        // 覆盖单路径工具（path/file/target/directory）与双路径工具
        // （move_file 的 source+destination）——任一路径越界都必须硬拒。
        if !read_only {
            if let Some(ref root) = self.workspace_root {
                for path in extract_paths(&args_value) {
                    if !is_within_workspace(root, &path) {
                        capability.path_outside_workspace = Some(path.clone());
                        return Preflight {
                            args_value,
                            readonly_kind: None,
                            readonly_cmd: false,
                            capability,
                            hard_deny: Some(CheckVerdict::hard_deny(format!(
                                "path outside workspace: {path}"
                            ))),
                        };
                    }
                }
            }
        }

        // Shell 命令只读分类：
        // - Dangerous（工具级注入面：git -c/--config-env、%G/%x 格式串、
        //   UNC/URL/SMB 路径形态）→ 安全硬拒，不可通过规则覆盖
        // - ReadOnly → 仅当无 deny 规则/缓存拒绝命中时才免询问放行
        //   （H1 修复：deny 优先于只读免询问——"Deny always wins"契约）
        // - NotReadOnly（含链式/重定向/命令替换等普通 shell 组合）
        //   → 走规则/审批流程，可由用户 allow 规则覆盖
        let mut readonly_kind = None;
        let mut readonly_cmd = false;
        if is_shell_tool(tool_name) {
            if let Some(cmd) = extract_command(&args_value) {
                use deepseeknova_security::readonly::{classify_readonly, ReadOnlyKind};
                match classify_readonly(&cmd) {
                    ReadOnlyKind::Dangerous => {
                        readonly_kind = Some(ReadOnlyKind::Dangerous);
                        return Preflight {
                            args_value,
                            readonly_kind,
                            readonly_cmd: false,
                            capability,
                            hard_deny: Some(CheckVerdict::hard_deny("dangerous command detected")),
                        };
                    }
                    ReadOnlyKind::ReadOnly => {
                        readonly_cmd = true;
                        readonly_kind = Some(ReadOnlyKind::ReadOnly);
                    }
                    ReadOnlyKind::NotReadOnly => {
                        readonly_kind = Some(ReadOnlyKind::NotReadOnly);
                    }
                }
            }
        }

        Preflight {
            args_value,
            readonly_kind,
            readonly_cmd,
            capability,
            hard_deny: None,
        }
    }

    /// 策略裁决（preflight 之后）。与 [`PermissionGate::check`] 的 Ask/Deny
    /// 分支同源；额外返回命中规则链供预览展示。
    fn finalize(&self, tool_name: &str, read_only: bool, preflight: &Preflight) -> CoreOutcome {
        // Evaluate policy; attach "拒绝即教育" suggestions on ask/deny.
        // - 模式预设（self.mode）覆盖写工具默认回退；
        // - untrusted + 项目层 allow 规则 → 降级为 Ask（不能静默放行陌生
        //   项目的自配置规则），此时不附加"添加规则即可放行"建议（规则已存在，
        //   正确做法是信任项目而非新增规则）。
        let degrade_allow = !self.trusted() && self.allow_project_scoped;
        let allow_hit = degrade_allow
            && self
                .policy
                .matching_rule(tool_name, &preflight.args_value, &self.policy.allow)
                .is_some();
        let matched_rules = self.matching_rules_chain(tool_name, &preflight.args_value);
        match self.policy.decide_effective(
            tool_name,
            read_only,
            &preflight.args_value,
            self.mode(),
            degrade_allow,
        ) {
            Decision::Allow => CoreOutcome {
                decision: Decision::Allow,
                hard: false,
                reason: String::new(),
                suggestions: Vec::new(),
                matched_rules,
            },
            Decision::Ask => {
                // 区分"显式 ask 规则命中"与"mode 回退 Ask"：
                // - 显式规则命中 → 只读命令也不得短路（用户明确要求确认，
                //   F3 修复：与 deny 同优先级语义，方向对称）
                // - mode 回退（无规则命中）→ 只读命令免询问放行
                let explicit_ask = self
                    .policy
                    .matching_rule(tool_name, &preflight.args_value, &self.policy.ask)
                    .is_some();
                if preflight.readonly_cmd && !explicit_ask {
                    CoreOutcome {
                        decision: Decision::Allow,
                        hard: false,
                        reason: String::new(),
                        suggestions: Vec::new(),
                        matched_rules,
                    }
                } else {
                    let mut v = CheckVerdict::ask("requires user approval");
                    // 仅当 Ask 来自被降级的 allow 规则时抑制建议。
                    if !(allow_hit && !explicit_ask) {
                        for s in suggest_allow(tool_name, &preflight.args_value) {
                            v = v.with_suggestion(s);
                        }
                    }
                    CoreOutcome {
                        decision: v.decision(),
                        hard: v.is_hard_deny(),
                        reason: v.reason().to_string(),
                        suggestions: v.suggestions().to_vec(),
                        matched_rules,
                    }
                }
            }
            Decision::Deny => {
                // 若 deny 由规则命中，reason 指名规则。deny 优先于 allow，
                // 此时不附加"添加 allow 规则即可放行"建议（该建议无效，
                // 只会误导用户）。
                let (hard, reason) = match self.policy.matching_rule(
                    tool_name,
                    &preflight.args_value,
                    &self.policy.deny,
                ) {
                    Some(r) => (
                        false,
                        format!(
                            "blocked by deny rule: tool={} subject={:?}",
                            r.tool, r.subject
                        ),
                    ),
                    None => (false, "blocked by deny rule".to_string()),
                };
                CoreOutcome {
                    decision: Decision::Deny,
                    hard,
                    reason,
                    suggestions: Vec::new(),
                    matched_rules,
                }
            }
        }
    }

    /// 全部命中的规则（deny > ask > allow 优先级顺序，每表取首个命中）。
    /// 供 exec 审计预览展示完整决策链。
    fn matching_rules_chain(&self, tool_name: &str, args: &Value) -> Vec<RuleHit> {
        let mut out = Vec::new();
        for (rules, source) in [
            (self.policy.deny.as_slice(), "deny"),
            (self.policy.ask.as_slice(), "ask"),
            (self.policy.allow.as_slice(), "allow"),
        ] {
            if let Some(r) = self.policy.matching_rule(tool_name, args, rules) {
                out.push(RuleHit {
                    source: source.to_string(),
                    rule: r.clone(),
                });
            }
        }
        out
    }

    /// Cache a user's decision for this session (called after user responds to Ask).
    pub fn cache_decision(&self, tool_name: &str, args: &str, decision: Decision) {
        let key = compute_cache_key(tool_name, args);
        if let Ok(mut cache) = self.session_cache.lock() {
            cache.insert(key, decision);
        }
    }

    /// shell 工具的只读分类查询（additive API）。
    ///
    /// 返回 `ReadOnlyKind`（只读 / 非只读 / 危险），供 agent Ask 审批路径
    /// 生成风险标签；非 shell 工具或参数无法解析命令时返回 `None`。
    /// 与 [`PermissionGate::check`] 内部使用的分类器同源，不重复判定逻辑。
    pub fn shell_readonly_kind(
        &self,
        tool_name: &str,
        args: &str,
    ) -> Option<deepseeknova_security::readonly::ReadOnlyKind> {
        use deepseeknova_security::readonly::classify_readonly;
        if !is_shell_tool(tool_name) {
            return None;
        }
        let args_value: Value = serde_json::from_str(args).ok()?;
        let cmd = extract_command(&args_value)?;
        Some(classify_readonly(&cmd))
    }

    /// Clear the session cache (e.g., on new session).
    pub fn clear_cache(&self) {
        if let Ok(mut cache) = self.session_cache.lock() {
            cache.clear();
        }
    }

    /// 当前策略中的 deny 规则（冻结用）。
    ///
    /// 子代理/委派 agent 共享本 gate 执行工具调用，deny 在执行层已被
    /// 结构性冻结；本访问器用于把 deny 清单注入子代理 system prompt，
    /// 让子代理模型在**发起**调用前就知道边界（减少无效尝试）。
    pub fn deny_rules(&self) -> &[Rule] {
        &self.policy.deny
    }
}

// ---------------------------------------------------------------------------
// 决策链中间结构（check 与 preview 共用）
// ---------------------------------------------------------------------------

/// 预检结果：参数解析 + 路径守卫 + 只读分类（不含限流/缓存/策略）。
///
/// `hard_deny` 为 `Some` 时表示预检阶段即硬拒（畸形参数/越界路径/危险
/// 命令），调用方应直接返回该裁决，不再进入策略。
struct Preflight {
    args_value: Value,
    readonly_kind: Option<deepseeknova_security::readonly::ReadOnlyKind>,
    /// shell 命令被分类为只读（ReadOnly，且无 deny/ask 规则/缓存拒绝时免询问）。
    readonly_cmd: bool,
    capability: CapabilityCheck,
    hard_deny: Option<CheckVerdict>,
}

/// 策略裁决结果（含命中规则链，供预览展示）。
struct CoreOutcome {
    decision: Decision,
    hard: bool,
    reason: String,
    suggestions: Vec<RuleSuggestion>,
    matched_rules: Vec<RuleHit>,
}

// ---------------------------------------------------------------------------
// Path-based helpers
// ---------------------------------------------------------------------------

/// 判断工具名是否为 shell 类工具（危险命令检测只作用于该类）。
fn is_shell_tool(tool_name: &str) -> bool {
    matches!(tool_name, "Bash" | "bash" | "shell")
}

/// 为 gate 拒绝事件推断最贴切的能力类别（审计事件需要 capability 字段）。
///
/// 尽力而为的启发式：shell 工具 → CommandExecute；写工具 → FileWrite；
/// 读工具 → FileRead。网络/记忆等细粒度类别 gate 层无法精确区分，
/// 取证时以 `tool_name` + `reason` 定位。
fn gate_capability(tool_name: &str, read_only: bool) -> Capability {
    if is_shell_tool(tool_name) {
        Capability::CommandExecute
    } else if read_only {
        Capability::FileRead
    } else {
        Capability::FileWrite
    }
}

/// 路径字段名（含 move_file 双路径 source/destination）。
const PATH_KEYS: &[&str] = &[
    "path",
    "file",
    "file_path",
    "target",
    "directory",
    "source",
    "destination",
];

/// 递归收集工具参数中的全部路径字段。
///
/// 覆盖单路径工具（`path`/`file`/`target`/`directory`）与双路径工具
/// （`move_file` 的 `source`+`destination`）——任一路径越界都必须触发守卫。
/// 递归进入嵌套对象与数组，防御未来新增的嵌套参数结构。
fn extract_paths(args: &Value) -> Vec<String> {
    let mut out = Vec::new();
    collect_path_fields(args, &mut out);
    out
}

fn collect_path_fields(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(map) => {
            for (key, val) in map {
                if PATH_KEYS.contains(&key.as_str()) {
                    if let Some(s) = val.as_str() {
                        out.push(s.to_string());
                    }
                }
                collect_path_fields(val, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_path_fields(item, out);
            }
        }
        _ => {}
    }
}

/// 提取可用作规则 subject 的参数字段（与 `subject_matches` 的字段集一致）。
fn extract_subject(args: &Value) -> Option<String> {
    if let Value::Object(map) = args {
        for key in &["command", "path", "file", "pattern", "query", "name"] {
            if let Some(val) = map.get(*key) {
                if let Some(s) = val.as_str() {
                    return Some(s.to_string());
                }
            }
        }
    }
    None
}

/// Check if a path is within the workspace root.
///
/// 相对路径按工作区根解释（与工具实际行为对齐：工具在 workspace root 下
/// 解析相对路径）。先 canonicalize 解析 symlink 与 `..`；目标尚不存在
/// （新建文件）时对最近存在的父目录 canonicalize；全部失败则词法兜底。
/// 判断路径是否在工作区内。
///
/// C7 修复：路径安全判定委托给 `deepseeknova_security::path::secure_resolve`
///（单一事实源），消除 permission 与 security 两套并行实现导致的分歧风险。
/// `secure_resolve` 内部已覆盖词法折叠 + canonicalize + symlink 逃逸检查，
/// 且经 security crate 的回归测试验证（含 `..` 逃逸、symlink 逃逸等场景）。
fn is_within_workspace(root: &std::path::Path, path: &str) -> bool {
    deepseeknova_security::path::secure_resolve(root, std::path::Path::new(path)).is_ok()
}

/// Extract a command string from shell tool arguments.
fn extract_command(args: &Value) -> Option<String> {
    if let Value::Object(map) = args {
        if let Some(cmd) = map.get("command") {
            return cmd.as_str().map(|s| s.to_string());
        }
    }
    if let Value::String(s) = args {
        return Some(s.clone());
    }
    None
}

/// 生成"拒绝即教育"建议：把当前待确认的操作改写为一条 allow 规则。
/// subject 取自调用参数（command/path/...），无参数时退化为工具级规则。
///
/// 仅用于 Ask 路径：deny 规则优先于 allow 规则，规则拒绝时添加 allow
/// 规则无法放行，因此不在 Deny 分支生成建议。
fn suggest_allow(tool_name: &str, args: &Value) -> Vec<RuleSuggestion> {
    let rule = match extract_subject(args) {
        // 建议规则使用精确匹配：subject 原文可能含 `*`（如 `rm *.tmp`），
        // glob 前缀解释会把 `rm *` 放大成放行 `rm -rf /`。
        Some(s) => Rule::with_subject_exact(tool_name, s),
        None => Rule::new(tool_name),
    };
    vec![RuleSuggestion::new(Decision::Allow, rule, "user")]
}

/// Compute a cache key for session-level permission caching.
fn compute_cache_key(tool_name: &str, args: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    tool_name.hash(&mut hasher);
    args.hash(&mut hasher);
    hasher.finish()
}

// ---------------------------------------------------------------------------
// Matching helpers
// ---------------------------------------------------------------------------

/// Check if a tool name matches a rule pattern.
/// Supports exact match and wildcard ("*" matches all).
fn tool_matches(pattern: &str, tool_name: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if pattern == tool_name {
        return true;
    }
    false
}

/// Check if tool arguments match a subject pattern.
/// The subject is matched against the string representation of the tool args,
/// with simple glob support.
fn subject_matches(pattern: &str, args: &Value) -> bool {
    let args_str = match args {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    };

    // Try to extract meaningful fields from JSON args for better matching
    // E.g., for ShellTool: {"command": "rm -rf /"} → match against "command"
    if let Value::Object(ref map) = args {
        // Check common fields: command, path, file, pattern, query
        for key in &["command", "path", "file", "pattern", "query", "name"] {
            if let Some(val) = map.get(*key) {
                if let Some(s) = val.as_str() {
                    return simple_glob_match(pattern, s);
                }
            }
        }
    }

    // Fall back to matching against the full args string
    simple_glob_match(pattern, &args_str)
}

/// Check if tool arguments equal the subject literally (exact-match rules).
/// 与 `subject_matches` 使用同一字段提取逻辑，但只做字面量相等比较，
/// 不做 glob 解释——用于"拒绝即教育"生成的建议规则。
fn exact_subject_matches(subject: &str, args: &Value) -> bool {
    let args_str = match args {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    };

    if let Value::Object(ref map) = args {
        for key in &["command", "path", "file", "pattern", "query", "name"] {
            if let Some(val) = map.get(*key) {
                if let Some(s) = val.as_str() {
                    return subject == s;
                }
            }
        }
    }

    subject == args_str
}

/// Simple glob matching for permission subjects.
fn simple_glob_match(pattern: &str, name: &str) -> bool {
    let pattern = pattern.trim();

    // Exact match
    if pattern == name {
        return true;
    }

    // Star-star: ** matches any path
    if pattern.contains("**") {
        let prefix = pattern.strip_suffix("**").unwrap_or("");
        let suffix = pattern.strip_prefix("**").unwrap_or("");
        if !prefix.is_empty() && name.starts_with(prefix) {
            return true;
        }
        if !suffix.is_empty() && name.ends_with(suffix) {
            return true;
        }
        if prefix.is_empty() && suffix.is_empty() {
            return true; // "**" matches everything
        }
    }

    // Suffix match: *.ext
    if let Some(ext) = pattern.strip_prefix("*.") {
        return name.ends_with(ext);
    }

    // Prefix match: dir/*
    if let Some(prefix) = pattern.strip_suffix("/*") {
        if let Some(remainder) = name.strip_prefix(prefix) {
            // Don't match "dir" itself, only "dir/..."
            return remainder == "/" || remainder.starts_with("/");
        }
        return false;
    }

    // Contains: *word*
    if pattern.starts_with('*') && pattern.ends_with('*') && pattern.len() > 1 {
        let inner = &pattern[1..pattern.len() - 1];
        return name.contains(inner);
    }

    // Prefix: word*
    if let Some(prefix) = pattern.strip_suffix('*') {
        return name.starts_with(prefix);
    }

    // Suffix: *word
    if let Some(suffix) = pattern.strip_prefix('*') {
        return name.ends_with(suffix);
    }

    false
}

// ---------------------------------------------------------------------------
// PermissionError (for future use)
// ---------------------------------------------------------------------------

/// 权限相关错误：工具调用被拒绝 / 需用户批准 / 策略无效 / 底层 IO 失败。
///
/// 可经 [`From<PermissionError>`] 转换为 [`deepseeknova_core::DeepseeknovaError`]，
/// 供上层用 `?` 直接传播。
#[derive(Debug, thiserror::Error)]
pub enum PermissionError {
    /// 工具调用被 deny 规则拒绝。
    #[error("tool '{tool}' denied: {reason}")]
    Denied {
        /// 被拒绝的工具名。
        tool: String,
        /// 拒绝原因。
        reason: String,
    },

    /// 工具调用需要用户批准。
    #[error("tool '{tool}' requires user approval")]
    RequiresApproval {
        /// 待批准的工具名。
        tool: String,
    },

    /// 策略内容无效（如 TOML/JSON 解析失败）。
    #[error("invalid policy: {0}")]
    InvalidPolicy(String),

    /// 底层 IO 错误。
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// 把 [`PermissionError`] 转换为 [`deepseeknova_core::DeepseeknovaError`]。
///
/// 本 impl 利用 orphan rule 放在拥有 `PermissionError` 的本 crate 中
/// （`DeepseeknovaError` 来自 `deepseeknova-core`，`From` 来自 std）。这使 `?`
/// 运算符能把 `Result<_, PermissionError>` 直接用于返回
/// `Result<_, DeepseeknovaError>` 的函数，无需显式 `.map_err`。
///
/// 当前映射保留人可读消息（`to_string()`），丢失变体级别的类型信息（如
/// `Denied` vs `RequiresApproval` 的区分）；`DeepseeknovaError::Permission`
/// 变体的 `is_retryable()` 返回 `false`，与权限错误的确定性语义一致。
impl From<PermissionError> for deepseeknova_core::DeepseeknovaError {
    fn from(err: PermissionError) -> Self {
        deepseeknova_core::DeepseeknovaError::Permission {
            message: err.to_string(),
            source: Some(Box::new(err)),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests — 已随 P2-D 拆分迁至同目录 tests.rs（纯行范围搬移，无内容变更），
// 本文件仅保留模块声明。
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;

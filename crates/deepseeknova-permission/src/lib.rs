//! # Permission — Policy-based tool execution gating
//!
//! Implements allow/ask/deny permission gates for every tool invocation.
//! Supports per-tool rules, user confirmation prompts, and session-level
//! permission caching.

use deepseeknova_core::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

// ---------------------------------------------------------------------------
// Permission Gate — intercept layer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Allow,
    Ask,
    Deny,
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
    pub fn allow() -> Self {
        Self {
            decision: Decision::Allow,
            hard: false,
            reason: String::new(),
            suggestions: Vec::new(),
        }
    }

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

    pub fn reason(&self) -> &str {
        &self.reason
    }

    pub fn suggestions(&self) -> &[RuleSuggestion] {
        &self.suggestions
    }
}

// ---------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------

/// Policy is built from config. Precedence: deny > ask > allow > fallback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    /// Fallback for writer tools when no rule matches.
    pub mode: Decision,
    pub allow: Vec<Rule>,
    pub ask: Vec<Rule>,
    pub deny: Vec<Rule>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

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
    pub fn new(tool: impl Into<String>) -> Self {
        Self {
            tool: tool.into(),
            subject: None,
            exact: false,
        }
    }

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
    pub fn decide(&self, tool_name: &str, read_only: bool, args: &Value) -> Decision {
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
            return Decision::Allow;
        }
        // Fallback: reader tools are allowed, writers follow mode
        if read_only {
            Decision::Allow
        } else {
            self.mode
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
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

        match ext {
            "toml" => {
                let policy: Policy = toml::from_str(&content)?;
                Ok(policy)
            }
            "json" => {
                let policy: Policy = serde_json::from_str(&content)?;
                Ok(policy)
            }
            other => anyhow::bail!("unsupported policy format: .{other}"),
        }
    }
}

// ---------------------------------------------------------------------------
// PolicyBuilder — fluent API for building policies
// ---------------------------------------------------------------------------

pub struct PolicyBuilder {
    mode: Decision,
    allow: Vec<Rule>,
    ask: Vec<Rule>,
    deny: Vec<Rule>,
}

impl PolicyBuilder {
    pub fn new() -> Self {
        Self {
            mode: Decision::Ask,
            allow: Vec::new(),
            ask: Vec::new(),
            deny: Vec::new(),
        }
    }

    pub fn default_mode(mut self, mode: Decision) -> Self {
        self.mode = mode;
        self
    }

    pub fn allow(mut self, rule: Rule) -> Self {
        self.allow.push(rule);
        self
    }

    pub fn ask(mut self, rule: Rule) -> Self {
        self.ask.push(rule);
        self
    }

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
}

impl PermissionGate {
    pub fn new(policy: Policy) -> Self {
        Self {
            policy,
            session_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
            workspace_root: None,
            rate_limit_per_minute: None,
            call_times: std::sync::Mutex::new(std::collections::VecDeque::new()),
        }
    }

    /// Set the workspace root for path-based permission checks.
    pub fn with_workspace_root(mut self, root: impl Into<std::path::PathBuf>) -> Self {
        self.workspace_root = Some(root.into());
        self
    }

    /// Enable rate limiting: at most `limit` gated tool calls per rolling minute.
    pub fn with_rate_limit(mut self, limit: u32) -> Self {
        self.rate_limit_per_minute = Some(limit.max(1));
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
    pub fn check(&self, tool: &dyn Tool, args: &str) -> CheckVerdict {
        // Rate limit first: a hard cap independent of per-tool decisions.
        if self.rate_limited() {
            return CheckVerdict::hard_deny("rate limit exceeded");
        }

        let args_value: Value = match serde_json::from_str(args) {
            Ok(v) => v,
            Err(_) => {
                // 参数无法解析（畸形 JSON）：写工具 + 有工作区根时无法验证
                // 路径，fail-closed 硬拒，避免畸形输入静默跳过工作区守卫。
                // （Windows 下未转义反斜杠路径即会命中此分支。）
                if !tool.read_only() && self.workspace_root.is_some() {
                    return CheckVerdict::hard_deny("malformed tool arguments: cannot verify path");
                }
                Value::Null
            }
        };
        let tool_name = &tool.schema().name;

        // Path-based guard: deny writes outside workspace (hard deny).
        // 覆盖单路径工具（path/file/target/directory）与双路径工具
        // （move_file 的 source+destination）——任一路径越界都必须硬拒。
        if !tool.read_only() {
            if let Some(ref root) = self.workspace_root {
                for path in extract_paths(&args_value) {
                    if !is_within_workspace(root, &path) {
                        return CheckVerdict::hard_deny(format!("path outside workspace: {path}"));
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
        let mut readonly_cmd = false;
        if is_shell_tool(tool_name) {
            if let Some(cmd) = extract_command(&args_value) {
                use deepseeknova_security::readonly::{classify_readonly, ReadOnlyKind};
                match classify_readonly(&cmd) {
                    ReadOnlyKind::Dangerous => {
                        return CheckVerdict::hard_deny("dangerous command detected");
                    }
                    ReadOnlyKind::ReadOnly => readonly_cmd = true,
                    ReadOnlyKind::NotReadOnly => {}
                }
            }
        }

        // Check session cache (user decisions take precedence over readonly auto-allow)
        let cache_key = compute_cache_key(tool_name, args);
        if let Ok(cache) = self.session_cache.lock() {
            if let Some(cached) = cache.get(&cache_key) {
                return match *cached {
                    Decision::Allow => CheckVerdict::allow(),
                    Decision::Ask => CheckVerdict::ask("cached: requires approval"),
                    Decision::Deny => CheckVerdict::deny("cached: denied by user"),
                };
            }
        }

        // Evaluate policy; attach "拒绝即教育" suggestions on ask/deny.
        match self.policy.decide(tool_name, tool.read_only(), &args_value) {
            Decision::Allow => CheckVerdict::allow(),
            Decision::Ask => {
                // 区分"显式 ask 规则命中"与"mode 回退 Ask"：
                // - 显式规则命中 → 只读命令也不得短路（用户明确要求确认，
                //   F3 修复：与 deny 同优先级语义，方向对称）
                // - mode 回退（无规则命中）→ 只读命令免询问放行
                if readonly_cmd
                    && self
                        .policy
                        .matching_rule(tool_name, &args_value, &self.policy.ask)
                        .is_none()
                {
                    CheckVerdict::allow()
                } else {
                    let mut v = CheckVerdict::ask("requires user approval");
                    for s in suggest_allow(tool_name, &args_value) {
                        v = v.with_suggestion(s);
                    }
                    v
                }
            }
            Decision::Deny => {
                // 若 deny 由规则命中，reason 指名规则。deny 优先于 allow，
                // 此时不附加"添加 allow 规则即可放行"建议（该建议无效，
                // 只会误导用户）。
                match self
                    .policy
                    .matching_rule(tool_name, &args_value, &self.policy.deny)
                {
                    Some(r) => CheckVerdict::deny(format!(
                        "blocked by deny rule: tool={} subject={:?}",
                        r.tool, r.subject
                    )),
                    None => CheckVerdict::deny("blocked by deny rule"),
                }
            }
        }
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
// Path-based helpers
// ---------------------------------------------------------------------------

/// 判断工具名是否为 shell 类工具（危险命令检测只作用于该类）。
fn is_shell_tool(tool_name: &str) -> bool {
    matches!(tool_name, "Bash" | "bash" | "shell")
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
fn is_within_workspace(root: &std::path::Path, path: &str) -> bool {
    let target = std::path::Path::new(path);
    // root 词法规范化（不解析 symlink），与 target 用同一坐标系比较：
    // 若 root 预先 canonicalize，而 target 是原始形式（如 macOS 的
    // `/var` → `/private/var`），分量会错位导致合法路径误拒。
    let root_norm = lexical_normalize(root);
    let target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        root_norm.join(target)
    };

    // 词法主判定（跨平台一致）：折叠 `.`/`..` 后 target 必须是 root 的延伸。
    // 任何 `..` 弹出 root 之外即拒绝；纯分量运算、不访问文件系统，不依赖
    // OS 的 canonicalize 返回形式（Windows 的 `\\?\` 前缀/盘符差异不影响）。
    if !lexical_normalize(&target).starts_with(&root_norm) {
        return false;
    }

    // symlink 补充检查：canonicalize（解析链接）后仍须落在 canonical root 内。
    // 目标不存在（新建文件）时对最近存在的父目录 canonicalize，再拼接剩余段。
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root_norm.clone());
    if let Ok(c) = target.canonicalize() {
        return lexical_normalize(&c).starts_with(&canonical_root);
    }
    let mut ancestor = target.as_path();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(c) = ancestor.canonicalize() {
            let mut full = c;
            for seg in rest.iter().rev() {
                full.push(seg);
            }
            return lexical_normalize(&full).starts_with(&canonical_root);
        }
        match ancestor.components().next_back() {
            Some(Component::ParentDir) => {
                rest.push(std::ffi::OsString::from(".."));
            }
            Some(Component::CurDir) => {}
            Some(_) => {
                if let Some(name) = ancestor.file_name() {
                    rest.push(name.to_os_string());
                }
            }
            None => {}
        }
        match ancestor.parent() {
            Some(p) => {
                ancestor = p;
            }
            None => break,
        }
    }

    // 完全无法解析（如根目录都不存在）：词法判定在上方已通过，此处恒真。
    true
}

/// 词法路径规范化：折叠 `.` / `..`，不访问文件系统。
fn lexical_normalize(p: &std::path::Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
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

#[derive(Debug, thiserror::Error)]
pub enum PermissionError {
    #[error("tool '{tool}' denied: {reason}")]
    Denied { tool: String, reason: String },

    #[error("tool '{tool}' requires user approval")]
    RequiresApproval { tool: String },

    #[error("invalid policy: {0}")]
    InvalidPolicy(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- Tool matching ---

    #[test]
    fn wildcard_matches_all_tools() {
        assert!(tool_matches("*", "bash"));
        assert!(tool_matches("*", "read_file"));
        assert!(tool_matches("*", "any_tool"));
    }

    #[test]
    fn exact_tool_match() {
        assert!(tool_matches("Bash", "Bash"));
        assert!(!tool_matches("Bash", "bash"));
    }

    // --- Subject matching ---

    #[test]
    fn exact_subject_match() {
        assert!(simple_glob_match("rm -rf /", "rm -rf /"));
        assert!(!simple_glob_match("rm -rf /", "ls -la"));
    }

    #[test]
    fn glob_star_star() {
        assert!(simple_glob_match("**", "anything"));
        assert!(simple_glob_match("docs/**", "docs/api/reference.md"));
        assert!(simple_glob_match("docs/**", "docs/index.md"));
        assert!(simple_glob_match("**/test", "some/deep/path/test"));
    }

    #[test]
    fn glob_suffix() {
        assert!(simple_glob_match("*.go", "main.go"));
        assert!(simple_glob_match("*.rs", "lib.rs"));
        assert!(!simple_glob_match("*.go", "main.rs"));
    }

    #[test]
    fn glob_prefix_slash() {
        assert!(simple_glob_match("src/*", "src/main.rs"));
        assert!(!simple_glob_match("src/*", "src")); // only matches contents
        assert!(!simple_glob_match("src/*", "tests/main.rs"));
    }

    #[test]
    fn glob_contains() {
        assert!(simple_glob_match("*test*", "my_test_file"));
        assert!(simple_glob_match("*delete*", "rm -rf delete_everything"));
        assert!(!simple_glob_match("*delete*", "rm -rf remove_all"));
    }

    #[test]
    fn exact_subject_matches_literal_command() {
        // 精确匹配规则只命中字面量相等：`rm *` 不得放大成 `rm -rf /`，
        // 中间 glob（`rm *.tmp`）也不得失去命中原命令的能力。
        assert!(exact_subject_matches(
            "rm *",
            &serde_json::json!({"command": "rm *"})
        ));
        assert!(!exact_subject_matches(
            "rm *",
            &serde_json::json!({"command": "rm -rf /"})
        ));
        assert!(exact_subject_matches(
            "rm *.tmp",
            &serde_json::json!({"command": "rm *.tmp"})
        ));
        assert!(!exact_subject_matches(
            "rm *.tmp",
            &serde_json::json!({"command": "rm x.tmp"})
        ));
        assert!(!exact_subject_matches(
            "ls *.rs",
            &serde_json::json!({"command": "ls -la"})
        ));
    }

    // --- Policy ---

    // --- Rate limit ---

    fn allow_all_gate() -> PermissionGate {
        PermissionGate::new(Policy {
            mode: Decision::Allow,
            allow: vec![],
            ask: vec![],
            deny: vec![],
        })
    }

    #[test]
    fn rate_limit_denies_after_threshold() {
        let gate = allow_all_gate().with_rate_limit(3);
        // 前 3 次在窗口内，不触发限流
        for _ in 0..3 {
            assert!(!gate.rate_limited());
        }
        // 第 4 次起滚动窗口已满 → 限流
        assert!(gate.rate_limited());
        assert!(gate.rate_limited());
    }

    #[test]
    fn no_rate_limit_never_denies() {
        let gate = allow_all_gate();
        for _ in 0..100 {
            assert!(!gate.rate_limited());
        }
    }

    #[test]
    fn rate_limit_floor_is_one() {
        // with_rate_limit(0) 被抬升到 1，避免永久拒绝首次调用
        let gate = allow_all_gate().with_rate_limit(0);
        assert!(!gate.rate_limited());
        assert!(gate.rate_limited());
    }

    // --- Rate limit through the public check() path ---

    /// Minimal writer tool for exercising `PermissionGate::check`.
    struct StubTool;

    #[async_trait::async_trait]
    impl Tool for StubTool {
        fn schema(&self) -> deepseeknova_core::ToolSchema {
            deepseeknova_core::ToolSchema {
                name: "stub".to_string(),
                description: "stub tool for tests".to_string(),
                parameters: Value::Null,
            }
        }

        async fn execute(
            &self,
            _ctx: &deepseeknova_core::ToolContext,
            _args: &str,
        ) -> anyhow::Result<String> {
            Ok(String::new())
        }
    }

    #[test]
    fn check_denies_once_rate_limit_exhausted() {
        // 策略本身全部 Allow，但限流优先于策略判定
        let gate = allow_all_gate().with_rate_limit(2);
        let tool = StubTool;
        assert_eq!(gate.check(&tool, "{}").decision(), Decision::Allow);
        assert_eq!(gate.check(&tool, "{}").decision(), Decision::Allow);
        // 第三次起窗口已满 → 硬性 Deny，不再进入策略/缓存判定
        let v = gate.check(&tool, "{}");
        assert_eq!(v.decision(), Decision::Deny);
        assert!(v.is_hard_deny());
        assert_eq!(gate.check(&tool, "{}").decision(), Decision::Deny);
    }

    #[test]
    fn check_without_rate_limit_is_unaffected() {
        // 负例：未启用限流时，连续调用始终走策略判定（Allow）
        let gate = allow_all_gate();
        let tool = StubTool;
        for _ in 0..20 {
            assert_eq!(gate.check(&tool, "{}").decision(), Decision::Allow);
        }
    }

    #[test]
    fn check_rate_limit_denies_even_cached_allow() {
        // 会话缓存中已有 Allow 决策，限流耗尽后仍须 Deny
        let gate = allow_all_gate().with_rate_limit(1);
        let tool = StubTool;
        gate.cache_decision("stub", "{}", Decision::Allow);
        assert_eq!(gate.check(&tool, "{}").decision(), Decision::Allow);
        let v = gate.check(&tool, "{}");
        assert_eq!(v.decision(), Decision::Deny);
        assert!(v.is_hard_deny());
    }

    #[test]
    fn deny_overrides_allow() {
        let policy = Policy {
            mode: Decision::Ask,
            allow: vec![Rule::new("Bash")],
            ask: vec![],
            deny: vec![Rule::with_subject("Bash", "rm *")],
        };
        assert_eq!(
            policy.decide("Bash", false, &Value::String("rm -rf /".into())),
            Decision::Deny
        );
    }

    #[test]
    fn subject_match_allows_when_no_match() {
        let policy = Policy {
            mode: Decision::Ask,
            allow: vec![Rule::with_subject("Bash", "ls *")],
            ask: vec![],
            deny: vec![],
        };
        // "ls -la" matches "ls *" → allow
        assert_eq!(
            policy.decide("Bash", false, &Value::String("ls -la".into())),
            Decision::Allow
        );
        // "rm -rf /" does NOT match "ls *" → fallback to mode
        assert_eq!(
            policy.decide("Bash", false, &Value::String("rm -rf /".into())),
            Decision::Ask
        );
    }

    #[test]
    fn reader_fallback_is_allow() {
        let policy = Policy {
            mode: Decision::Ask,
            allow: vec![],
            ask: vec![],
            deny: vec![],
        };
        assert_eq!(
            policy.decide("read_file", true, &Value::Null),
            Decision::Allow
        );
    }

    #[test]
    fn writer_fallback_follows_mode() {
        let policy = Policy {
            mode: Decision::Ask,
            allow: vec![],
            ask: vec![],
            deny: vec![],
        };
        assert_eq!(policy.decide("bash", false, &Value::Null), Decision::Ask);
    }

    #[test]
    fn policy_builder_safe_defaults() {
        let policy = PolicyBuilder::new().safe_defaults().build();
        assert_eq!(policy.mode, Decision::Ask);
        // 修复点：safe_defaults 不得添加 allow("*")（那会让写工具全放行）
        assert_eq!(policy.allow.len(), 0);
        // 读工具仍走 fallback 放行
        assert_eq!(
            policy.decide("read_file", true, &Value::Null),
            Decision::Allow
        );
        // 写工具回退到 Ask
        assert_eq!(policy.decide("bash", false, &Value::Null), Decision::Ask);
    }

    #[test]
    fn policy_builder_custom() {
        let policy = PolicyBuilder::new()
            .default_mode(Decision::Deny)
            .allow(Rule::new("read_file"))
            .allow(Rule::new("ls"))
            .deny(Rule::new("bash"))
            .build();

        assert_eq!(policy.mode, Decision::Deny);
        assert_eq!(policy.allow.len(), 2);
        assert_eq!(policy.deny.len(), 1);
    }

    // --- CheckVerdict 契约：硬拒 / 建议 / 原因 ---

    /// 带名字与只读标志的最小工具（便于按工具名触发分支）。
    struct NamedTool {
        name: &'static str,
        read_only: bool,
    }

    #[async_trait::async_trait]
    impl Tool for NamedTool {
        fn schema(&self) -> deepseeknova_core::ToolSchema {
            deepseeknova_core::ToolSchema {
                name: self.name.to_string(),
                description: "named stub".to_string(),
                parameters: Value::Null,
            }
        }

        fn read_only(&self) -> bool {
            self.read_only
        }

        async fn execute(
            &self,
            _ctx: &deepseeknova_core::ToolContext,
            _args: &str,
        ) -> anyhow::Result<String> {
            Ok(String::new())
        }
    }

    impl NamedTool {
        fn writer(name: &'static str) -> Self {
            Self {
                name,
                read_only: false,
            }
        }
    }

    fn allow_all_gate_with_root(root: std::path::PathBuf) -> PermissionGate {
        allow_all_gate().with_workspace_root(root)
    }

    #[test]
    fn check_hard_denies_tool_level_injection() {
        // 工具级注入面（git -c/--config-env、UNC/URL/SMB）= 安全硬拒：
        // 不附带建议，用户不可通过规则覆盖
        let gate = allow_all_gate();
        let tool = NamedTool::writer("bash");

        // git 配置注入（看起来只读的攻击面）
        let v = gate.check(
            &tool,
            r#"{"command": "git -c core.pager='cat /etc/passwd' log"}"#,
        );
        assert_eq!(v.decision(), Decision::Deny);
        assert!(v.is_hard_deny());
        assert!(v.suggestions().is_empty());

        // UNC/URL/SMB 路径形态
        let v = gate.check(&tool, r#"{"command": "//evil/share"}"#);
        assert_eq!(v.decision(), Decision::Deny);
        assert!(v.is_hard_deny());
        assert!(v.suggestions().is_empty());
    }

    #[test]
    fn check_shell_composition_goes_through_policy() {
        // 普通 shell 组合（命令替换/链式/重定向）不再硬拒：归 NotReadOnly，
        // 由权限规则裁决——allow-all 放行、默认 Ask 询问，绝不静默免询问。
        let allow_all = allow_all_gate();
        let tool = NamedTool::writer("bash");
        assert_eq!(
            allow_all
                .check(&tool, r#"{"command": "ls $(rm -rf /)"}"#)
                .decision(),
            Decision::Allow
        );

        let ask_gate = PermissionGate::new(Policy {
            mode: Decision::Ask,
            allow: vec![],
            ask: vec![],
            deny: vec![],
        });
        let v = ask_gate.check(&tool, r#"{"command": "cat /etc/passwd > /tmp/steal"}"#);
        assert_eq!(v.decision(), Decision::Ask);
        assert!(!v.is_hard_deny());
        assert_eq!(v.suggestions().len(), 1);
    }

    #[test]
    fn check_readonly_command_skips_prompt() {
        // 只读命令（四层白名单命中）免询问直接放行
        let policy = Policy {
            mode: Decision::Ask,
            allow: vec![],
            ask: vec![],
            deny: vec![],
        };
        let gate = PermissionGate::new(policy);
        let tool = NamedTool::writer("bash");

        let v = gate.check(&tool, r#"{"command": "git status"}"#);
        assert_eq!(v.decision(), Decision::Allow);

        let v = gate.check(&tool, r#"{"command": "ls -la"}"#);
        assert_eq!(v.decision(), Decision::Allow);

        // 非只读命令仍走策略（Ask）
        let v = gate.check(&tool, r#"{"command": "rm -rf /tmp/x"}"#);
        assert_eq!(v.decision(), Decision::Ask);
    }

    #[test]
    fn shell_readonly_kind_exposes_classification_for_approval_risk_label() {
        use deepseeknova_security::readonly::ReadOnlyKind;
        let gate = allow_all_gate();
        // 只读 / 非只读 / 危险三态
        assert_eq!(
            gate.shell_readonly_kind("bash", r#"{"command": "git status"}"#),
            Some(ReadOnlyKind::ReadOnly)
        );
        assert_eq!(
            gate.shell_readonly_kind("Bash", r#"{"command": "rm -rf /tmp/x"}"#),
            Some(ReadOnlyKind::NotReadOnly)
        );
        assert_eq!(
            gate.shell_readonly_kind(
                "shell",
                r#"{"command": "git -c core.pager='sh -x' status"}"#
            ),
            Some(ReadOnlyKind::Dangerous)
        );
        // 非 shell 工具 / 不可解析参数 → None
        assert_eq!(
            gate.shell_readonly_kind("grep", r#"{"command": "x"}"#),
            None
        );
        assert_eq!(gate.shell_readonly_kind("bash", "not-json"), None);
    }

    #[test]
    fn check_deny_rule_beats_readonly_auto_allow() {
        // H1 回归：用户 deny 规则优先于只读免询问（"Deny always wins"）。
        // 修复前 readonly 分类在 policy 之前短路，`Bash("git *")` deny
        // 规则对 `git status` 静默失效。
        let policy = Policy {
            mode: Decision::Ask,
            allow: vec![],
            ask: vec![],
            deny: vec![Rule::with_subject("bash", "git *")],
        };
        let gate = PermissionGate::new(policy);
        let tool = NamedTool::writer("bash");

        let v = gate.check(&tool, r#"{"command": "git status"}"#);
        assert_eq!(
            v.decision(),
            Decision::Deny,
            "deny rule must beat readonly auto-allow"
        );
        assert!(!v.is_hard_deny(), "rule deny is not a hard deny");
    }

    #[test]
    fn check_cached_deny_beats_readonly_auto_allow() {
        // H1 回归：会话缓存中的用户拒绝优先于只读免询问
        let policy = Policy {
            mode: Decision::Ask,
            allow: vec![],
            ask: vec![],
            deny: vec![],
        };
        let gate = PermissionGate::new(policy);
        let tool = NamedTool::writer("bash");
        gate.cache_decision("bash", r#"{"command": "ls -la /etc"}"#, Decision::Deny);

        let v = gate.check(&tool, r#"{"command": "ls -la /etc"}"#);
        assert_eq!(
            v.decision(),
            Decision::Deny,
            "cached deny must beat readonly auto-allow"
        );
    }

    #[test]
    fn check_ask_rule_beats_readonly_auto_allow() {
        // R2/F3 回归：显式 ask 规则命中时，只读命令不得免询问放行
        //（与 deny 同优先级语义，方向对称——用户显式要求确认就须确认）
        let policy = Policy {
            mode: Decision::Ask,
            allow: vec![],
            ask: vec![Rule::with_subject("bash", "git *")],
            deny: vec![],
        };
        let gate = PermissionGate::new(policy);
        let tool = NamedTool::writer("bash");

        let v = gate.check(&tool, r#"{"command": "git status"}"#);
        assert_eq!(
            v.decision(),
            Decision::Ask,
            "explicit ask rule must beat readonly auto-allow"
        );

        // 未命中 ask 规则的只读命令仍免询问
        let v = gate.check(&tool, r#"{"command": "ls -la"}"#);
        assert_eq!(v.decision(), Decision::Allow);
    }

    #[test]
    fn check_hard_denies_malformed_json_for_writer_with_root() {
        // 回归：畸形 JSON（如 Windows 路径含未转义反斜杠，`\a`/`\.` 非法
        // 转义）曾静默降级为 Null、跳过工作区路径守卫导致逃逸放行。
        // 现在对"写工具 + 有工作区根"fail-closed 硬拒。
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        let gate = allow_all_gate_with_root(root.clone());
        let tool = NamedTool::writer("write");

        let v = gate.check(&tool, r#"{"path": "D:\a\_temp\.tmpX\..\etc\shadow"}"#);
        assert_eq!(v.decision(), Decision::Deny);
        assert!(v.is_hard_deny());

        // 无工作区根约束时：畸形 JSON 不硬拒、不 panic（行为与旧逻辑一致）。
        let gate2 = allow_all_gate();
        let v = gate2.check(&tool, r#"{"path": "D:\a\_temp\.tmpX\..\etc\shadow"}"#);
        assert_eq!(v.decision(), Decision::Allow);
    }

    #[test]
    fn check_hard_denies_path_outside_workspace() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        let gate = allow_all_gate_with_root(root.clone());
        let tool = NamedTool::writer("write");

        // 绝对路径越界
        let v = gate.check(&tool, r#"{"path": "/etc/passwd"}"#);
        assert_eq!(v.decision(), Decision::Deny);
        assert!(v.is_hard_deny());

        // `..` 逃逸（词法上仍在根内，解析后越界）。用 serde_json 序列化以
        // 正确转义路径（Windows 反斜杠经手写 format! 会变成畸形 JSON）。
        let escape = root.join("..").join("etc").join("shadow");
        let args = serde_json::json!({ "path": escape.display().to_string() }).to_string();
        let v = gate.check(&tool, &args);
        assert_eq!(v.decision(), Decision::Deny, "dotdot escape must be denied");
    }

    #[cfg(unix)]
    #[test]
    fn check_denies_symlink_escape() {
        // 工作区内 symlink 指向外部目录 → 写入目标实际在外部，必须拒绝
        let ws = tempfile::tempdir().expect("ws");
        let outside = tempfile::tempdir().expect("outside");
        let link = ws.path().join("link");
        std::os::unix::fs::symlink(outside.path(), &link).expect("symlink");
        let gate = allow_all_gate_with_root(ws.path().to_path_buf());
        let tool = NamedTool::writer("write");

        let target = format!(r#"{{"path": "{}"}}"#, link.join("pwn.txt").display());
        let v = gate.check(&tool, &target);
        assert_eq!(
            v.decision(),
            Decision::Deny,
            "symlink escape must be denied"
        );
    }

    #[test]
    fn check_allows_path_inside_workspace() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        let gate = allow_all_gate_with_root(root.clone());
        let tool = NamedTool::writer("write");

        // 尚不存在的目标文件（父目录链最深层可解析）应放行
        let target = root.join("a").join("b").join("new.rs");
        let args = serde_json::json!({ "path": target.display().to_string() }).to_string();
        let v = gate.check(&tool, &args);
        assert_eq!(v.decision(), Decision::Allow);
        // 相对路径按工作区根解释
        let v = gate.check(&tool, r#"{"path": "relative/new.rs"}"#);
        assert_eq!(v.decision(), Decision::Allow);
        // 不存在的中间目录 + `..` 仍留在根内（词法折叠后应放行）
        let v = gate.check(&tool, r#"{"path": "missing/../inside.txt"}"#);
        assert_eq!(v.decision(), Decision::Allow);
        // 相对路径 `..` 逃逸拒绝
        let v = gate.check(&tool, r#"{"path": "../outside"}"#);
        assert_eq!(v.decision(), Decision::Deny);
    }

    #[test]
    fn check_denies_dotdot_escape_through_missing_dir() {
        // 回归：祖先回溯曾丢弃 `..` 分量，导致
        // `root/missing/../../outside/pwn.txt` 被误判为工作区内；
        // 工具 create_dir_all 后该路径会真实解析到工作区外。
        let ws = tempfile::tempdir().expect("ws");
        let outside = tempfile::tempdir().expect("outside");
        let root = ws.path().to_path_buf();
        let gate = allow_all_gate_with_root(root.clone());
        let tool = NamedTool::writer("write");

        let escape = root
            .join("missing")
            .join("..")
            .join("..")
            .join(outside.path().file_name().unwrap())
            .join("pwn.txt");
        let args = serde_json::json!({ "path": escape.display().to_string() }).to_string();
        let v = gate.check(&tool, &args);
        assert_eq!(
            v.decision(),
            Decision::Deny,
            "dotdot escape through missing dir must be denied"
        );
        assert!(v.is_hard_deny());
    }

    #[test]
    fn check_attaches_suggestion_on_ask() {
        // 拒绝即教育：Ask 附带"添加 allow 规则即可自动放行"
        let policy = Policy {
            mode: Decision::Ask,
            allow: vec![],
            ask: vec![],
            deny: vec![],
        };
        let gate = PermissionGate::new(policy);
        let tool = NamedTool::writer("write");
        let v = gate.check(&tool, r#"{"path": "/tmp/x"}"#);
        assert_eq!(v.decision(), Decision::Ask);
        assert!(!v.is_hard_deny());
        assert_eq!(v.suggestions().len(), 1);
        let s = &v.suggestions()[0];
        assert_eq!(s.behavior, Decision::Allow);
        assert_eq!(s.rule.tool, "write");
        assert_eq!(s.rule.subject.as_deref(), Some("/tmp/x"));
    }

    #[test]
    fn suggested_allow_rule_matches_only_the_exact_command() {
        // 含通配符的命令被建议为精确规则：批准后只放行原命令，
        // 不放大成前缀匹配（`rm *` 不得放行 `rm -rf /`）。
        let gate = PermissionGate::new(Policy {
            mode: Decision::Ask,
            allow: vec![],
            ask: vec![],
            deny: vec![],
        });
        let tool = NamedTool::writer("bash");
        let v = gate.check(&tool, r#"{"command": "rm *"}"#);
        assert_eq!(v.decision(), Decision::Ask);
        let s = &v.suggestions()[0];
        assert!(s.rule.exact, "suggested rule must be exact");
        assert_eq!(s.rule.subject.as_deref(), Some("rm *"));

        let approved = PermissionGate::new(Policy {
            mode: Decision::Ask,
            allow: vec![s.rule.clone()],
            ask: vec![],
            deny: vec![],
        });
        assert_eq!(
            approved.check(&tool, r#"{"command": "rm *"}"#).decision(),
            Decision::Allow
        );
        // 前缀放大被阻断：rm -rf / 不命中精确规则，走 mode 回退 Ask
        assert_eq!(
            approved
                .check(&tool, r#"{"command": "rm -rf /"}"#)
                .decision(),
            Decision::Ask
        );
    }

    #[test]
    fn check_deny_rule_reason_names_rule() {
        // 规则拒（非硬拒）：reason 指名命中的 deny 规则；
        // 不附加 allow 建议（deny 优先于 allow，该建议无效）
        let policy = Policy {
            mode: Decision::Ask,
            allow: vec![],
            ask: vec![],
            deny: vec![Rule::with_subject("bash", "rm *")],
        };
        let gate = PermissionGate::new(policy);
        let tool = NamedTool::writer("bash");
        // "rm -f x.txt" 不在危险命令黑名单，走到规则层被 deny
        let v = gate.check(&tool, r#"{"command": "rm -f x.txt"}"#);
        assert_eq!(v.decision(), Decision::Deny);
        assert!(!v.is_hard_deny());
        assert!(v.reason().contains("rm *"), "reason: {}", v.reason());
        assert!(v.suggestions().is_empty());
    }

    #[test]
    fn check_cached_decision_roundtrips_verdict() {
        let gate = allow_all_gate();
        let tool = NamedTool::writer("write");
        gate.cache_decision("write", r#"{"path": "/tmp/x"}"#, Decision::Deny);
        let v = gate.check(&tool, r#"{"path": "/tmp/x"}"#);
        assert_eq!(v.decision(), Decision::Deny);
        assert!(!v.is_hard_deny(), "cached 决策不是硬拒");
    }

    #[test]
    fn check_guards_move_file_both_paths() {
        // move_file 双路径：source 或 destination 任一出工作区都必须硬拒
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        let gate = allow_all_gate_with_root(root.clone());
        let tool = NamedTool::writer("move_file");

        // destination 越界
        let v = gate.check(&tool, r#"{"source":"a.txt","destination":"/etc/passwd"}"#);
        assert_eq!(v.decision(), Decision::Deny);
        assert!(v.is_hard_deny());
        assert!(v.reason().contains("/etc/passwd"), "reason: {}", v.reason());

        // source 越界
        let v = gate.check(&tool, r#"{"source":"/etc/passwd","destination":"b.txt"}"#);
        assert_eq!(v.decision(), Decision::Deny);
        assert!(v.is_hard_deny());

        // 双路径都在工作区内 → 放行
        let v = gate.check(&tool, r#"{"source":"a.txt","destination":"b.txt"}"#);
        assert_eq!(v.decision(), Decision::Allow);

        // 相对 `..` 逃逸任一方向都拒绝
        let v = gate.check(&tool, r#"{"source":"ok.txt","destination":"../outside"}"#);
        assert_eq!(v.decision(), Decision::Deny);
        assert!(v.is_hard_deny());
    }

    #[test]
    fn extract_paths_collects_multi_and_nested() {
        let v = serde_json::json!({
            "source": "s.txt",
            "destination": "d.txt",
            "edits": [{"path": "nested.rs"}],
            "other": "not-a-path-key",
        });
        let paths = extract_paths(&v);
        assert_eq!(paths, vec!["s.txt", "d.txt", "nested.rs"]);
    }
}

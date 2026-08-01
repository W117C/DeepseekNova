//! # Permission — Policy-based tool execution gating
//!
//! Implements allow/ask/deny permission gates for every tool invocation.
//! Supports per-tool rules, user confirmation prompts, and session-level
//! permission caching.

use deepseeknova_core::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    /// Tool name, e.g. "Bash", "read_file", or "*" for all tools.
    pub tool: String,
    /// Optional subject pattern, e.g. "rm *", "docs/**", "go test:*".
    /// Uses simple glob matching. Only applies when tool name matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

impl Rule {
    pub fn new(tool: impl Into<String>) -> Self {
        Self {
            tool: tool.into(),
            subject: None,
        }
    }

    pub fn with_subject(tool: impl Into<String>, subject: impl Into<String>) -> Self {
        Self {
            tool: tool.into(),
            subject: Some(subject.into()),
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
                subject_matches(subject, args)
            } else {
                true // No subject constraint → matches any args
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

    /// Allow all read tools, ask for write tools.
    pub fn safe_defaults(mut self) -> Self {
        self.mode = Decision::Ask;
        self.allow.push(Rule::new("*"));
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
    pub fn check(&self, tool: &dyn Tool, args: &str) -> Decision {
        // Rate limit first: a hard cap independent of per-tool decisions.
        if self.rate_limited() {
            return Decision::Deny;
        }

        let args_value: Value = serde_json::from_str(args).unwrap_or(Value::Null);
        let tool_name = &tool.schema().name;

        // Path-based guard: deny writes outside workspace
        if !tool.read_only() {
            if let Some(ref root) = self.workspace_root {
                if let Some(path) = extract_path(&args_value) {
                    if !is_within_workspace(root, &path) {
                        return Decision::Deny;
                    }
                }
            }
        }

        // Dangerous command detection for shell tools
        if tool_name == "Bash" || tool_name == "bash" || tool_name == "shell" {
            if let Some(cmd) = extract_command(&args_value) {
                if is_dangerous_command(&cmd) {
                    return Decision::Deny;
                }
            }
        }

        // Check session cache
        let cache_key = compute_cache_key(tool_name, args);
        if let Ok(cache) = self.session_cache.lock() {
            if let Some(cached) = cache.get(&cache_key) {
                return *cached;
            }
        }

        // Evaluate policy
        self.policy.decide(tool_name, tool.read_only(), &args_value)
    }

    /// Cache a user's decision for this session (called after user responds to Ask).
    pub fn cache_decision(&self, tool_name: &str, args: &str, decision: Decision) {
        let key = compute_cache_key(tool_name, args);
        if let Ok(mut cache) = self.session_cache.lock() {
            cache.insert(key, decision);
        }
    }

    /// Clear the session cache (e.g., on new session).
    pub fn clear_cache(&self) {
        if let Ok(mut cache) = self.session_cache.lock() {
            cache.clear();
        }
    }
}

// ---------------------------------------------------------------------------
// Path-based helpers
// ---------------------------------------------------------------------------

/// Extract a file path from tool arguments.
fn extract_path(args: &Value) -> Option<String> {
    if let Value::Object(map) = args {
        for key in &["path", "file", "file_path", "target", "directory"] {
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
fn is_within_workspace(root: &std::path::Path, path: &str) -> bool {
    let path_buf = std::path::Path::new(path);
    // Absolute path: must start with workspace root
    if path_buf.is_absolute() {
        path_buf.starts_with(root)
    } else {
        // Relative paths are always within workspace
        !path.contains("..")
    }
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

/// Detect dangerous shell commands that should always be denied.
fn is_dangerous_command(cmd: &str) -> bool {
    let cmd_lower = cmd.to_lowercase();
    const DANGEROUS_PATTERNS: &[&str] = &[
        "rm -rf /",
        "rm -rf /*",
        "mkfs",
        "dd if=",
        ":(){ :|:& };:", // fork bomb
        "chmod -r 777 /",
        "> /dev/sda",
        "shutdown",
        "reboot",
        "format c:",
    ];
    DANGEROUS_PATTERNS.iter().any(|p| cmd_lower.contains(p))
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
        assert_eq!(gate.check(&tool, "{}"), Decision::Allow);
        assert_eq!(gate.check(&tool, "{}"), Decision::Allow);
        // 第三次起窗口已满 → 硬性 Deny，不再进入策略/缓存判定
        assert_eq!(gate.check(&tool, "{}"), Decision::Deny);
        assert_eq!(gate.check(&tool, "{}"), Decision::Deny);
    }

    #[test]
    fn check_without_rate_limit_is_unaffected() {
        // 负例：未启用限流时，连续调用始终走策略判定（Allow）
        let gate = allow_all_gate();
        let tool = StubTool;
        for _ in 0..20 {
            assert_eq!(gate.check(&tool, "{}"), Decision::Allow);
        }
    }

    #[test]
    fn check_rate_limit_denies_even_cached_allow() {
        // 会话缓存中已有 Allow 决策，限流耗尽后仍须 Deny
        let gate = allow_all_gate().with_rate_limit(1);
        let tool = StubTool;
        gate.cache_decision("stub", "{}", Decision::Allow);
        assert_eq!(gate.check(&tool, "{}"), Decision::Allow);
        assert_eq!(gate.check(&tool, "{}"), Decision::Deny);
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
        assert_eq!(policy.allow.len(), 1);
        assert_eq!(policy.allow[0].tool, "*");
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
}

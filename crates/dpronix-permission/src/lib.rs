use dpronix_core::tool::Tool;
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
pub struct PermissionGate {
    policy: Policy,
}

impl PermissionGate {
    pub fn new(policy: Policy) -> Self {
        Self { policy }
    }

    /// Check whether a tool call should be allowed.
    pub fn check(&self, tool: &dyn Tool, args: &str) -> Decision {
        let args_value: Value = serde_json::from_str(args).unwrap_or(Value::Null);
        self.policy
            .decide(&tool.schema().name, tool.read_only(), &args_value)
    }
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

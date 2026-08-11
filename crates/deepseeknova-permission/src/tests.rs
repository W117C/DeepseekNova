use super::*;
use deepseeknova_security::audit::JsonlAuditLogger;

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
    ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
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
    ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
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

// --- 权限模式预设：同一规则不同模式下裁决不同 ---

/// 无规则、模式回退驱动的 gate（写工具默认裁决由预设决定）。
fn ask_policy_gate() -> PermissionGate {
    PermissionGate::new(Policy {
        mode: Decision::Ask,
        allow: vec![],
        ask: vec![],
        deny: vec![],
    })
}

#[test]
fn mode_plan_asks_writers_allows_readers() {
    let gate = ask_policy_gate().with_mode(Some(PermissionMode::Plan));
    let writer = NamedTool::writer("write_file");
    assert_eq!(gate.check(&writer, "{}").decision(), Decision::Ask);
    let shell = NamedTool::writer("bash");
    assert_eq!(
        gate.check(&shell, r#"{"command": "git push"}"#).decision(),
        Decision::Ask
    );
    let reader = NamedTool {
        name: "read_file",
        read_only: true,
    };
    // 只读工具按 read_only fallback 放行
    let v = gate.check(&reader, "{}");
    assert_eq!(v.decision(), Decision::Allow);
}

#[test]
fn mode_accept_edits_allows_file_edits_asks_shell_writes() {
    let gate = ask_policy_gate().with_mode(Some(PermissionMode::AcceptEdits));
    let writer = NamedTool::writer("write_file");
    assert_eq!(gate.check(&writer, "{}").decision(), Decision::Allow);
    let editor = NamedTool::writer("edit_file");
    assert_eq!(gate.check(&editor, "{}").decision(), Decision::Allow);
    let mover = NamedTool::writer("move_file");
    assert_eq!(
        gate.check(&mover, r#"{"source":"a","destination":"b"}"#)
            .decision(),
        Decision::Allow
    );
    let shell = NamedTool::writer("bash");
    assert_eq!(
        gate.check(&shell, r#"{"command": "git push"}"#).decision(),
        Decision::Ask,
        "accept_edits 下 shell 写形态仍询问"
    );
    // 只读命令仍免询问
    assert_eq!(
        gate.check(&shell, r#"{"command": "git status"}"#)
            .decision(),
        Decision::Allow
    );
}

#[test]
fn mode_auto_allows_all_writers() {
    let gate = ask_policy_gate().with_mode(Some(PermissionMode::Auto));
    let writer = NamedTool::writer("write_file");
    assert_eq!(gate.check(&writer, "{}").decision(), Decision::Allow);
    let shell = NamedTool::writer("bash");
    assert_eq!(
        gate.check(&shell, r#"{"command": "git push"}"#).decision(),
        Decision::Allow
    );
}

#[test]
fn mode_none_keeps_legacy_policy_mode() {
    // 预设 None（缺省）→ 写工具回退 Policy.mode（Ask）。
    let gate = ask_policy_gate(); // mode: None by default
    assert_eq!(gate.mode(), None);
    let writer = NamedTool::writer("write_file");
    assert_eq!(gate.check(&writer, "{}").decision(), Decision::Ask);
    // 显式回退 None：恢复旧行为。
    let gate = ask_policy_gate().with_mode(Some(PermissionMode::Auto));
    gate.set_mode(None);
    assert_eq!(gate.check(&writer, "{}").decision(), Decision::Ask);
}

#[test]
fn mode_does_not_override_deny_or_ask_rules() {
    // 规则优先级不因模式改变：deny/ask 恒优先于 allow/回退。
    let gate = PermissionGate::new(Policy {
        mode: Decision::Ask,
        allow: vec![],
        ask: vec![Rule::with_subject("bash", "git *")],
        deny: vec![Rule::with_subject("bash", "rm *")],
    })
    .with_mode(Some(PermissionMode::Auto));
    let shell = NamedTool::writer("bash");
    // deny 优先
    assert_eq!(
        gate.check(&shell, r#"{"command": "rm -rf /tmp/x"}"#)
            .decision(),
        Decision::Deny
    );
    // 显式 ask 规则优先（即使 auto 模式）
    assert_eq!(
        gate.check(&shell, r#"{"command": "git push"}"#).decision(),
        Decision::Ask
    );
}

#[test]
fn permission_mode_serde_roundtrip() {
    let s = serde_json::to_string(&PermissionMode::AcceptEdits).unwrap();
    assert_eq!(s, "\"accept_edits\"");
    assert_eq!(
        serde_json::from_str::<PermissionMode>("\"plan\"").unwrap(),
        PermissionMode::Plan
    );
    assert_eq!(
        serde_json::from_str::<PermissionMode>("\"auto\"").unwrap(),
        PermissionMode::Auto
    );
}

// --- 工作区信任：untrusted 项目层 allow 降级为 ask ---

fn project_allow_gate() -> PermissionGate {
    PermissionGate::new(Policy {
        mode: Decision::Ask,
        allow: vec![Rule::new("write_file")],
        ask: vec![],
        deny: vec![],
    })
    .with_allow_project_scoped(true)
}

#[test]
fn untrusted_project_allow_degrades_to_ask() {
    let gate = project_allow_gate(); // trusted=false 默认
    assert!(!gate.trusted());
    let writer = NamedTool::writer("write_file");
    let v = gate.check(&writer, "{}");
    assert_eq!(
        v.decision(),
        Decision::Ask,
        "untrusted 下项目层 allow 规则必须降级为 ask"
    );
    // 降级 Ask 不附"添加规则即可放行"建议（规则已存在，应信任项目）。
    assert!(
        v.suggestions().is_empty(),
        "降级 ask 不应建议添加规则: {:?}",
        v.suggestions()
    );
}

#[test]
fn trusted_project_allow_works() {
    let gate = project_allow_gate().with_trusted(true);
    assert!(gate.trusted());
    let writer = NamedTool::writer("write_file");
    assert_eq!(gate.check(&writer, "{}").decision(), Decision::Allow);
    // 运行时切换回 untrusted → 降级恢复。
    gate.set_trusted(false);
    assert_eq!(gate.check(&writer, "{}").decision(), Decision::Ask);
}

#[test]
fn user_scoped_allow_is_not_degraded_when_untrusted() {
    // allow_project_scoped=false（用户层规则）→ untrusted 也不降级。
    let gate = PermissionGate::new(Policy {
        mode: Decision::Ask,
        allow: vec![Rule::new("write_file")],
        ask: vec![],
        deny: vec![],
    }); // allow_project_scoped=false, trusted=false
    let writer = NamedTool::writer("write_file");
    assert_eq!(gate.check(&writer, "{}").decision(), Decision::Allow);
}

#[test]
fn untrusted_downgraded_allow_readonly_still_auto_allows() {
    // 项目层 allow 规则命中只读命令并被降级 → 只读仍免询问放行
    //（只读命令本就安全，降级针对写操作）。
    let gate = PermissionGate::new(Policy {
        mode: Decision::Ask,
        allow: vec![Rule::with_subject("bash", "git *")],
        ask: vec![],
        deny: vec![],
    })
    .with_allow_project_scoped(true);
    let shell = NamedTool::writer("bash");
    assert_eq!(
        gate.check(&shell, r#"{"command": "git status"}"#)
            .decision(),
        Decision::Allow,
        "只读命令在降级下仍放行"
    );
}

#[test]
fn decide_effective_honors_precedence_with_degrade() {
    // 直接测 Policy 层：degrade_allow 只影响 allow 命中，不影响 deny/ask。
    let policy = Policy {
        mode: Decision::Ask,
        allow: vec![Rule::new("write_file")],
        ask: vec![Rule::with_subject("write_file", "secret*")],
        deny: vec![],
    };
    let args = serde_json::json!({ "path": "x" });
    // allow 命中 + degrade → Ask
    assert_eq!(
        policy.decide_effective("write_file", false, &args, None, true),
        Decision::Ask
    );
    // allow 命中 + 不 degrade → Allow
    assert_eq!(
        policy.decide_effective("write_file", false, &args, None, false),
        Decision::Allow
    );
    // 显式 ask 规则优先于降级
    let secret = serde_json::json!({ "path": "secret-file" });
    assert_eq!(
        policy.decide_effective("write_file", false, &secret, None, true),
        Decision::Ask
    );
}

// ── exec 审计：preview 与真实执行决策一致性 + 无副作用 ──

#[test]
fn preview_matches_check_decision() {
    // exec 审计一致性：预览决策 == 真实执行路径 check 决策（同源）。
    let gate = PermissionGate::new(Policy {
        mode: Decision::Ask,
        allow: vec![],
        ask: vec![],
        deny: vec![],
    });
    let cases: &[(&str, bool, &str)] = &[
        // 只读命令免询问放行
        ("bash", false, r#"{"command": "git status"}"#),
        ("bash", false, r#"{"command": "ls -la"}"#),
        // 非只读 → Ask
        ("bash", false, r#"{"command": "rm -rf /tmp/x"}"#),
        // 写工具无规则 → Ask
        ("write", false, r#"{"path": "/tmp/x"}"#),
        // 读工具 → 放行
        ("read_file", true, r#"{"path": "src/main.rs"}"#),
        // 危险注入 → 硬拒
        (
            "bash",
            false,
            r#"{"command": "git -c core.pager='cat /etc/passwd' log"}"#,
        ),
        ("bash", false, r#"{"command": "//evil/share"}"#),
        // 畸形 JSON（无工作区根 → Null → Ask）
        ("write", false, r#"{"path": "D:\a\_temp\x"}"#),
    ];
    for &(tool, ro, args) in cases {
        let v = gate.check(
            &NamedTool {
                name: tool,
                read_only: ro,
            },
            args,
        );
        let p = gate.preview(tool, ro, args);
        assert_eq!(p.decision, v.decision(), "decision mismatch: {tool} {args}");
        assert_eq!(p.hard, v.is_hard_deny(), "hard mismatch: {tool} {args}");
        assert_eq!(p.reason, v.reason(), "reason mismatch: {tool} {args}");
        assert_eq!(
            p.suggestions.len(),
            v.suggestions().len(),
            "suggestion count mismatch: {tool} {args}"
        );
    }
}

#[test]
fn preview_matches_check_decision_with_rules() {
    // 规则命中路径下预览与 check 仍一致（deny/ask 覆盖只读免询问）。
    let policy = Policy {
        mode: Decision::Ask,
        allow: vec![Rule::with_subject("bash", "ls *")],
        ask: vec![Rule::with_subject("bash", "git *")],
        deny: vec![Rule::with_subject("bash", "rm *")],
    };
    let gate = PermissionGate::new(policy);
    let cases: &[(&str, bool, &str)] = &[
        ("bash", false, r#"{"command": "rm -rf /tmp/x"}"#), // deny 优先
        ("bash", false, r#"{"command": "git status"}"#),    // 显式 ask 覆盖只读
        ("bash", false, r#"{"command": "ls -la"}"#),        // allow 命中
        ("bash", false, r#"{"command": "pwd"}"#),           // 无规则 + 只读 → 放行
        ("bash", false, r#"{"command": "find . -delete"}"#), // 无规则 + 非只读 → Ask
    ];
    for &(tool, ro, args) in cases {
        let v = gate.check(
            &NamedTool {
                name: tool,
                read_only: ro,
            },
            args,
        );
        let p = gate.preview(tool, ro, args);
        assert_eq!(p.decision, v.decision(), "decision mismatch: {tool} {args}");
        assert_eq!(p.hard, v.is_hard_deny(), "hard mismatch: {tool} {args}");
        assert_eq!(p.reason, v.reason(), "reason mismatch: {tool} {args}");
    }
}

#[test]
fn preview_reports_matched_rules_chain() {
    let policy = Policy {
        mode: Decision::Ask,
        allow: vec![Rule::with_subject("bash", "ls *")],
        ask: vec![Rule::with_subject("bash", "git *")],
        deny: vec![Rule::with_subject("bash", "rm *")],
    };
    let gate = PermissionGate::new(policy);
    // 命中 deny → 规则链以 deny 打头，指名 subject
    let p = gate.preview("bash", false, r#"{"command": "rm -rf /tmp/x"}"#);
    assert_eq!(p.decision, Decision::Deny);
    assert_eq!(p.matched_rules.len(), 1);
    assert_eq!(p.matched_rules[0].source, "deny");
    assert_eq!(p.matched_rules[0].rule.subject.as_deref(), Some("rm *"));
    assert!(p.suggestions.is_empty(), "deny 不附 allow 建议");
    // 显式 ask 覆盖只读
    let p = gate.preview("bash", false, r#"{"command": "git status"}"#);
    assert_eq!(p.decision, Decision::Ask);
    assert_eq!(p.matched_rules[0].source, "ask");
    // allow 命中
    let p = gate.preview("bash", false, r#"{"command": "ls -la"}"#);
    assert_eq!(p.decision, Decision::Allow);
    assert_eq!(p.matched_rules[0].source, "allow");
    // 无规则命中 → 空链
    let p = gate.preview("bash", false, r#"{"command": "pwd"}"#);
    assert_eq!(p.decision, Decision::Allow);
    assert!(p.matched_rules.is_empty());
}

#[test]
fn preview_readonly_kind_reported_for_shell() {
    use deepseeknova_security::readonly::ReadOnlyKind;
    let gate = allow_all_gate();
    let p = gate.preview("bash", false, r#"{"command": "git status"}"#);
    assert_eq!(p.readonly_kind, Some(ReadOnlyKind::ReadOnly));
    let p = gate.preview("Bash", false, r#"{"command": "rm -rf /tmp/x"}"#);
    assert_eq!(p.readonly_kind, Some(ReadOnlyKind::NotReadOnly));
    let p = gate.preview("shell", false, r#"{"command": "//evil/share"}"#);
    assert_eq!(p.readonly_kind, Some(ReadOnlyKind::Dangerous));
    // 非 shell 工具 → None
    let p = gate.preview("read_file", true, r#"{"path": "x"}"#);
    assert_eq!(p.readonly_kind, None);
}

#[test]
fn preview_does_not_write_session_cache() {
    // 无副作用：preview 只计算，不得写会话缓存。
    let gate = PermissionGate::new(Policy {
        mode: Decision::Ask,
        allow: vec![],
        ask: vec![],
        deny: vec![],
    });
    let _ = gate.preview("bash", false, r#"{"command": "rm -rf /tmp/x"}"#);
    let _ = gate.preview("write", false, r#"{"path": "/tmp/x"}"#);
    let _ = gate.preview("bash", false, r#"{"command": "git -c core.pager='x' log"}"#);
    assert!(
        gate.session_cache.lock().unwrap().is_empty(),
        "preview 不得写会话缓存"
    );
}

#[test]
fn preview_does_not_consume_rate_limit() {
    // 无副作用：preview 不触发限流计数（限流是执行期状态）。
    let gate = allow_all_gate().with_rate_limit(1);
    for _ in 0..5 {
        let _ = gate.preview("stub", false, "{}");
    }
    let tool = StubTool;
    assert_eq!(
        gate.check(&tool, "{}").decision(),
        Decision::Allow,
        "preview 不得消耗限流窗口"
    );
}

#[test]
fn preview_captures_capability_checks() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let gate = allow_all_gate_with_root(root.clone());
    // 路径越界 → 硬拒 + 能力检查记录越界路径
    let p = gate.preview("write", false, r#"{"path": "/etc/passwd"}"#);
    assert_eq!(p.decision, Decision::Deny);
    assert!(p.hard);
    assert_eq!(
        p.capability.path_outside_workspace.as_deref(),
        Some("/etc/passwd")
    );
    let root_str = root.display().to_string();
    assert_eq!(
        p.capability.workspace_root.as_deref(),
        Some(root_str.as_str())
    );
    // 畸形 JSON（写工具 + root）→ 硬拒 + malformed_args
    let p = gate.preview("write", false, r#"{"path": "D:\a\_temp\x"}"#);
    assert_eq!(p.decision, Decision::Deny);
    assert!(p.hard);
    assert!(p.capability.malformed_args);
    // 工作区内路径 → 放行，能力检查无越界
    let args = serde_json::json!({ "path": root.join("new.rs").display().to_string() }).to_string();
    let p = gate.preview("write", false, &args);
    assert_eq!(p.decision, Decision::Allow);
    assert!(p.capability.path_outside_workspace.is_none());
}

#[test]
fn preview_gate_preview_serializes() {
    // JSON 输出契约：GatePreview 可序列化（audit --format json 依赖）。
    let gate = PermissionGate::new(Policy {
        mode: Decision::Ask,
        allow: vec![],
        ask: vec![],
        deny: vec![],
    });
    let p = gate.preview("bash", false, r#"{"command": "rm -rf /tmp/x"}"#);
    let json = serde_json::to_string(&p).unwrap();
    assert!(json.contains("\"decision\":\"ask\""), "{json}");
    assert!(json.contains("\"tool_name\":\"bash\""), "{json}");
    assert!(
        json.contains("\"readonly_kind\":\"not_read_only\""),
        "{json}"
    );
}

// ── AUDIT M1：gate 拒绝持久化到 JSONL 审计（消除取证盲区）──

#[test]
fn gate_denial_written_to_jsonl_audit() {
    // 越界路径硬拒 → JSONL 落盘（tool_name/capability/path/allowed/reason）
    let dir = tempfile::tempdir().unwrap();
    let logger = Arc::new(JsonlAuditLogger::at_workspace(dir.path()));
    let root = dir.path().join("ws");
    let gate = allow_all_gate()
        .with_workspace_root(root)
        .with_audit_logger(logger);
    let tool = NamedTool::writer("write");

    let v = gate.check(&tool, r#"{"path": "/etc/passwd"}"#);
    assert_eq!(v.decision(), Decision::Deny);
    assert!(v.is_hard_deny());

    let log = dir.path().join(".deepseeknova/security/audit.jsonl");
    let content = std::fs::read_to_string(&log).expect("audit log must be written");
    assert!(content.contains(r#""event_type":"gate_deny""#), "{content}");
    assert!(content.contains(r#""tool_name":"write""#), "{content}");
    assert!(content.contains(r#""capability":"FileWrite""#), "{content}");
    assert!(content.contains(r#""path":"/etc/passwd""#), "{content}");
    assert!(content.contains(r#""allowed":false"#), "{content}");
    assert!(content.contains("path outside workspace"), "{content}");
    assert_eq!(content.trim_end().lines().count(), 1, "每事件一行 JSON");
}

#[test]
fn gate_denies_dangerous_denyrule_ratelimit_all_audited() {
    // 危险命令硬拒 / deny 规则拒绝 / 限流拒绝三类分支均落盘
    let dir = tempfile::tempdir().unwrap();
    let logger = Arc::new(JsonlAuditLogger::at_workspace(dir.path()));
    let shell = NamedTool::writer("bash");

    // 1) 危险命令硬拒
    let gate = allow_all_gate().with_audit_logger(logger.clone());
    let v = gate.check(
        &shell,
        r#"{"command": "git -c core.pager='cat /etc/passwd' log"}"#,
    );
    assert_eq!(v.decision(), Decision::Deny);
    assert!(v.is_hard_deny());

    // 2) deny 规则拒绝（非硬拒）
    let policy = Policy {
        mode: Decision::Ask,
        allow: vec![],
        ask: vec![],
        deny: vec![Rule::with_subject("bash", "rm *")],
    };
    let gate = PermissionGate::new(policy).with_audit_logger(logger.clone());
    let v = gate.check(&shell, r#"{"command": "rm -f x.txt"}"#);
    assert_eq!(v.decision(), Decision::Deny);
    assert!(!v.is_hard_deny());

    // 3) 限流拒绝
    let gate = allow_all_gate()
        .with_rate_limit(1)
        .with_audit_logger(logger.clone());
    let stub = StubTool;
    assert_eq!(gate.check(&stub, "{}").decision(), Decision::Allow);
    let v = gate.check(&stub, "{}");
    assert_eq!(v.decision(), Decision::Deny);
    assert!(v.is_hard_deny());

    let log = dir.path().join(".deepseeknova/security/audit.jsonl");
    let content = std::fs::read_to_string(&log).expect("audit log must be written");
    assert_eq!(content.trim_end().lines().count(), 3, "三类拒绝各记一行");
    assert!(content.contains("dangerous command detected"), "{content}");
    assert!(content.contains("blocked by deny rule"), "{content}");
    assert!(
        content.contains(r#""reason":"rate limit exceeded"#),
        "{content}"
    );
    assert!(
        content.contains(r#""capability":"CommandExecute""#),
        "{content}"
    );
    // 取证字段：审计 reason 附加原始调用参数（对抗性取证需看到被拒命令原文）
    assert!(
        content.contains("rm -f x.txt"),
        "审计应含被拒命令原文: {content}"
    );
    assert!(
        content.contains("rate limit exceeded | args: {}"),
        "限流拒绝应附原始参数: {content}"
    );
}

#[test]
fn gate_without_audit_logger_records_nothing() {
    // 向后兼容：缺省无审计器 → 拒绝照常、不写审计文件、不 panic。
    let dir = tempfile::tempdir().unwrap();
    let gate = allow_all_gate().with_workspace_root(dir.path().join("ws"));
    let tool = NamedTool::writer("write");
    let v = gate.check(&tool, r#"{"path": "/etc/passwd"}"#);
    assert_eq!(v.decision(), Decision::Deny);
    let log = dir.path().join(".deepseeknova/security/audit.jsonl");
    assert!(!log.exists(), "无审计器不得写审计文件");
}

/// 验证 `From<PermissionError> for DeepseeknovaError` 让 `?` 直接把
/// `Result<_, PermissionError>` 用于返回 `Result<_, DeepseeknovaError>` 的函数。
#[test]
fn permission_error_converts_via_question_mark() {
    fn inner() -> Result<(), PermissionError> {
        Err(PermissionError::Denied {
            tool: "Bash".into(),
            reason: "blocked".into(),
        })
    }
    fn outer() -> Result<(), deepseeknova_core::DeepseeknovaError> {
        inner()?;
        Ok(())
    }
    let err = outer().unwrap_err();
    assert!(
        matches!(err, deepseeknova_core::DeepseeknovaError::Permission { .. }),
        "应映射到 Permission 类别"
    );
    assert!(!err.is_retryable(), "权限错误不应可重试");
}

/// 验证 `From<PermissionError>` 保留原始错误实例与 source 链：调用方可通过
/// `source().downcast_ref::<PermissionError>()` 恢复 `Denied` /
/// `RequiresApproval` / `InvalidPolicy` / `Io` 等具体变体。
#[test]
fn permission_error_source_preserves_variant_for_downcast() {
    fn inner() -> Result<(), PermissionError> {
        Err(PermissionError::RequiresApproval {
            tool: "Bash".into(),
        })
    }
    fn outer() -> Result<(), deepseeknova_core::DeepseeknovaError> {
        inner()?;
        Ok(())
    }
    let err = outer().unwrap_err();
    use std::error::Error as _;
    let src = err
        .source()
        .expect("Permission 变体应持有 source")
        .downcast_ref::<PermissionError>()
        .expect("source 应可 downcast 回 PermissionError");
    match src {
        PermissionError::RequiresApproval { tool } => assert_eq!(tool, "Bash"),
        other => panic!("期望 RequiresApproval，得到 {other:?}"),
    }
}

/// 验证 `From<PermissionError::Denied>` 保留 Denied 变体的 reason 字段。
#[test]
fn permission_error_denied_source_preserves_reason() {
    fn inner() -> Result<(), PermissionError> {
        Err(PermissionError::Denied {
            tool: "WriteFile".into(),
            reason: "path escapes workspace".into(),
        })
    }
    fn outer() -> Result<(), deepseeknova_core::DeepseeknovaError> {
        inner()?;
        Ok(())
    }
    let err = outer().unwrap_err();
    use std::error::Error as _;
    let src = err
        .source()
        .expect("Permission 变体应持有 source")
        .downcast_ref::<PermissionError>()
        .expect("source 应可 downcast 回 PermissionError");
    match src {
        PermissionError::Denied { tool, reason } => {
            assert_eq!(tool, "WriteFile");
            assert_eq!(reason, "path escapes workspace");
        }
        other => panic!("期望 Denied，得到 {other:?}"),
    }
}

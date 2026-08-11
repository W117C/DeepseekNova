//! 会话缓存契约（T10，high）回归测试。
//!
//! 覆盖四类缺口：
//! 1. deny 规则优先于缓存决策（缓存不得绕过 deny）；
//! 2. `set_mode` / `set_trusted` 状态切换清空缓存（Auto 批准 → 切 Plan 同一
//!    命令重新裁决为 Ask）；
//! 3. 缓存容量上限 + 逐出（超过 4096 条后淘汰旧条目，缓存不再无限增长）；
//! 4. 缓存键规范化（JSON 空白/键序差异命中同一缓存项）。

use deepseeknova_permission::{Decision, PermissionGate, PermissionMode, Policy, Rule};

/// 带名字与只读标志的最小工具（integration 测试不可复用 src/tests.rs 内部类型）。
struct NamedTool {
    name: &'static str,
    read_only: bool,
}

impl NamedTool {
    fn writer(name: &'static str) -> Self {
        Self {
            name,
            read_only: false,
        }
    }
}

#[async_trait::async_trait]
impl deepseeknova_core::tool::Tool for NamedTool {
    fn schema(&self) -> deepseeknova_core::ToolSchema {
        deepseeknova_core::ToolSchema {
            name: self.name.to_string(),
            description: "integration stub".to_string(),
            parameters: serde_json::Value::Null,
        }
    }

    fn read_only(&self) -> bool {
        self.read_only
    }

    async fn execute(
        &self,
        _ctx: &deepseeknova_core::tool::ToolContext,
        _args: &str,
    ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
        Ok(String::new())
    }
}

/// 无规则、模式回退驱动的 gate（写工具默认裁决由模式预设决定）。
fn bare_gate() -> PermissionGate {
    PermissionGate::new(Policy {
        mode: Decision::Ask,
        allow: vec![],
        ask: vec![],
        deny: vec![],
    })
}

#[test]
fn auto_approve_then_plan_asks_same_command() {
    // 正例：Auto 批准 → 切 Plan（承诺写工具默认询问）→ 同一命令变 Ask。
    // 修复前缓存 Allow 直接返回，Plan 模式下同一命令仍免询问放行。
    let gate = bare_gate().with_mode(Some(PermissionMode::Auto));
    let tool = NamedTool::writer("write_file");

    // Auto：写工具默认放行
    assert_eq!(
        gate.check(&tool, r#"{"path":"/tmp/x"}"#).decision(),
        Decision::Allow
    );

    // 模拟 agent 在 Auto 模式下把批准写回会话缓存
    gate.cache_decision("write_file", r#"{"path":"/tmp/x"}"#, Decision::Allow);

    // 切到 Plan：set_mode 必须清空缓存，同一命令重新裁决为 Ask
    gate.set_mode(Some(PermissionMode::Plan));
    let v = gate.check(&tool, r#"{"path":"/tmp/x"}"#);
    assert_eq!(
        v.decision(),
        Decision::Ask,
        "mode switch Auto→Plan must invalidate cached allow"
    );
}

#[test]
fn deny_rule_beats_cached_allow() {
    // 正例：deny 规则命中优先于缓存 Allow（"deny > ask > allow" 对缓存成立）。
    let gate = PermissionGate::new(Policy {
        mode: Decision::Ask,
        allow: vec![],
        ask: vec![],
        deny: vec![Rule::with_subject("bash", "rm *")],
    });
    let tool = NamedTool::writer("bash");

    // 用户曾批准该写命令 → 缓存 Allow
    gate.cache_decision("bash", r#"{"command":"rm -rf /tmp/x"}"#, Decision::Allow);

    let v = gate.check(&tool, r#"{"command":"rm -rf /tmp/x"}"#);
    assert_eq!(
        v.decision(),
        Decision::Deny,
        "deny rule must beat cached allow"
    );
    assert!(!v.is_hard_deny(), "rule deny is not a hard deny");
}

#[test]
fn deny_rule_beats_cached_ask() {
    // 正例（对称）：缓存 Ask 命中后新增 deny 规则同样优先。
    let gate = PermissionGate::new(Policy {
        mode: Decision::Ask,
        allow: vec![],
        ask: vec![],
        deny: vec![Rule::with_subject("bash", "rm *")],
    });
    let tool = NamedTool::writer("bash");
    gate.cache_decision("bash", r#"{"command":"rm -f x.txt"}"#, Decision::Ask);

    let v = gate.check(&tool, r#"{"command":"rm -f x.txt"}"#);
    assert_eq!(
        v.decision(),
        Decision::Deny,
        "deny rule must beat cached ask"
    );
}

#[test]
fn cache_key_ignores_json_whitespace() {
    // 正例：仅空白差异的写法命中同一缓存项（不再重复弹窗）。
    // 判别器：缓存 Deny，而只读命令重新裁决本会免询问放行——
    // 若 key 未规范化，check 会命中不同 key 并返回 Allow。
    let gate = bare_gate();
    let tool = NamedTool::writer("bash");

    gate.cache_decision("bash", r#"{"command":"ls -la /etc"}"#, Decision::Deny);
    let v = gate.check(&tool, r#"{"command": "ls -la /etc"}"#);
    assert_eq!(
        v.decision(),
        Decision::Deny,
        "whitespace-only difference must hit same cache key"
    );
}

#[test]
fn cache_key_ignores_json_key_order() {
    // 正例：键序差异的写法命中同一缓存项（preserve_order 下需显式排序键）。
    let gate = bare_gate();
    let tool = NamedTool::writer("bash");

    gate.cache_decision(
        "bash",
        r#"{"command":"git status","flag":"--short"}"#,
        Decision::Deny,
    );
    let v = gate.check(&tool, r#"{"flag":"--short","command":"git status"}"#);
    assert_eq!(
        v.decision(),
        Decision::Deny,
        "key-order difference must hit same cache key"
    );
}

#[test]
fn cache_key_normalizes_nested_objects() {
    // 正例：嵌套对象键序 + 空白同时差异仍命中同一缓存项。
    let gate = bare_gate();
    let tool = NamedTool::writer("edit_file");

    gate.cache_decision(
        "edit_file",
        r#"{"path":"a.rs","edits":[{"old":"x","new":"y"}]}"#,
        Decision::Deny,
    );
    let v = gate.check(
        &tool,
        r#"{"edits": [ { "new": "y", "old": "x" } ], "path": "a.rs"}"#,
    );
    assert_eq!(
        v.decision(),
        Decision::Deny,
        "nested key-order/whitespace difference must hit same cache key"
    );
}

#[test]
fn cache_capacity_evicts_one_entry() {
    // 正例：超过容量上限（4096）后逐出一个既有条目，缓存不无限增长。
    // 写工具（mode=Ask、无规则）重新裁决为 Ask，未逐出者命中缓存 Deny。
    let gate = bare_gate();
    let tool = NamedTool::writer("write");
    let capacity = 4096usize;
    let total = capacity + 1;

    let args: Vec<String> = (0..total)
        .map(|i| format!(r#"{{"path":"/tmp/evict_{i}"}}"#))
        .collect();
    for a in &args {
        gate.cache_decision("write", a, Decision::Deny);
    }

    let denies = args
        .iter()
        .filter(|a| gate.check(&tool, a).decision() == Decision::Deny)
        .count();
    let asks = args
        .iter()
        .filter(|a| gate.check(&tool, a).decision() == Decision::Ask)
        .count();
    assert_eq!(
        asks, 1,
        "超过容量后应恰好逐出一个条目（被逐出者重新裁决为 Ask）"
    );
    assert_eq!(denies, capacity, "其余条目仍命中缓存 Deny");
}

#[test]
fn set_trusted_clears_cache() {
    // 正例：信任状态切换同样清空缓存——trusted 下批准写回缓存后切回
    // untrusted，项目层 allow 降级为 Ask，同一命令重新裁决。
    let gate = PermissionGate::new(Policy {
        mode: Decision::Ask,
        allow: vec![Rule::new("write_file")],
        ask: vec![],
        deny: vec![],
    })
    .with_allow_project_scoped(true)
    .with_trusted(true);
    let tool = NamedTool::writer("write_file");

    assert_eq!(gate.check(&tool, "{}").decision(), Decision::Allow);
    gate.cache_decision("write_file", "{}", Decision::Allow);

    gate.set_trusted(false);
    let v = gate.check(&tool, "{}");
    assert_eq!(
        v.decision(),
        Decision::Ask,
        "trust switch must invalidate cached allow (degraded to ask)"
    );
}

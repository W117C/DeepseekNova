# Long-Task Engine B3 — 完成前自审（默认关）· Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** run 收尾发 `Done` 前，若本轮发生过文件写入且 `[review] enabled=true`，用廉价模型审查 `git diff` + 任务文本；发现 issues 回注反馈继续修复（max_cycles=1）；1 轮后仍有 issues → `Paused(reason 前缀 "review_issues")` 交人工。默认关 + 可观测计数器（≥50 次触发后人工评估再翻默认）。

**Architecture:** 方案 A 延续——零新 crate。新建 `agent/review.rs`（纯逻辑全单测：宽松 JSON 解析、prompt 渲染、diff 截断；LLM 调用与 git 子进程隔离成小函数），`Complete` 臂前插审查门（复用 B2 `Paused` 先例），`stream_and_process_turn` 加 `wrote_files: &mut bool` 最小管道，runtime 按 `compact_model` 先例装配 `review_model`，计数器走 MemoryStore counters 表泛化 API（memory 关闭时仅 tracing）。

**基线：** worktree `.worktrees/feat-long-task-b3` @ `8cf3ec6`（=origin/main，B2+模型指针合并树，`make check` 51 目标绿已验）。

---

## File Map

| 文件 | 动作 | 职责 |
|---|---|---|
| `crates/deepseeknova-config/src/lib.rs` | Modify | `[review]` 段（enabled=false 默认） |
| `crates/deepseeknova-core/src/memory/store.rs` | Modify | 泛化 `bump_counter`/`read_counter` |
| `crates/deepseeknova-core/src/memory/engine.rs` | Modify | 透出 `bump_counter`/`read_counter` |
| `crates/deepseeknova-agent/src/review.rs` | Create | 审查模块（git diff 采集、prompt、宽松解析、ReviewSettings） |
| `crates/deepseeknova-agent/src/agent.rs` | Modify | wrote_files 管道 + Complete 臂审查门 + builders |
| `crates/deepseeknova-agent/src/lib.rs` | Modify | `mod review;` |
| `crates/deepseeknova-runtime/src/lib.rs` | Modify | 装配 review provider/settings/计数钩子 |
| `CHANGELOG.md` | Modify | 新特性条目（非 breaking：默认关） |

**关键既有事实（已核，勿再猜）**：`StepOutcome::Complete(output)` 臂在 agent.rs ~L660（`tx.send(Done)` + `return Ok(())`）；`stream_and_process_turn` 已有 `tool_calls_made: &mut usize` 可对照加 `wrote_files: &mut bool`；`RunEvent::Paused` 先例在 budget Reject / max_steps 两处；counters 表 API 先例 `note_recall`/`recall_counters`（INSERT ON CONFLICT）；`compact_model` 装配先例在 runtime（resolve_provider_for_model + create_provider，失败 warn 降级）；CLI 无需改（Paused 已消费）。写类工具名以 `all_builtin_tools` 实际 schema().name 为准核对（预期 `write_file`/`edit_file`/`move_file`/`shell`，实现者 grep 确认）。

**钉死的语义**：
- 触发：`Complete` 臂 && review 已装配 && `wrote_files` && 尚未超过审查轮次。
- 轮次：第 1 次审查 approve→Done；issues→计 issues_found、回注反馈 User 消息、`continue`；修复后再次 `Complete` → 第 2 次审查 approve→计 fix_succeeded + Done；仍 issues→`Paused { reason: format!("review_issues: {前3条issue摘要}"), session_id }`。`max_cycles=1` 即只允许 1 轮修复。
- 降级（全部 → 跳过审查直接 Done + warn）：非 git 仓库 / git 命令失败 / LLM 调用失败 / verdict JSON 解析失败（宽松解析先试 ```json 块、再试首个 `{...}`；`verdict` 字段缺失或非 approve|issues 视为解析失败）。
- 纯问答 run（无文件写入）零触发、零 token；`enabled=false` 行为与现状逐字节一致。

---

## Task 1: config — `[review]` 段

**Files:** Modify `crates/deepseeknova-config/src/lib.rs`

- [ ] **Step 1: 失败测试**（`mod tests`）：

```rust
    #[test]
    fn review_config_defaults_off() {
        let c = Config::default();
        assert!(!c.review.enabled, "review must default OFF per spec");
        assert_eq!(c.review.review_model, "");
        assert_eq!(c.review.diff_cap_tokens, 3000);
        assert_eq!(c.review.max_cycles, 1);
    }

    #[test]
    fn review_config_parses_overrides() {
        let toml = "[review]\nenabled = true\nreview_model = \"deepseek-chat\"\ndiff_cap_tokens = 1500\n";
        let c: Config = toml::from_str(toml).unwrap();
        assert!(c.review.enabled);
        assert_eq!(c.review.review_model, "deepseek-chat");
        assert_eq!(c.review.diff_cap_tokens, 1500);
        assert_eq!(c.review.max_cycles, 1); // 未覆盖取默认
    }
```

- [ ] **Step 2:** `cargo test -p deepseeknova-config --lib review_config` → 期望编译错 `no field 'review'`
- [ ] **Step 3: 实现** — `Config` 加 `#[serde(default)] pub review: ReviewConfig,`（`budget` 字段之后）；Budget 段之后新增（镜像 SessionConfig/BudgetConfig 风格，全部 `///` 文档注释）：

```rust
// ---------------------------------------------------------------------------
// Review（完成前自审，B3）
// ---------------------------------------------------------------------------

/// Pre-completion self-review configuration (default OFF).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewConfig {
    /// Whether the pre-Done review gate runs (default: false — data-driven
    /// flip after ≥50 triggers evaluated manually).
    #[serde(default)]
    pub enabled: bool,

    /// Model for the review verdict. Empty = main provider.
    #[serde(default)]
    pub review_model: String,

    /// Cap (estimated tokens) on the diff excerpt sent to the reviewer.
    #[serde(default = "default_diff_cap")]
    pub diff_cap_tokens: usize,

    /// Fix cycles allowed before pausing for human review (default: 1).
    #[serde(default = "default_review_cycles")]
    pub max_cycles: usize,
}

fn default_diff_cap() -> usize {
    3000
}

fn default_review_cycles() -> usize {
    1
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            review_model: String::new(),
            diff_cap_tokens: default_diff_cap(),
            max_cycles: default_review_cycles(),
        }
    }
}
```

`Config::merge` 在 `self.budget = other.budget;` 后加 `self.review = other.review;`。

- [ ] **Step 4:** `cargo test -p deepseeknova-config` 全绿 + clippy 干净
- [ ] **Step 5:** fmt + 提交 `feat(config): [review] section for pre-completion self-review (default off)`

---

## Task 2: core/memory — 泛化计数器

**Files:** Modify `crates/deepseeknova-core/src/memory/store.rs`、`crates/deepseeknova-core/src/memory/engine.rs`

- [ ] **Step 1: 失败测试**（store.rs `mod tests`）：

```rust
    #[test]
    fn generic_counters_bump_and_read() {
        let store = MemoryStore::open_in_memory().unwrap();
        assert_eq!(store.read_counter("review_triggered").unwrap(), 0);
        store.bump_counter("review_triggered").unwrap();
        store.bump_counter("review_triggered").unwrap();
        assert_eq!(store.read_counter("review_triggered").unwrap(), 2);
    }
```

- [ ] **Step 2: 实现** — store.rs 在 `note_recall` 旁加（同款 INSERT ON CONFLICT / 锁风格）：

```rust
    /// 泛化计数器：任意名字 +1（B3 审查指标等）。
    pub fn bump_counter(&self, name: &str) -> Result<()> {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        db.execute(
            "INSERT INTO counters(name, value) VALUES (?1, 1)
             ON CONFLICT(name) DO UPDATE SET value = value + 1",
            rusqlite::params![name],
        )?;
        Ok(())
    }

    /// 读取泛化计数器（缺失 = 0）。
    pub fn read_counter(&self, name: &str) -> Result<u64> {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let v: i64 = db
            .query_row(
                "SELECT value FROM counters WHERE name = ?1",
                rusqlite::params![name],
                |r| r.get(0),
            )
            .unwrap_or(0);
        Ok(v as u64)
    }
```

engine.rs 透出（`stats` 旁，`///` 注释）：

```rust
    /// 泛化计数器 +1（审查指标 review_triggered/issues_found/fix_succeeded 等）。
    pub fn bump_counter(&self, name: &str) -> Result<()> {
        self.store.bump_counter(name)
    }

    /// 读取泛化计数器（缺失 = 0）。
    pub fn read_counter(&self, name: &str) -> Result<u64> {
        self.store.read_counter(name)
    }
```

- [ ] **Step 3:** `cargo test -p deepseeknova-core --lib memory` 全绿 + clippy 干净（core 禁 unwrap/expect 非测试代码——以上无违例）
- [ ] **Step 4:** fmt + 提交 `feat(core/memory): generic bump/read counters for review metrics`

---

## Task 3: agent — `review.rs` 审查模块

**Files:** Create `crates/deepseeknova-agent/src/review.rs`；Modify `crates/deepseeknova-agent/src/lib.rs`（`mod review;` 字母序，暂 `#[allow(dead_code)] // wired in Task 4`）

设计同 compaction.rs：纯逻辑（渲染/解析/截断）全单测；git 与 LLM 各一个薄函数。

- [ ] **Step 1: 建文件**：

```rust
//! B3 完成前自审：run 收尾发 Done 前，用廉价模型审查本轮 git diff 与任务
//! 文本，宽松解析 {verdict, issues}。非 git / diff 失败 / LLM 失败 / 解析
//! 失败一律优雅降级（跳过审查，warn），绝不阻断 Done。

use deepseeknova_core::{Message, Role};
use deepseeknova_provider::Provider;
use std::path::Path;
use tracing::warn;

/// 审查配置（runtime 从 [review] 装配）。
pub(crate) struct ReviewSettings {
    pub diff_cap_tokens: usize,
    pub max_cycles: usize,
}

/// 审查判定结果。
#[derive(Debug, PartialEq)]
pub(crate) enum Verdict {
    Approve,
    Issues(Vec<String>),
}

/// 采集 git diff（--stat + 正文，正文按 cap 截断）。非 git 仓库或命令失败
/// 返回 None（调用方跳过审查）。
pub(crate) async fn collect_diff(workspace_root: &Path, cap_chars: usize) -> Option<String> {
    let in_repo = tokio::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(workspace_root)
        .output()
        .await
        .ok()?;
    if !in_repo.status.success() {
        return None;
    }
    let stat = git_capture(workspace_root, &["diff", "--stat"]).await?;
    let body = git_capture(workspace_root, &["diff"]).await?;
    if stat.trim().is_empty() && body.trim().is_empty() {
        return None; // 无改动可审
    }
    let capped: String = body.chars().take(cap_chars).collect();
    Some(format!("## diff --stat\n{stat}\n## diff (capped)\n{capped}"))
}

async fn git_capture(root: &Path, args: &[&str]) -> Option<String> {
    let out = tokio::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        warn!("git {args:?} failed during review; skipping");
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 渲染审查 prompt：diff + 任务文本 + 完成声明 → 严格要求 JSON 判定。
pub(crate) fn render_review_prompt(task: &str, completion: &str, diff: &str) -> String {
    format!(
        "You are a strict but fair code reviewer. The agent claims the task is \
         complete. Review the diff against the task. Respond with ONLY a JSON \
         object: {{\"verdict\": \"approve\"}} or \
         {{\"verdict\": \"issues\", \"issues\": [\"...\", \"...\"]}}. \
         List only real, actionable problems (bugs, task requirements not met, \
         broken code); style nits are NOT issues.\n\n\
         # Task\n{task}\n\n# Completion claim\n{completion}\n\n# Diff\n{diff}"
    )
}

/// 宽松解析：先找 ```json 块，再退回首个 {...} 平衡块；verdict 缺失或
/// 非法 → None（调用方按解析失败降级）。
pub(crate) fn parse_verdict(raw: &str) -> Option<Verdict> {
    let json_str = extract_json(raw)?;
    let v: serde_json::Value = serde_json::from_str(&json_str).ok()?;
    match v.get("verdict")?.as_str()? {
        "approve" => Some(Verdict::Approve),
        "issues" => {
            let issues = v
                .get("issues")
                .and_then(|i| i.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(str::to_string))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if issues.is_empty() {
                Some(Verdict::Approve) // issues 判定但清单为空 = 无事可修
            } else {
                Some(Verdict::Issues(issues))
            }
        }
        _ => None,
    }
}

fn extract_json(raw: &str) -> Option<String> {
    if let Some(start) = raw.find("```json") {
        let rest = &raw[start + 7..];
        if let Some(end) = rest.find("```") {
            return Some(rest[..end].trim().to_string());
        }
    }
    // 首个平衡的 {...}
    let bytes = raw.as_bytes();
    let start = raw.find('{')?;
    let mut depth = 0usize;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(raw[start..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// 单次审查 LLM 调用（复用 compaction::summarize 同款 ValidatedRequest 通路）。
pub(crate) async fn ask_reviewer(provider: &dyn Provider, prompt: &str) -> Option<Verdict> {
    let msgs = vec![Message {
        role: Role::User,
        content: prompt.to_string(),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    }];
    let validated = match deepseeknova_provider::ValidatedRequest::new(&msgs, &[]) {
        Ok(v) => v,
        Err(_) => return None,
    };
    match provider.generate(validated).await {
        Ok(out) => parse_verdict(&out.content),
        Err(e) => {
            warn!("review model call failed ({e}); skipping review");
            None
        }
    }
}

/// 把 issues 回注为反馈 User 消息文本。
pub(crate) fn render_feedback(issues: &[String]) -> String {
    let list = issues
        .iter()
        .map(|i| format!("- {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "[Pre-completion review] The reviewer found issues that must be fixed \
         before the task can be considered complete:\n{list}\n\
         Fix these, then finish the task."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_approve() {
        assert_eq!(parse_verdict(r#"{"verdict":"approve"}"#), Some(Verdict::Approve));
    }

    #[test]
    fn parses_fenced_issues() {
        let raw = "Here you go:\n```json\n{\"verdict\":\"issues\",\"issues\":[\"missing test\",\"typo in api\"]}\n```";
        match parse_verdict(raw) {
            Some(Verdict::Issues(v)) => assert_eq!(v, vec!["missing test", "typo in api"]),
            other => panic!("expected issues, got {other:?}"),
        }
    }

    #[test]
    fn issues_verdict_with_empty_list_is_approve() {
        assert_eq!(
            parse_verdict(r#"{"verdict":"issues","issues":[]}"#),
            Some(Verdict::Approve)
        );
    }

    #[test]
    fn garbage_and_unknown_verdict_yield_none() {
        assert_eq!(parse_verdict("not json at all"), None);
        assert_eq!(parse_verdict(r#"{"verdict":"maybe"}"#), None);
        assert_eq!(parse_verdict(r#"{"foo":1}"#), None);
    }

    #[test]
    fn extracts_embedded_json_object() {
        let raw = "prefix {\"verdict\":\"approve\"} suffix";
        assert_eq!(parse_verdict(raw), Some(Verdict::Approve));
    }

    #[test]
    fn prompt_contains_all_sections_and_feedback_lists_issues() {
        let p = render_review_prompt("fix auth", "done", "diff body");
        for s in ["# Task", "# Completion claim", "# Diff", "verdict"] {
            assert!(p.contains(s), "missing {s}");
        }
        let fb = render_feedback(&["a".into(), "b".into()]);
        assert!(fb.contains("- a") && fb.contains("- b"));
    }

    #[tokio::test]
    async fn collect_diff_returns_none_outside_git_repo() {
        let dir = std::env::temp_dir().join(format!(
            "dnv-b3-nogit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(collect_diff(&dir, 4000).await.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
```

> **待核对点**：`Provider::generate(ValidatedRequest)` 返回 `Message`（`.content`），与 compaction.rs::summarize 同款——照抄其调用形态即可。

- [ ] **Step 2:** `cargo test -p deepseeknova-agent --lib review` → 7 测试全过；clippy 干净
- [ ] **Step 3:** fmt + 提交 `feat(agent): review module (diff collection, lenient verdict parse, feedback render)`

---

## Task 4: agent — 主循环审查门 + wrote_files 管道

**Files:** Modify `crates/deepseeknova-agent/src/agent.rs`、`crates/deepseeknova-agent/src/lib.rs`（去掉 review 的 dead_code allow）

- [ ] **Step 1: Agent 字段/builder**（`session_label` 之后）：

```rust
    /// B3 审查：provider + 设置；None = 关闭（默认）。
    review_provider: Option<Arc<dyn Provider>>,
    review_settings: Option<crate::review::ReviewSettings>,

    /// 审查计数钩子（runtime 注入，落 memory counters；None = 仅 tracing）。
    review_counter: Option<ReviewCounterHook>,
```

类型别名（`DistillHook` 旁）+ builders（带 `///`）：

```rust
/// 审查指标计数钩子：name ∈ review_triggered/issues_found/fix_succeeded。
pub type ReviewCounterHook = Arc<dyn Fn(&str) + Send + Sync>;
```

```rust
    /// 启用完成前自审（B3）。provider 为审查模型，settings 含 diff 上限与轮次。
    pub fn with_review(
        mut self,
        provider: Arc<dyn Provider>,
        diff_cap_tokens: usize,
        max_cycles: usize,
    ) -> Self {
        self.review_provider = Some(provider);
        self.review_settings = Some(crate::review::ReviewSettings {
            diff_cap_tokens,
            max_cycles,
        });
        self
    }

    /// 注入审查指标计数钩子。
    pub fn with_review_counter(mut self, hook: ReviewCounterHook) -> Self {
        self.review_counter = Some(hook);
        self
    }
```

`Agent::new` 补三个 None。`run_stream` → `run_agent_loop` 透传（3 个新参数，clone Arc/Option）。

- [ ] **Step 2: wrote_files 管道** — `stream_and_process_turn` 签名在 `tool_calls_made: &mut usize` 后加 `wrote_files: &mut bool`；在工具真正执行成功处（与 `*tool_calls_made += 1` 同点位）加：

```rust
                    // B3：写类工具或 shell 执行过 → 本轮需审查（名字以注册 schema 为准）。
                    if matches!(name.as_str(), "write_file" | "edit_file" | "move_file" | "shell") {
                        *wrote_files = true;
                    }
```

（实现者先 `grep -rn 'name: "' crates/deepseeknova-tools/src` 核对这 4 个名字的真实拼写，以实际为准调整。）`run_agent_loop` 循环前 `let mut wrote_files = false; let mut review_cycles = 0usize;`，调用点传 `&mut wrote_files`。

- [ ] **Step 3: Complete 臂审查门** — 把 `StepOutcome::Complete(output)` 臂替换为：

```rust
            StepOutcome::Complete(output) => {
                // ── B3 完成前自审：有文件写入才触发；降级路径一律放行 Done ──
                if let (Some(rp), Some(rs)) = (&review_provider, &review_settings) {
                    if wrote_files {
                        let bump = |name: &str| {
                            if let Some(ref h) = review_counter {
                                h(name);
                            }
                            info!("review counter: {name}");
                        };
                        match run_review_pass(
                            rp.as_ref(),
                            rs,
                            &workspace_root,
                            &input.prompt,
                            &output.text,
                            &bump,
                            review_cycles == 0,
                        )
                        .await
                        {
                            ReviewOutcome::Approve => {
                                if review_cycles > 0 {
                                    bump("fix_succeeded");
                                }
                            }
                            ReviewOutcome::Issues(issues) if review_cycles < rs.max_cycles => {
                                review_cycles += 1;
                                memory.add_message(Message {
                                    role: Role::User,
                                    content: crate::review::render_feedback(&issues),
                                    name: None,
                                    tool_calls: None,
                                    tool_call_id: None,
                                    reasoning_content: None,
                                });
                                continue; // 回炉修复，下一次 Complete 再审
                            }
                            ReviewOutcome::Issues(issues) => {
                                let head = issues
                                    .iter()
                                    .take(3)
                                    .cloned()
                                    .collect::<Vec<_>>()
                                    .join("; ");
                                tx.send(Ok(RunEvent::Paused {
                                    reason: format!("review_issues: {head}"),
                                    session_id: session_label.clone(),
                                }))
                                .await
                                .ok();
                                return Ok(());
                            }
                            ReviewOutcome::Skipped => {}
                        }
                    }
                }
                tx.send(Ok(RunEvent::Done(output))).await.ok();
                return Ok(());
            }
```

辅助（agent.rs 内、run_agent_loop 之后）：

```rust
/// 审查一轮的三态结果。
enum ReviewOutcome {
    Approve,
    Issues(Vec<String>),
    Skipped,
}

/// 执行一次审查：采集 diff → 问审查模型 → 判定。任何失败 → Skipped。
/// `first_pass` 仅用于 review_triggered 只计首轮。
async fn run_review_pass(
    provider: &dyn Provider,
    settings: &crate::review::ReviewSettings,
    workspace_root: &std::path::Path,
    task: &str,
    completion: &str,
    bump: &dyn Fn(&str),
    first_pass: bool,
) -> ReviewOutcome {
    let cap_chars = settings.diff_cap_tokens.saturating_mul(4);
    let Some(diff) = crate::review::collect_diff(workspace_root, cap_chars).await else {
        warn!("review skipped: no git diff available");
        return ReviewOutcome::Skipped;
    };
    if first_pass {
        bump("review_triggered");
    }
    let prompt = crate::review::render_review_prompt(task, completion, &diff);
    match crate::review::ask_reviewer(provider, &prompt).await {
        Some(crate::review::Verdict::Approve) => ReviewOutcome::Approve,
        Some(crate::review::Verdict::Issues(list)) => {
            bump("issues_found");
            ReviewOutcome::Issues(list)
        }
        None => {
            warn!("review skipped: reviewer verdict unavailable/unparseable");
            ReviewOutcome::Skipped
        }
    }
}
```

- [ ] **Step 4: 循环测试**（agent.rs `mod tests`，沿用既有 MockProvider/looping 构造；审查 provider 单独 mock，天然可控）：

```rust
    #[tokio::test]
    async fn review_disabled_behavior_unchanged() { /* 不设 with_review：写文件工具跑完后照常 Done */ }

    #[tokio::test]
    async fn review_issues_then_fix_leads_to_done() {
        // 主 provider：第1轮 tool call(write类)，第2轮 Done 文本，第3轮 Done 文本
        // 审查 provider：第1次回 issues，第2次回 approve
        // 断言：最终收到 Done；计数钩子记录 review_triggered/issues_found/fix_succeeded 各 1
    }

    #[tokio::test]
    async fn review_persistent_issues_pauses() {
        // 审查 provider 两次都回 issues → 断言收到 Paused 且 reason 以 "review_issues" 开头，无 Done
    }

    #[tokio::test]
    async fn review_skips_outside_git_repo() {
        // workspace_root 指向非 git 临时目录 → 收到 Done（降级），review_triggered 未计
    }
```

> 测试要点：需要一个"能按调用次数返回不同响应"的审查 mock——若既有 MockProvider 只支持单响应重放，为测试模块加一个最小 `SeqProvider`（Vec<String> 依次出队作为 generate 响应）。写类工具触发用既有 SpyTool 改名注册为 `write_file`（或按真实名单）。计数断言用 `Arc<Mutex<Vec<String>>>` 钩子收集。

- [ ] **Step 5:** `cargo test -p deepseeknova-agent` 全绿（既有 + 新 4）+ clippy 干净 + `cargo check --workspace --exclude deepseeknova-desktop` 干净
- [ ] **Step 6:** fmt + 提交 `feat(agent): pre-completion review gate (feedback cycle, pause on persistent issues)`

---

## Task 5: runtime — 装配 review

**Files:** Modify `crates/deepseeknova-runtime/src/lib.rs`

- [ ] **Step 1: 失败测试**：

```rust
    #[test]
    fn build_agent_with_review_enabled_constructs() {
        let mut config = Config::default();
        config.graph.enabled = false;
        config.memory.enabled = false;
        config.review.enabled = true; // review_model 空 → 复用主 provider
        let provider = std::sync::Arc::new(stub_provider());
        let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None, vec![]).unwrap();
        let _ = agent;
    }
```

- [ ] **Step 2: 实现** — B2 装配区之后追加（完全镜像 compact_model 先例；**计数钩子只在 memory 启用时接**——把既有 memory 装配块里的 `handle` clone 一份挂给钩子，实现者按该块实际变量名对齐）：

```rust
    // ── B3 完成前自审（默认关）──
    if config.review.enabled {
        // 审查模型：非空按名解析（同 compact_model 先例），空/失败回退主 provider。
        let review_provider: Arc<dyn deepseeknova_provider::Provider> =
            if config.agent_review_model_nonempty() { /* 实现处直接内联 if !config.review.review_model.is_empty() */
                unreachable!()
            } else {
                provider.clone()
            };
        // ↑ 上行为占位示意；实际实现为：
        // let review_provider = if !config.review.review_model.is_empty() {
        //     match config.resolve_provider_for_model(&config.review.review_model).cloned() {
        //         Some(mut cfg) => { cfg.model = Some(config.review.review_model.clone());
        //             match deepseeknova_provider::factory::create_provider(&cfg) {
        //                 Ok(p) => Arc::from(p),
        //                 Err(e) => { tracing::warn!("review_model '{}' unavailable ({e}); using main provider", config.review.review_model); provider.clone() }
        //             } }
        //         None => { tracing::warn!("review_model '{}' has no matching provider; using main provider", config.review.review_model); provider.clone() }
        //     }
        // } else { provider.clone() };
        agent = agent.with_review(
            review_provider,
            config.review.diff_cap_tokens,
            config.review.max_cycles,
        );
        // 计数钩子：memory 启用时落 counters 表（engine.bump_counter），否则不注入（agent 内 tracing 兜底）。
    }
```

> 实现者注意：上面代码块中的占位示意必须替换为注释里的真实实现（写计划时为避免猜测 `provider` 变量的克隆语义留了展开）；`Arc::from(p)`/`p.into()` 以 compact_model 先例的实际写法为准。计数钩子在 memory 装配块内（拿得到 `handle` 处）追加：
> ```rust
> if config.review.enabled {
>     let ch = handle.clone();
>     agent = agent.with_review_counter(std::sync::Arc::new(move |name: &str| {
>         let _ = ch.bump_counter(name);
>     }));
> }
> ```

- [ ] **Step 3:** `cargo test -p deepseeknova-runtime` 全绿 + clippy 干净
- [ ] **Step 4: CHANGELOG** — Unreleased 的 Added（或新建）加一条：`[review] 完成前自审（默认关）：文件写入后由廉价模型审查 diff，issues 回炉一轮，仍有问题以 Paused(review_issues) 交人工`
- [ ] **Step 5:** fmt + 提交 `feat(runtime): assemble [review] gate (model resolution, counters via memory)`

---

## Task 6: 全量验收

- [ ] `make check` → exit 0
- [ ] `make check-desktop` → exit 0（无桌面改动，回归确认）
- [ ] 冒烟（无 key 则记录跳过）：`[review] enabled=true` 配置下跑一个写文件任务
- [ ] 整体终审（跨切面）

## 验收清单（对照 spec §7）

| spec | 落点 |
|---|---|
| 触发三条件（enabled && 写入 && 未超轮） | T4 Complete 臂门 |
| 廉价模型 + diff_cap + 宽松 JSON | T3 模块（review_model 空=主 provider） |
| issues→回注反馈 continue（max_cycles=1） | T4 门逻辑 |
| 1 轮后仍 issues→Paused(review_issues) | T4 + B2 Paused 先例 |
| 非 git/diff 失败/坏 JSON→跳过+warn | T3/T4 Skipped 路径 |
| 计数器 review_triggered/issues_found/fix_succeeded | T2 泛化 counters + T5 钩子（memory 关→tracing） |
| 默认关、纯问答零触发、enabled=false 零变化 | T1 默认 + T4 wrote_files 门 + 测试 |

**自审记录**：① review_triggered 只计首轮（否则修复轮重复计数污染翻转依据）；② `Issues` 空清单归一为 Approve（宽松解析防噪声）；③ T5 的占位示意块已显式标注"必须替换为注释内真实实现"，非留白。

# deepsec 式安全扫描流水线 P1 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 新建 `deepseeknova-scanner` crate，实现 scan（regex matcher 零 AI）+ process（每 finding 起一次性 agent 调查）+ 报表，并接入 CLI `scan` 子命令。

**Architecture:** scanner crate 只依赖 `deepseeknova-core`（Runner/Provider trait）、`deepseeknova-graph`（`Lang::from_path`）、`regex`/`serde`/`walkdir`/`tokio`/`anyhow`。文件遍历 scanner 自持 walkdir（graph 的 collect_files 是私有，不复用）。`investigate` 接收 `&dyn Runner`（调用方注入一次性 agent），使 scanner 不依赖 runtime/agent。CLI 负责构造 Task 指针 agent 并编排。

**Tech Stack:** Rust workspace 新成员；regex 1.11、walkdir 2.5、serde、tokio（均在 workspace.dependencies）。

**Spec:** `docs/superpowers/specs/2026-07-31-deepsec-scanner-p1-design.md`

**基线：** main @ 832ca90。**关键事实**：graph `collect_files`/`load_gitignore` 私有→scanner 自持遍历；`deepseeknova_graph::parser::Lang::from_path(&str)->Option<Lang>` 公开可复用；`security::path::secure_resolve(root,input)->Result<PathBuf>` 公开；`GrepTool`/`ReadFileTool` 为单元结构体（`pub struct X;`）；Runner trait 有 `run(input)->RunOutput` 便利方法。

**验证约定：** 每任务 TDD + 独立提交；scanner 是新 crate，Task 1 起即须在 `Cargo.toml` workspace members 注册方能 `cargo test -p`。

---

### Task 1: crate 骨架 + rule.rs

**Files:**
- Create: `crates/deepseeknova-scanner/Cargo.toml`
- Create: `crates/deepseeknova-scanner/src/lib.rs`（先仅模块声明）
- Create: `crates/deepseeknova-scanner/src/rule.rs`
- Modify: `Cargo.toml`（根 workspace members 追加）

- [ ] **Step 1: 建 Cargo.toml + workspace 注册**

`crates/deepseeknova-scanner/Cargo.toml`：
```toml
[package]
name = "deepseeknova-scanner"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
deepseeknova-core = { path = "../deepseeknova-core" }
deepseeknova-graph = { path = "../deepseeknova-graph" }
deepseeknova-security = { path = "../deepseeknova-security" }
anyhow = { workspace = true }
regex = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
walkdir = { workspace = true }
tracing = { workspace = true }

[dev-dependencies]
tokio = { workspace = true }
```
根 `Cargo.toml` 的 `[workspace] members` 列表追加 `"crates/deepseeknova-scanner"`（保持字母序/现有风格）。若根 workspace 有 `default-members` 也同步追加。

- [ ] **Step 2: 写失败测试**（rule.rs 底部）

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_rules_all_compile_and_nonempty() {
        let rules = builtin_rules();
        assert!(!rules.is_empty(), "must ship some builtin rules");
        // Regex 已在构造时编译；这里断言每条有非空 id 与 message。
        for r in &rules {
            assert!(!r.id.is_empty());
            assert!(!r.message.is_empty());
        }
    }

    #[test]
    fn hardcoded_secret_rule_matches_and_rejects() {
        let rules = builtin_rules();
        let secret = rules.iter().find(|r| r.id == "hardcoded-secret").unwrap();
        assert!(secret.pattern.is_match(r#"api_key = "sk-abc123""#));
        assert!(!secret.pattern.is_match("let count = 3;"));
    }

    #[test]
    fn rust_unwrap_rule_is_rust_scoped() {
        let rules = builtin_rules();
        let unwrap = rules.iter().find(|r| r.id == "rust-unwrap").unwrap();
        assert_eq!(unwrap.lang, Some(deepseeknova_graph::parser::Lang::Rust));
        assert!(unwrap.pattern.is_match("let x = foo().unwrap();"));
    }
}
```

- [ ] **Step 3: 运行确认失败**

Run: `cargo test -p deepseeknova-scanner rule`
Expected: 编译失败——`Rule`/`Severity`/`builtin_rules` 未定义。

- [ ] **Step 4: 实现 rule.rs**

```rust
//! Scan rules: regex matchers with severity, optional language scope.

use deepseeknova_graph::parser::Lang;
use regex::Regex;
use serde::{Deserialize, Serialize};

/// Finding severity. Ordered high→low for report grouping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Severity {
    High,
    Medium,
    Low,
}

impl Severity {
    /// Stable lowercase label for CLI args / report.
    pub fn label(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }

    /// Parse a CLI severity-min argument (unknown → None).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "high" => Some(Self::High),
            "medium" => Some(Self::Medium),
            "low" => Some(Self::Low),
            _ => None,
        }
    }
}

/// A regex matcher rule. `lang = None` applies to all supported languages.
pub struct Rule {
    pub id: String,
    pub severity: Severity,
    pub lang: Option<Lang>,
    pub pattern: Regex,
    pub message: String,
}

fn rule(id: &str, sev: Severity, lang: Option<Lang>, pat: &str, msg: &str) -> Rule {
    Rule {
        id: id.to_string(),
        severity: sev,
        lang,
        // builtin patterns are constants verified by unit tests; a compile
        // failure here is a build-time bug, surfaced immediately by tests.
        pattern: Regex::new(pat).expect("builtin rule regex must compile"),
        message: msg.to_string(),
    }
}

/// The P1 built-in high-signal rule set. Small and precise — the AI
/// investigation stage (process) adjudicates true/false positives.
pub fn builtin_rules() -> Vec<Rule> {
    vec![
        rule(
            "hardcoded-secret",
            Severity::High,
            None,
            r#"(?i)(api[_-]?key|secret|token|password)\s*[:=]\s*["'][^"']{8,}["']"#,
            "疑似硬编码密钥/凭据",
        ),
        rule(
            "sql-string-interpolation",
            Severity::Medium,
            None,
            r#"(?i)(SELECT|INSERT|UPDATE|DELETE)\s+.*(\{|\+|%s|\$\{)"#,
            "疑似 SQL 字符串拼接（注入面）",
        ),
        rule(
            "command-injection",
            Severity::High,
            None,
            r#"(sh\s+-c|Command::new\([^)]*\)\s*\.arg\([^)]*(\+|format!|\{))"#,
            "疑似命令注入面",
        ),
        rule(
            "rust-unwrap",
            Severity::Low,
            Some(Lang::Rust),
            r#"\.unwrap\(\)|\.expect\(|panic!\("#,
            "非测试路径的 panic 面（unwrap/expect/panic!）",
        ),
    ]
}
```

lib.rs 先写模块声明：
```rust
//! deepseeknova-scanner — deepsec-style security scanning (P1: scan + process).
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod finding;
pub mod report;
pub mod rule;
pub mod scan;
```
注意：`rule.rs` 的 `.expect(...)` 会触发 crate 级 `deny(expect_used)`（非 test）。用 `#[allow(clippy::expect_used)]` 标注 `rule()` 辅助函数并注释理由（builtin 常量正则，编译期 bug 由测试拦截）。investigate 模块在 Task 4 加入声明。

- [ ] **Step 5: 运行确认通过**

Run: `cargo test -p deepseeknova-scanner rule`
Expected: 3 测试 PASS。`cargo clippy -p deepseeknova-scanner --all-targets -- -D warnings` 零警告；`cargo fmt --all`。

- [ ] **Step 6: Commit**

```bash
git add crates/deepseeknova-scanner Cargo.toml Cargo.lock
git commit -m "feat(scanner): crate 骨架与内置 matcher 规则集"
```

---

### Task 2: finding.rs + scan.rs

**Files:**
- Create: `crates/deepseeknova-scanner/src/finding.rs`
- Create: `crates/deepseeknova-scanner/src/scan.rs`

- [ ] **Step 1: 写失败测试**（scan.rs 底部）

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::builtin_rules;

    fn tmp_with(files: &[(&str, &str)]) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("dnv-scan-{}", uid()));
        for (rel, body) in files {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, body).unwrap();
        }
        root
    }
    fn uid() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64
    }

    #[test]
    fn scans_secret_and_reports_line() {
        let root = tmp_with(&[("src/config.rs", "fn a() {}\nlet api_key = \"sk-abcdefgh\";\n")]);
        let findings = scan_files(&root, &builtin_rules()).unwrap();
        let f = findings.iter().find(|f| f.rule_id == "hardcoded-secret").unwrap();
        assert_eq!(f.line, 2, "line is 1-based");
        assert!(f.verdict.is_none(), "scan stage sets no verdict");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn skips_gitignored_and_target() {
        let root = tmp_with(&[
            (".gitignore", "ignored/\n"),
            ("ignored/x.rs", "let secret = \"sk-longenough\";\n"),
            ("target/y.rs", "let secret = \"sk-longenough\";\n"),
        ]);
        let findings = scan_files(&root, &builtin_rules()).unwrap();
        assert!(findings.is_empty(), "gitignored + target excluded");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rust_rule_skips_python_file() {
        let root = tmp_with(&[("a.py", "x = foo().unwrap()\n")]);
        let findings = scan_files(&root, &builtin_rules()).unwrap();
        assert!(!findings.iter().any(|f| f.rule_id == "rust-unwrap"),
            "rust-scoped rule must not fire on .py");
        std::fs::remove_dir_all(&root).ok();
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p deepseeknova-scanner scan`
Expected: 编译失败——`scan_files` / `Finding` 未定义。

- [ ] **Step 3: 实现 finding.rs**

```rust
//! Scan findings and AI verdicts.

use crate::rule::Severity;
use serde::{Deserialize, Serialize};

/// AI investigation verdict for a finding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    pub true_positive: bool,
    pub note: String,
}

/// One matcher hit. `verdict` is filled by the process (AI) stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub rule_id: String,
    pub severity: Severity,
    pub path: String,
    pub line: usize,
    pub excerpt: String,
    pub verdict: Option<Verdict>,
}
```

- [ ] **Step 4: 实现 scan.rs**

```rust
//! File discovery + line-by-line regex matching (zero AI).

use crate::finding::Finding;
use crate::rule::Rule;
use deepseeknova_graph::parser::Lang;
use std::path::Path;
use walkdir::WalkDir;

/// Directory names always excluded from scanning.
const HARD_EXCLUDES: &[&str] = &["target", "node_modules", ".git", "dist"];

/// Max file size scanned (bytes); larger files are skipped.
const MAX_FILE_BYTES: u64 = 1_000_000;

/// Scan every supported source file under `root`, returning findings with no
/// verdict. Unreadable / non-UTF-8 / oversized files are skipped silently.
pub fn scan_files(root: &Path, rules: &[Rule]) -> anyhow::Result<Vec<Finding>> {
    let ignores = load_gitignore(root);
    let mut out = Vec::new();
    for entry in WalkDir::new(root).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if is_excluded(root, path, &ignores) {
            continue;
        }
        let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy().to_string();
        let lang = match Lang::from_path(&rel) {
            Some(l) => l,
            None => continue,
        };
        if entry.metadata().map(|m| m.len()).unwrap_or(0) > MAX_FILE_BYTES {
            continue;
        }
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue, // 权限 / 非 UTF-8 → 跳过
        };
        scan_content(&rel, lang, &content, rules, &mut out);
    }
    Ok(out)
}

fn scan_content(rel: &str, lang: Lang, content: &str, rules: &[Rule], out: &mut Vec<Finding>) {
    for (idx, line) in content.lines().enumerate() {
        for rule in rules {
            if let Some(rl) = rule.lang {
                if rl != lang {
                    continue;
                }
            }
            if rule.pattern.is_match(line) {
                out.push(Finding {
                    rule_id: rule.id.clone(),
                    severity: rule.severity,
                    path: rel.to_string(),
                    line: idx + 1,
                    excerpt: line.trim().chars().take(200).collect(),
                    verdict: None,
                });
            }
        }
    }
}

fn is_excluded(root: &Path, path: &Path, ignores: &[String]) -> bool {
    let rel = path.strip_prefix(root).unwrap_or(path);
    for comp in rel.components() {
        let c = comp.as_os_str().to_string_lossy();
        if HARD_EXCLUDES.contains(&c.as_ref()) {
            return true;
        }
    }
    let rel_str = rel.to_string_lossy();
    ignores.iter().any(|ig| {
        let ig = ig.trim_end_matches('/');
        !ig.is_empty() && (rel_str.starts_with(ig) || rel_str.contains(&format!("/{ig}/")) || rel_str.contains(&format!("{ig}/")))
    })
}

/// Minimal .gitignore reader: non-comment, non-empty lines (prefix match).
fn load_gitignore(root: &Path) -> Vec<String> {
    std::fs::read_to_string(root.join(".gitignore"))
        .map(|s| {
            s.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}
```

- [ ] **Step 5: 运行确认通过**

Run: `cargo test -p deepseeknova-scanner scan`
Expected: 3 测试 PASS。clippy 零警告；fmt。

- [ ] **Step 6: Commit**

```bash
git add crates/deepseeknova-scanner
git commit -m "feat(scanner): Finding 数据结构与 scan_files 遍历匹配"
```

---

### Task 3: report.rs

**Files:**
- Create: `crates/deepseeknova-scanner/src/report.rs`

- [ ] **Step 1: 写失败测试**（report.rs 底部）

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::finding::{Finding, Verdict};
    use crate::rule::Severity;

    fn f(rule: &str, sev: Severity, verdict: Option<bool>) -> Finding {
        Finding {
            rule_id: rule.into(),
            severity: sev,
            path: "a.rs".into(),
            line: 1,
            excerpt: "x".into(),
            verdict: verdict.map(|tp| Verdict { true_positive: tp, note: "n".into() }),
        }
    }

    #[test]
    fn report_groups_by_severity_high_first() {
        let findings = vec![
            f("low1", Severity::Low, None),
            f("high1", Severity::High, Some(true)),
        ];
        let report = ScanReport::new(findings);
        let md = report.to_markdown();
        let hi = md.find("high1").unwrap();
        let lo = md.find("low1").unwrap();
        assert!(hi < lo, "high severity rendered before low");
    }

    #[test]
    fn report_json_roundtrips() {
        let report = ScanReport::new(vec![f("r", Severity::Medium, Some(false))]);
        let json = report.to_json().unwrap();
        assert!(json.contains("\"rule_id\""));
        assert!(json.contains("\"true_positive\""));
    }

    #[test]
    fn report_counts_unmetered_verdicts() {
        let report = ScanReport::new(vec![
            f("a", Severity::Low, None),
            f("b", Severity::Low, Some(true)),
        ]);
        assert_eq!(report.uninvestigated(), 1);
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p deepseeknova-scanner report`
Expected: 编译失败——`ScanReport` 未定义。

- [ ] **Step 3: 实现 report.rs**

```rust
//! Scan report: severity grouping + markdown / JSON rendering.

use crate::finding::Finding;
use crate::rule::Severity;

/// Aggregated scan output.
pub struct ScanReport {
    findings: Vec<Finding>,
}

impl ScanReport {
    /// Build a report, sorting findings by severity (High→Low) then path.
    pub fn new(mut findings: Vec<Finding>) -> Self {
        findings.sort_by(|a, b| {
            a.severity
                .cmp(&b.severity)
                .then_with(|| a.path.cmp(&b.path))
                .then_with(|| a.line.cmp(&b.line))
        });
        Self { findings }
    }

    /// Findings without an AI verdict (skipped or --no-ai).
    pub fn uninvestigated(&self) -> usize {
        self.findings.iter().filter(|f| f.verdict.is_none()).count()
    }

    /// Render as a grouped markdown report.
    pub fn to_markdown(&self) -> String {
        let mut s = String::from("# Scan Report\n\n");
        s.push_str(&format!(
            "{} finding(s), {} uninvestigated\n\n",
            self.findings.len(),
            self.uninvestigated()
        ));
        for sev in [Severity::High, Severity::Medium, Severity::Low] {
            let group: Vec<&Finding> =
                self.findings.iter().filter(|f| f.severity == sev).collect();
            if group.is_empty() {
                continue;
            }
            s.push_str(&format!("## {}\n\n", sev.label()));
            for f in group {
                let verdict = match &f.verdict {
                    Some(v) if v.true_positive => format!(" ✅ TP: {}", v.note),
                    Some(v) => format!(" ⚪ FP: {}", v.note),
                    None => String::new(),
                };
                s.push_str(&format!(
                    "- `{}` {}:{} [{}]{}\n",
                    f.rule_id, f.path, f.line, f.excerpt, verdict
                ));
            }
            s.push('\n');
        }
        s
    }

    /// Render as JSON (array of findings).
    pub fn to_json(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string_pretty(&self.findings)?)
    }

    /// Borrow findings (for CLI iteration in the process stage).
    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }
}
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p deepseeknova-scanner report`
Expected: 3 测试 PASS。clippy 零警告；fmt。

- [ ] **Step 5: Commit**

```bash
git add crates/deepseeknova-scanner
git commit -m "feat(scanner): ScanReport severity 分组与 md/json 渲染"
```


---

### Task 4: investigate.rs（AI 调查阶段）

**Files:**
- Create: `crates/deepseeknova-scanner/src/investigate.rs`
- Modify: `crates/deepseeknova-scanner/src/lib.rs`（加 `pub mod investigate;`）
- Modify: `crates/deepseeknova-scanner/Cargo.toml`（dev-deps 加 `async-trait`、`futures`——测试内造 MockRunner 需要）

- [ ] **Step 1: 写失败测试**（investigate.rs 底部）

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::finding::Finding;
    use crate::rule::Severity;
    use deepseeknova_core::runner::{RunEvent, RunEventStream, RunInput, RunOutput, Runner};

    struct MockRunner {
        reply: String,
    }
    #[async_trait::async_trait]
    impl Runner for MockRunner {
        async fn run_stream(&self, _input: RunInput) -> anyhow::Result<RunEventStream> {
            let out = RunOutput {
                text: self.reply.clone(),
                tool_calls: Vec::new(),
                usage: None,
            };
            let events = vec![Ok(RunEvent::Done(out))];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    fn finding() -> Finding {
        Finding {
            rule_id: "hardcoded-secret".into(),
            severity: Severity::High,
            path: "a.rs".into(),
            line: 2,
            excerpt: "let api_key = \"sk-x\";".into(),
            verdict: None,
        }
    }

    #[tokio::test]
    async fn parses_true_positive_verdict() {
        let runner = MockRunner {
            reply: r#"Here: {"true_positive": true, "note": "real secret"}"#.into(),
        };
        let v = investigate(&finding(), &runner).await;
        assert!(v.is_some());
        let v = v.unwrap();
        assert!(v.true_positive);
        assert_eq!(v.note, "real secret");
    }

    #[tokio::test]
    async fn unparseable_reply_yields_none() {
        let runner = MockRunner {
            reply: "I could not determine anything useful.".into(),
        };
        assert!(investigate(&finding(), &runner).await.is_none());
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p deepseeknova-scanner investigate`
Expected: 编译失败——`investigate` 未定义。

- [ ] **Step 3: 实现 investigate.rs**

```rust
//! AI investigation: adjudicate a finding true/false positive via a one-shot
//! agent run. Lenient JSON extraction — an unparseable reply yields `None`.

use crate::finding::{Finding, Verdict};
use deepseeknova_core::runner::{RunInput, Runner};

/// The investigation prompt template. The runner is expected to have file /
/// grep tools so the model can inspect surrounding code before judging.
fn build_prompt(finding: &Finding) -> String {
    format!(
        "You are a security reviewer. A regex matcher flagged a potential issue.\n\
         Rule: {rule}
File: {path}:{line}
Matched line: {excerpt}

\
         Investigate the surrounding code (read the file / grep as needed) and \
         decide whether this is a real security issue.\n\
         Reply with a single JSON object and nothing else:\n\
         {{\"true_positive\": <bool>, \"note\": \"<one-sentence reason>\"}}",
        rule = finding.rule_id,
        path = finding.path,
        line = finding.line,
        excerpt = finding.excerpt,
    )
}

/// Extract the first balanced `{...}` JSON object containing a
/// `true_positive` key and parse it. Returns `None` on any failure.
fn parse_verdict(reply: &str) -> Option<Verdict> {
    // Scan for candidate `{...}` slices; try to deserialize each.
    let bytes = reply.as_bytes();
    let mut start = None;
    let mut depth = 0i32;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'{' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(s) = start {
                        let slice = &reply[s..=i];
                        if let Ok(v) = serde_json::from_str::<Verdict>(slice) {
                            return Some(v);
                        }
                    }
                    start = None;
                }
            }
            _ => {}
        }
    }
    None
}

/// Investigate one finding. Returns `None` when the run errors or the reply
/// cannot be parsed into a verdict (caller records it as uninvestigated).
pub async fn investigate(finding: &Finding, runner: &dyn Runner) -> Option<Verdict> {
    let input = RunInput {
        prompt: build_prompt(finding),
        images: Vec::new(),
        model_override: None,
    };
    match runner.run(input).await {
        Ok(output) => parse_verdict(&output.text),
        Err(e) => {
            tracing::warn!("investigation of {}:{} failed: {e}", finding.path, finding.line);
            None
        }
    }
}
```

lib.rs 追加 `pub mod investigate;`。Cargo.toml `[dev-dependencies]` 追加 `async-trait = { workspace = true }`、`futures = { workspace = true }`。

注意：`Runner::run` 是 trait 便利方法（收集流为 RunOutput）——确认 core 的 Runner trait 确有 `run` 默认方法；若无则测试的 MockRunner 只实现 `run_stream`，`investigate` 内改为手动收集 `run_stream` 到 `RunEvent::Done`。实现前先 `grep -n "async fn run\b" crates/deepseeknova-core/src/runner.rs` 确认。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p deepseeknova-scanner investigate`
Expected: 2 测试 PASS。clippy 零警告；fmt。

- [ ] **Step 5: Commit**

```bash
git add crates/deepseeknova-scanner
git commit -m "feat(scanner): investigate 一次性 agent 调查与 lenient verdict 解析"
```

---

### Task 5: CLI `scan` 子命令

**Files:**
- Modify: `crates/deepseeknova-cli/src/cli.rs`（Commands 加 Scan 变体）
- Modify: `crates/deepseeknova-cli/src/main.rs`（Scan 分支）
- Modify: `crates/deepseeknova-cli/Cargo.toml`（加 `deepseeknova-scanner` 依赖）

- [ ] **Step 1: cli.rs 加 Scan 变体**

在 `Commands` 枚举内（Run/Plan 附近）追加：
```rust
    /// Scan the codebase for security issues (regex matchers + optional AI investigation).
    Scan {
        /// Root path to scan (default: current directory).
        #[arg(long)]
        path: Option<String>,
        /// Output format: "md" or "json".
        #[arg(long, default_value = "md")]
        format: String,
        /// Skip the AI investigation stage (matcher-only output).
        #[arg(long)]
        no_ai: bool,
        /// Minimum severity to report: high|medium|low.
        #[arg(long, default_value = "low")]
        severity_min: String,
    },
```

- [ ] **Step 2: Cargo.toml 依赖**

`crates/deepseeknova-cli/Cargo.toml` `[dependencies]` 追加：
```toml
deepseeknova-scanner = { path = "../deepseeknova-scanner" }
```

- [ ] **Step 3: main.rs Scan 分支**

在 `match cli.command` 中（Plan 分支之后）追加。使用已构建的 `model_router`：
```rust
        Some(Commands::Scan {
            path,
            format,
            no_ai,
            severity_min,
        }) => {
            use deepseeknova_provider::cost::ModelRole;
            let root = std::path::PathBuf::from(path.as_deref().unwrap_or("."));
            let root = deepseeknova_security::path::secure_resolve(
                &std::env::current_dir().unwrap_or_default(),
                &root,
            )
            .unwrap_or(root);
            let min = deepseeknova_scanner::rule::Severity::parse(severity_min)
                .unwrap_or(deepseeknova_scanner::rule::Severity::Low);

            info!("scan: path={}, format={format}, no_ai={no_ai}", root.display());
            let rules = deepseeknova_scanner::rule::builtin_rules();
            let mut findings = deepseeknova_scanner::scan::scan_files(&root, &rules)?;
            // severity 过滤（High=最小序，min 为下限 → 保留 <= min 序号者）。
            findings.retain(|f| f.severity <= min);

            if !no_ai && !findings.is_empty() {
                let mcp_tools = deepseeknova_runtime::discover_mcp_tools(&config).await;
                let provider = model_router.provider_for(ModelRole::Task, None)?;
                for f in &mut findings {
                    let agent = build_agent(
                        Arc::clone(&provider),
                        deepseeknova_runtime::AgentRoleProviders::default(),
                        None,
                        &config,
                        5,
                        mcp_tools.clone(),
                    )?;
                    f.verdict = deepseeknova_scanner::investigate::investigate(f, &agent).await;
                }
            }

            let report = deepseeknova_scanner::report::ScanReport::new(findings);
            match format.as_str() {
                "json" => println!("{}", report.to_json()?),
                _ => println!("{}", report.to_markdown()),
            }
        }
```
说明：
- severity 过滤用 `<= min`——`Severity` 派生 Ord 且 `High` 声明在前（序最小），"severity_min = medium" 应保留 High+Medium。**实现时务必核对该方向**：若语义相反则改 `>=`。附一条 CLI/单测锚定（见 Step 4）。
- 每 finding 新建 agent（`AgentRoleProviders::default()` — 子代理不需要委派/compact 角色，主 provider 即 Task 指针）。findings 多时这是成本点，spec 已注明 P2 triage 降量。
- process 阶段 token 经 Task 指针的 MeteredProvider 计量；`build_agent` 是 main.rs 现有本地包装。

- [ ] **Step 4: 写 severity 过滤方向锚定测试**（main.rs tests 模块）

```rust
    #[test]
    fn severity_min_filter_direction() {
        use deepseeknova_scanner::rule::Severity;
        // "medium" 下限应保留 High 与 Medium，排除 Low。
        let min = Severity::Medium;
        assert!(Severity::High <= min, "High kept under medium floor");
        assert!(Severity::Medium <= min);
        assert!(!(Severity::Low <= min), "Low excluded under medium floor");
    }
```
（此测试同时验证 `Severity` 的 Ord 方向假设——若失败说明 §Step 3 的 retain 需反向，一并修正。）

- [ ] **Step 5: 验证**

Run:
```bash
cargo test -p deepseeknova-cli severity_min
cargo check -p deepseeknova-cli
cargo clippy -p deepseeknova-cli --all-targets -- -D warnings
cargo fmt --all
```
Expected: 全绿。手工冒烟（无需 API key）：`cargo run -p deepseeknova-cli -- scan --path crates/deepseeknova-scanner --no-ai` 应输出 markdown 报表（scanner 自身的 unwrap 等会命中，验证端到端）。

- [ ] **Step 6: Commit**

```bash
git add crates/deepseeknova-cli
git commit -m "feat(cli): scan 子命令（matcher + 可选 AI 调查 + 报表）"
```

---

### Task 6: 文档与全量回归

**Files:**
- Modify: `GUIDE.md`（工具/命令章节加 scan 说明）
- Create: `crates/deepseeknova-scanner/README.md`（简短 crate 说明，与其他 crate README 风格一致）

- [ ] **Step 1: GUIDE.md 追加**

在命令参考区加：
```markdown
### 安全扫描（deepsec 式，P1）

`deepseeknova scan [--path .] [--format md|json] [--no-ai] [--severity-min low]`

正则 matcher 定位候选点（零 AI），再对每个 finding 起一次性 agent（`task` 指针）
调查判真伪，token 计入 Task 角色。`--no-ai` 只出 matcher 结果。P1 内置高信号规则集
（硬编码密钥、SQL 拼接、命令注入面、Rust panic 面）。
```

- [ ] **Step 2: scanner README.md**

```markdown
# deepseeknova-scanner

deepsec 式安全扫描（P1）：regex matcher 扫描 → 每 finding 一次性 agent 调查 → 报表。

- `rule` — 内置 matcher 规则集
- `scan` — 文件遍历 + 逐行匹配（零 AI）
- `investigate` — 一次性 agent 调查裁定真伪
- `report` — severity 分组 + md/json 渲染

CLI 入口：`deepseeknova scan`。P2 规划：triage / revalidate。
```

- [ ] **Step 3: 全量回归**

Run: `make check`
Expected: fmt + clippy + test + doc 全绿（新 crate 已纳入 workspace，被覆盖）。

- [ ] **Step 4: Commit**

```bash
git add GUIDE.md crates/deepseeknova-scanner/README.md
git commit -m "docs(scanner): scan 子命令与 crate 说明"
```

---

## 自检

- **Spec 覆盖**：crate 边界+模块表（T1-T4 逐文件）、数据流四步（scan=T2、process=T4、report=T3、CLI 编排=T5）、CLI 集成（T5）、错误边界表（scan 跳过 T2 / verdict None T4 / no_ai + 无 provider T5）、内置规则起始集（T1）、测试计划各项（每 Task TDD + T6 make check）、YAGNI 不做项（无任务引入 triage/revalidate/checkpoint）——spec 全节有对应任务。
- **类型一致性**：`Rule{id,severity,lang,pattern,message}`、`Finding{rule_id,severity,path,line,excerpt,verdict}`、`Verdict{true_positive,note}`、`Severity{High,Medium,Low}`、`ScanReport::new/to_markdown/to_json/findings/uninvestigated`、`scan_files(&Path,&[Rule])->Result<Vec<Finding>>`、`investigate(&Finding,&dyn Runner)->Option<Verdict>` 跨任务签名一致。
- **两处必须运行时核对（已在步骤内标注）**：①`Runner::run` 便利方法是否存在（T4 Step 3，不存在则手动收集流）；②`Severity` Ord 方向与 retain `<=` 语义（T5 Step 3+4，锚定测试兜底）。
- **成本提示**：process 每 finding 一次 agent run，findings 多时 token 可观——spec 已记 P2 triage 降量；本期用 `--no-ai` 与 `--severity-min` 作为用户侧节流手段。

//! 写后质量策略（任务质量闭环 A 阶段）。
//!
//! [`QualityPolicy`] 对工具结果文本（diff/输出）与变更路径做策略评估，
//! 产出 [`QualityFinding`] 列表。内置策略 [`QualityPolicy::builtin`] 含三条
//! 规则：`no-commit-secret`（私钥 / AWS key 正则）、`no-forbidden-path`
//! （禁写路径 glob）、`oversized-write`（单次写入体积上限）。

use deepseeknova_core::tool_hook::{FindingSeverity, QualityFinding};
use regex::Regex;
use std::path::{Path, PathBuf};

/// 内置密钥检测正则模式（`no-commit-secret` 规则与诊断报告落盘脱敏共用，
/// 单一事实来源：内置规则按 `(|)` 拼接，diagnose 落盘逐条替换）。
/// 安全审查 S4 扩展：覆盖 PKCS#8 加密私钥、PGP 私钥块、GitHub PAT、
/// Slack token、AWS 临时凭据、Anthropic sk-ant 格式；`sk-` 收紧到 16+
/// 字符（真实 key 一般 20+，短串如 `sk-20240805` 多为版本号/序列号，避免
/// 误伤合法文本）。
pub const SECRET_PATTERNS: &[&str] = &[
    r"-----BEGIN (RSA |EC |OPENSSH |ENCRYPTED )?PRIVATE KEY-----",
    r"-----BEGIN PGP PRIVATE KEY BLOCK-----",
    r"\bAKIA[0-9A-Z]{16}\b",
    // AWS 临时凭据（STS 颁发的 ASIA 前缀）。
    r"\bASIA[0-9A-Z]{16}\b",
    // GitHub Personal Access Token。
    r"\bghp_[A-Za-z0-9]{36}\b",
    // Slack token（xoxb/xoxp/xoxa/xoxr/xoxs 前缀）。
    r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b",
    // Anthropic API key（sk-ant- 前缀）。
    r"\bsk-ant-[A-Za-z0-9-]{16,}\b",
    // OpenAI 风格 API key（与 deepseeknova-scanner 的 hardcoded-secret
    // 规则同源；scanner 规则更激进，此处收敛下限避免误伤）。
    r"\bsk-[A-Za-z0-9]{16,}\b",
];

/// 把文本中的密钥/凭据串替换为 `[REDACTED]`（诊断报告落盘脱敏用；
/// 复用 [`SECRET_PATTERNS`]，替换后不留原文）。
pub fn redact_secrets(s: &str) -> String {
    let mut out = s.to_string();
    for pat in SECRET_PATTERNS {
        let Ok(re) = Regex::new(pat) else {
            continue;
        };
        out = re.replace_all(&out, "[REDACTED]").into_owned();
    }
    out
}

/// 规则执行形态。
pub enum RuleKind {
    /// 正则匹配。`targets` 为文件名后缀白名单（空 = 全部文件/文本均适用）。
    Regex {
        pattern: String,
        targets: Vec<String>,
    },
    /// 禁写路径 glob（相对 workspace）。命中即违规。
    PathGlob { deny: Vec<String> },
    /// 单次写入字节上限。
    SizeLimit { bytes: u64 },
}

/// 一条质量规则。
pub struct QualityRule {
    /// 规则 id（`no-commit-secret` 等）。
    pub id: &'static str,
    /// 命中时的严重级别。
    pub severity: FindingSeverity,
    /// 执行形态。
    pub kind: RuleKind,
    /// 规则说明（诊断/审计用）。
    pub message: &'static str,
}

/// 质量策略：一组规则的有序集合。`evaluate` 按注册顺序逐条评估。
pub struct QualityPolicy {
    rules: Vec<QualityRule>,
}

impl QualityPolicy {
    /// 内置策略：
    /// - `no-commit-secret`（Blocking）：私钥头 `-----BEGIN ... PRIVATE KEY-----`
    ///   与 AWS access key `AKIA[0-9A-Z]{16}`；
    /// - `no-forbidden-path`（Blocking）：禁写 `.env`、`**/*.pem`、`**/id_rsa`、
    ///   `**/id_ed25519`；
    /// - `oversized-write`（Warning）：单次写入超 1 MiB。
    pub fn builtin() -> Self {
        Self {
            rules: vec![
                QualityRule {
                    id: "no-commit-secret",
                    severity: FindingSeverity::Blocking,
                    kind: RuleKind::Regex {
                        pattern: SECRET_PATTERNS.join("|"),
                        targets: Vec::new(),
                    },
                    message: "detected private key or AWS access key material",
                },
                QualityRule {
                    id: "no-forbidden-path",
                    severity: FindingSeverity::Blocking,
                    kind: RuleKind::PathGlob {
                        deny: vec![
                            ".env".to_string(),
                            "**/*.pem".to_string(),
                            "**/id_rsa".to_string(),
                            "**/id_ed25519".to_string(),
                        ],
                    },
                    message: "writing to forbidden paths (.env / key material)",
                },
                QualityRule {
                    id: "oversized-write",
                    severity: FindingSeverity::Warning,
                    kind: RuleKind::SizeLimit { bytes: 1024 * 1024 },
                    message: "single write exceeds 1 MiB",
                },
            ],
        }
    }

    /// 评估一次写操作：`diff` 为结果文本（bash 输出 / 写摘要 / diff 内容），
    /// `changed` 为本次变更涉及的文件路径，`workspace_root` 用于解析相对路径。
    ///
    /// 仅返回违规 finding（`passed: false`）；`passed: true` 仅审计用，
    /// 本阶段不产出。空 diff + 空路径 → 零 finding。
    pub fn evaluate(
        &self,
        diff: &str,
        changed: &[PathBuf],
        workspace_root: &Path,
    ) -> Vec<QualityFinding> {
        let mut findings = Vec::new();
        for rule in &self.rules {
            match &rule.kind {
                RuleKind::Regex { pattern, targets } => {
                    if !targets.is_empty()
                        && !changed.iter().any(|p| {
                            p.file_name()
                                .map(|n| {
                                    let n = n.to_string_lossy();
                                    targets.iter().any(|t| n.ends_with(t.as_str()))
                                })
                                .unwrap_or(false)
                        })
                    {
                        continue;
                    }
                    let Ok(re) = Regex::new(pattern) else {
                        // F8：规则编译失败不再静默跳过；未来规则可配置化后，
                        // 编译失败（拼写错误/非法语法）在运行时可见告警可定位。
                        tracing::warn!(
                            rule_id = rule.id,
                            pattern = %pattern,
                            "quality rule regex failed to compile; rule skipped"
                        );
                        continue;
                    };
                    if let Some(m) = re.find(diff) {
                        let mut evidence = m.as_str().to_string();
                        if evidence.len() > 120 {
                            evidence.truncate(120);
                            evidence.push('…');
                        }
                        findings.push(QualityFinding {
                            rule: rule.id.to_string(),
                            severity: rule.severity,
                            passed: false,
                            evidence,
                        });
                    }
                }
                RuleKind::PathGlob { deny } => {
                    for path in changed {
                        if deny.iter().any(|p| glob_matches(p, path, workspace_root)) {
                            findings.push(QualityFinding {
                                rule: rule.id.to_string(),
                                severity: rule.severity,
                                passed: false,
                                evidence: path.display().to_string(),
                            });
                        }
                    }
                }
                RuleKind::SizeLimit { bytes } => {
                    if (diff.len() as u64) > *bytes {
                        findings.push(QualityFinding {
                            rule: rule.id.to_string(),
                            severity: rule.severity,
                            passed: false,
                            evidence: format!("{} bytes exceeds {} byte limit", diff.len(), bytes),
                        });
                    }
                }
            }
        }
        findings
    }

    /// 供 before 钩子使用：若路径命中任一 PathGlob deny 规则，返回规则 id。
    /// 未命中返回 `None`。
    pub fn denied_path(&self, path: &Path) -> Option<&'static str> {
        for rule in &self.rules {
            if let RuleKind::PathGlob { deny } = &rule.kind {
                if deny.iter().any(|p| glob_matches(p, path, Path::new(""))) {
                    return Some(rule.id);
                }
            }
        }
        None
    }
}

/// glob 匹配：先剥掉 workspace 前缀再匹配；无目录分隔符的简单模式（如
/// `.env`、`*.pem`）额外对文件名做后缀/精确匹配，保证 `**/*.pem` 命中
/// 任意层级、`.env` 命中根目录。
///
/// 大小写处理（F2）：macOS/Windows 文件系统大小写不敏感，`glob::Pattern`
/// 默认大小写敏感会放行 `.ENV` / `KEY.PEM` 等变体。匹配前对**模式与路径
/// 双方** `to_lowercase()` 归一后再匹配（原模式保持原样，仅匹配时归一），
/// 既堵住大写变体绕过，又不影响安全路径的正常不命中。
fn glob_matches(pattern: &str, path: &Path, workspace_root: &Path) -> bool {
    let rel = path.strip_prefix(workspace_root).unwrap_or(path);
    let rel_str = rel.to_string_lossy().replace('\\', "/");
    let pat = match glob::Pattern::new(pattern) {
        Ok(p) => p,
        Err(_) => return false,
    };
    if pat.matches(&rel_str) {
        return true;
    }
    // 无目录分隔符的模式：对文件名做匹配（`*.pem` 命中任意层级）。
    if !pattern.contains('/') {
        if let Some(name) = rel.file_name() {
            if pat.matches(&name.to_string_lossy()) {
                return true;
            }
        }
    }
    // F2：大小写不敏感兜底——模式与路径都小写化后重新走 glob 与文件名匹配。
    let lower_pat = pattern.to_lowercase();
    let lower_rel = rel_str.to_lowercase();
    let lower_glob = match glob::Pattern::new(&lower_pat) {
        Ok(p) => p,
        Err(_) => return false,
    };
    if lower_glob.matches(&lower_rel) {
        return true;
    }
    if !pattern.contains('/') {
        if let Some(name) = rel.file_name() {
            return lower_glob.matches(&name.to_string_lossy().to_lowercase());
        }
    }
    false
}

/// 从 bash 命令文本中启发式提取疑似写入目标路径（F1：bash 写路径零覆盖）。
///
/// 轻量启发式（非 shell 解析器）：
/// - 重定向：`>` / `>>` 后跟的 token（`2>`/`2>>` 等 stderr 重定向排除；
///   `1>` 保留）；token 去除引号后返回；
/// - `tee` 的目标参数（`tee -a file` 的 `file`）；
/// - `cp` / `mv` / `install` 的最后一个非选项参数（去 `-` 开头选项后的目标）；
/// - 管道右侧（`|` 之后）不解析：只取 `|` 前的段；`&&` / `;` 分段各自解析；
/// - 不做变量展开/嵌套引号处理，解析失败返回空。
///
/// 相对路径原样返回（由调用方按 workspace 语义判定）。
pub fn extract_shell_write_paths(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    for seg in cmd
        .split([';', '&'])
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        // 只取 `|` 前的段（保守：不解析管道右侧）。
        let seg = seg.split('|').next().unwrap_or(seg).trim();
        if seg.is_empty() {
            continue;
        }
        // 重定向：`>` / `>>`（含 `1>`）；排除 `2>`/`2>>`。
        let mut rest = seg;
        while let Some(idx) = rest.find('>') {
            let before = rest[..idx].trim_end();
            let after = &rest[idx + 1..];
            let after = after.strip_prefix('>').unwrap_or(after);
            let after = after.trim_start();
            // 重定向前的描述符：数字(如 2)或空；`2>` 排除（stderr 不写文件内容），
            // `1>` / `>` 保留。
            let fd = before
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>();
            let fd_ok = fd.is_empty() || fd == "1";
            if fd_ok {
                if let Some(path) = take_first_token(after) {
                    out.push(path);
                }
            }
            // 继续扫描 `>` 之后剩余部分（多个重定向：`cmd > a > b`）。
            rest = after;
        }
        // 命令名与参数：tokenize 整段，首个 token 为命令名（可带引号），
        // 其后为参数（选项可能与命令同名冲突，故命令判定只看第一个 token）。
        let tokens = split_tokens(seg);
        let Some((first, rest)) = tokens.split_first() else {
            continue;
        };
        let cmd = strip_quotes(first).to_ascii_lowercase();
        match cmd.as_str() {
            "tee" => {
                // tee 的目标参数：跳过 `-a` 等选项后的第一个非选项参数。
                for w in rest {
                    let bare = strip_quotes(w);
                    if !bare.starts_with('-') {
                        out.push(bare);
                        break;
                    }
                }
            }
            "cp" | "mv" | "install" => {
                // 最后一个非 `-` 开头参数为目标。
                if let Some(w) = rest
                    .iter()
                    .rev()
                    .find(|w| !strip_quotes(w).starts_with('-'))
                {
                    out.push(strip_quotes(w));
                }
            }
            _ => {}
        }
    }
    out
}

/// 取字符串首个空白分隔 token，并去除外层引号（`'` / `"`，不成对时原样返回）。
fn take_first_token(s: &str) -> Option<String> {
    let t = s.split_whitespace().next()?;
    Some(strip_quotes(t))
}

/// 去除 token 外层成对引号；不成对或内部引号原样返回（不做嵌套解析）。
fn strip_quotes(t: &str) -> String {
    let bytes = t.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return t[1..t.len() - 1].to_string();
        }
    }
    t.to_string()
}

/// 按空白切分 token，保留引号内容（简单引号感知：引号内空白不切分）。
fn split_tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match quote {
            Some(q) => {
                cur.push(c);
                if c == q {
                    quote = None;
                }
            }
            None => {
                if c == '"' || c == '\'' {
                    quote = Some(c);
                    cur.push(c);
                } else if c.is_whitespace() {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                } else {
                    cur.push(c);
                }
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> QualityPolicy {
        QualityPolicy::builtin()
    }

    #[test]
    fn no_commit_secret_hits_blocking_on_private_key_diff() {
        let diff = "some code\n-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAK...\n";
        let findings = policy().evaluate(diff, &[], Path::new("/workspace"));
        let hit = findings
            .iter()
            .find(|f| f.rule == "no-commit-secret")
            .expect("must find no-commit-secret finding");
        assert_eq!(hit.severity, FindingSeverity::Blocking);
        assert!(!hit.passed);
        assert!(hit.evidence.contains("PRIVATE KEY"));
    }

    #[test]
    fn no_commit_secret_hits_aws_key() {
        let diff = "credentials = AKIAIOSFODNN7EXAMPLE";
        let findings = policy().evaluate(diff, &[], Path::new("/workspace"));
        assert!(
            findings.iter().any(|f| f.rule == "no-commit-secret"),
            "findings: {findings:?}"
        );
    }

    #[test]
    fn no_commit_secret_ignores_benign_text() {
        let findings = policy().evaluate("plain code, no secrets here", &[], Path::new("/w"));
        assert!(findings.is_empty());
    }

    #[test]
    fn forbidden_path_hits_env() {
        let findings = policy().evaluate("ok", &[PathBuf::from(".env")], Path::new("/w"));
        let hit = findings
            .iter()
            .find(|f| f.rule == "no-forbidden-path")
            .expect("must hit no-forbidden-path");
        assert_eq!(hit.severity, FindingSeverity::Blocking);
        assert_eq!(hit.evidence, ".env");
    }

    #[test]
    fn forbidden_path_hits_nested_pem_via_glob() {
        let findings = policy().evaluate(
            "ok",
            &[PathBuf::from("config/keys/server.pem")],
            Path::new("/w"),
        );
        assert!(
            findings.iter().any(|f| f.rule == "no-forbidden-path"),
            "findings: {findings:?}"
        );
    }

    #[test]
    fn forbidden_path_does_not_hit_safe_path() {
        let findings = policy().evaluate("ok", &[PathBuf::from("src/main.rs")], Path::new("/w"));
        assert!(findings.is_empty());
    }

    #[test]
    fn oversized_write_hits_warning() {
        let big = "x".repeat(1024 * 1024 + 1);
        let findings = policy().evaluate(&big, &[], Path::new("/w"));
        let hit = findings
            .iter()
            .find(|f| f.rule == "oversized-write")
            .expect("must hit oversized-write");
        assert_eq!(hit.severity, FindingSeverity::Warning);
        assert!(hit.evidence.contains("bytes exceeds"));
    }

    #[test]
    fn oversized_write_allows_below_limit() {
        let small = "x".repeat(1024);
        let findings = policy().evaluate(&small, &[], Path::new("/w"));
        assert!(!findings.iter().any(|f| f.rule == "oversized-write"));
    }

    #[test]
    fn empty_diff_yields_zero_findings() {
        let findings = policy().evaluate("", &[], Path::new("/w"));
        assert!(findings.is_empty());
    }

    #[test]
    fn denied_path_returns_rule_id() {
        let p = policy();
        assert_eq!(p.denied_path(Path::new(".env")), Some("no-forbidden-path"));
        assert_eq!(
            p.denied_path(Path::new("secrets/id_rsa")),
            Some("no-forbidden-path")
        );
        assert_eq!(p.denied_path(Path::new("src/lib.rs")), None);
    }

    // -----------------------------------------------------------------------
    // F2：禁写 glob 大小写不敏感（macOS/Windows 文件系统大小写不敏感）
    // -----------------------------------------------------------------------

    #[test]
    fn forbidden_path_hits_uppercase_variants() {
        // `.ENV` / `KEY.PEM` / `ID_RSA` 大写变体必须命中 deny（大小写归一）。
        for p in [
            ".ENV",
            "secrets/ID_RSA",
            "config/keys/KEY.PEM",
            "ID_ED25519",
        ] {
            let findings = policy().evaluate("ok", &[PathBuf::from(p)], Path::new("/w"));
            assert!(
                findings.iter().any(|f| f.rule == "no-forbidden-path"),
                "uppercase variant {p} must hit no-forbidden-path, findings: {findings:?}"
            );
        }
        // denied_path 同样大小写归一。
        assert_eq!(
            policy().denied_path(Path::new(".ENV")),
            Some("no-forbidden-path")
        );
        assert_eq!(
            policy().denied_path(Path::new("KEYS/ID_RSA")),
            Some("no-forbidden-path")
        );
    }

    #[test]
    fn forbidden_path_still_hits_lowercase_and_ignores_safe_paths() {
        // 原小写路径仍命中。
        assert_eq!(
            policy().denied_path(Path::new(".env")),
            Some("no-forbidden-path")
        );
        // 安全路径（含大小写变体）仍不命中。
        assert_eq!(policy().denied_path(Path::new("src/Main.RS")), None);
        assert_eq!(policy().denied_path(Path::new("README.MD")), None);
        let findings = policy().evaluate("ok", &[PathBuf::from("src/MAIN.RS")], Path::new("/w"));
        assert!(
            findings.is_empty(),
            "safe uppercase path must not hit: {findings:?}"
        );
    }

    // -----------------------------------------------------------------------
    // F1：bash 写路径启发式提取
    // -----------------------------------------------------------------------

    #[test]
    fn extract_shell_write_paths_redirects() {
        assert_eq!(extract_shell_write_paths("echo x > .env"), vec![".env"]);
        assert_eq!(
            extract_shell_write_paths("echo x >> logs/app.log"),
            vec!["logs/app.log"]
        );
        assert_eq!(
            extract_shell_write_paths("echo x 1>out.txt"),
            vec!["out.txt"]
        );
        // stderr 重定向排除。
        assert!(extract_shell_write_paths("ls 2>/dev/null").is_empty());
        assert!(extract_shell_write_paths("ls 2>>err.log").is_empty());
        // 带引号的 token 去引号。
        assert_eq!(extract_shell_write_paths("echo x > \".env\""), vec![".env"]);
        assert_eq!(extract_shell_write_paths("echo x > '.env'"), vec![".env"]);
    }

    #[test]
    fn extract_shell_write_paths_tee_cp_mv_install() {
        assert_eq!(
            extract_shell_write_paths("tee -a notes.txt"),
            vec!["notes.txt"]
        );
        // 管道右侧不解析：tee 在 `|` 右侧时不被提取。
        assert!(extract_shell_write_paths("echo hi | tee out.txt").is_empty());
        assert_eq!(extract_shell_write_paths("cp a.txt x.pem"), vec!["x.pem"]);
        assert_eq!(
            extract_shell_write_paths("cp ~/.ssh/id_rsa x.pem"),
            vec!["x.pem"]
        );
        assert_eq!(
            extract_shell_write_paths("mv -f old.rs new.rs"),
            vec!["new.rs"]
        );
        assert_eq!(
            extract_shell_write_paths("install -m 644 src target/key.pem"),
            vec!["target/key.pem"]
        );
    }

    #[test]
    fn extract_shell_write_paths_pipeline_and_segments() {
        // 管道右侧不解析。
        assert!(extract_shell_write_paths("cat a | tee x.pem").is_empty());
        // `&&` / `;` 分段各自解析。
        assert_eq!(
            extract_shell_write_paths("echo a > .env && cp b x.pem"),
            vec![".env", "x.pem"]
        );
        assert_eq!(
            extract_shell_write_paths("echo a > .env; cp b c.pem"),
            vec![".env", "c.pem"]
        );
    }

    #[test]
    fn extract_shell_write_paths_no_write_and_rel_paths() {
        assert!(extract_shell_write_paths("ls -la").is_empty());
        assert!(extract_shell_write_paths("echo hi").is_empty());
        // 相对路径原样返回。
        assert_eq!(
            extract_shell_write_paths("echo x > ./dir/.env"),
            vec!["./dir/.env"]
        );
    }

    // -----------------------------------------------------------------------
    // F6：诊断脱敏复用（redact_secrets）
    // -----------------------------------------------------------------------

    #[test]
    fn redact_secrets_replaces_key_material() {
        assert_eq!(
            redact_secrets("key is -----BEGIN RSA PRIVATE KEY----- here"),
            "key is [REDACTED] here"
        );
        assert_eq!(
            redact_secrets("creds = AKIAIOSFODNN7EXAMPLE"),
            "creds = [REDACTED]"
        );
        // sk- 前缀 API key（OpenAI/Anthropic 风格；16+ 字符才命中，避免
        // 误伤 `sk-20240805` 这类短串）。
        assert_eq!(
            redact_secrets("api key sk-abcdef12345678901234 leaked"),
            "api key [REDACTED] leaked"
        );
        // 短串不误伤。
        assert_eq!(
            redact_secrets("version sk-20240805 ready"),
            "version sk-20240805 ready"
        );
        // 新增格式：GitHub PAT / Anthropic sk-ant / 加密私钥。
        assert_eq!(
            redact_secrets("token ghp_abcdefghijklmnopqrstuvwxyz0123456789 here"),
            "token [REDACTED] here"
        );
        assert_eq!(
            redact_secrets("key sk-ant-api03-abcdefghijklmnopqrstuvwxyz123456"),
            "key [REDACTED]"
        );
        assert_eq!(
            redact_secrets("-----BEGIN ENCRYPTED PRIVATE KEY-----"),
            "[REDACTED]"
        );
        // 明文保留。
        assert_eq!(redact_secrets("plain text"), "plain text");
    }
}

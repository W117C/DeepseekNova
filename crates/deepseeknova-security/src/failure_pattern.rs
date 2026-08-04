//! Failure-pattern clustering and feedback-injection support.
//!
//! Clusters failure observations by a normalized key (phase + tool + hash of
//! a normalized error summary) and persists the clusters as JSON, so later
//! sessions can be pre-warned about known failure modes (design D, spec
//! `docs/superpowers/specs/2026-08-05-protocol-enhancement-design.md` §6).

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::quality::redact_secrets;

/// Maximum number of characters of the raw error used for clustering.
const NORMALIZE_MAX_CHARS: usize = 64;
/// Maximum number of characters kept for the error-summary fallback in
/// [`FailurePatternStore::suggest`] output.
const SUMMARY_MAX_CHARS: usize = 120;
/// Maximum number of patterns kept in the store; beyond this the
/// lowest-count patterns are evicted.
const MAX_PATTERNS: usize = 200;
/// Upper bound for [`FailurePatternStore::suggest`] results (prompt-size
/// guard; callers are expected to pass at most 3).
const SUGGEST_MAX: usize = 3;

/// One clustered failure pattern.
///
/// `key` is produced by [`cluster_key`]; `lesson` carries the most recent
/// lesson (root cause / fix plan) observed for this cluster.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailurePattern {
    /// Clustering key (`{phase}::{tool|"none"}::{hash}`).
    pub key: String,
    /// Protocol phase the failure occurred in (e.g. "execute", "verify").
    pub phase: String,
    /// Tool family that failed, when known.
    pub tool: Option<String>,
    /// Number of times this pattern has been observed.
    pub count: u32,
    /// Unix timestamp (milliseconds) of the most recent observation.
    pub last_seen_ms: u64,
    /// Most recent lesson (root cause / fix plan) for this cluster.
    pub lesson: Option<String>,
}

/// On-disk JSON shape of the store.
#[derive(Debug, Serialize, Deserialize)]
struct FailurePatternFile {
    /// Patterns keyed by their clustering key.
    #[serde(default)]
    patterns: HashMap<String, FailurePattern>,
    /// Truncated raw-error summaries keyed by pattern key; used as the
    /// suggestion fallback when no lesson is recorded.
    #[serde(default)]
    summaries: HashMap<String, String>,
}

/// In-memory failure pattern store with JSON persistence.
///
/// Data is loaded from / saved to the path injected by the caller
/// (`.deepseeknova/security/failure-patterns.json`). Missing or corrupt
/// files load as an empty store (with a warning). At most `MAX_PATTERNS`
/// patterns are kept; excess patterns are evicted by ascending `count`.
pub struct FailurePatternStore {
    path: PathBuf,
    patterns: HashMap<String, FailurePattern>,
    summaries: HashMap<String, String>,
}

impl FailurePatternStore {
    /// Load a store from `path`.
    ///
    /// A missing file yields an empty store. A file that exists but cannot
    /// be read or parsed also yields an empty store (a warning is logged)
    /// rather than failing the session.
    pub fn load(path: &Path) -> Result<Self> {
        let mut store = Self {
            path: path.to_path_buf(),
            patterns: HashMap::new(),
            summaries: HashMap::new(),
        };
        if !path.exists() {
            return Ok(store);
        }
        match std::fs::read_to_string(path) {
            Ok(contents) => match serde_json::from_str::<FailurePatternFile>(&contents) {
                Ok(file) => {
                    store.patterns = file.patterns;
                    store.summaries = file.summaries;
                    store.evict();
                    Ok(store)
                }
                Err(err) => {
                    warn!(
                        path = %path.display(),
                        error = %err,
                        "failure pattern store is corrupt; starting empty"
                    );
                    Ok(store)
                }
            },
            Err(err) => {
                warn!(
                    path = %path.display(),
                    error = %err,
                    "failed to read failure pattern store; starting empty"
                );
                Ok(store)
            }
        }
    }

    /// Record one failure observation.
    ///
    /// Observations are clustered with [`cluster_key`]: an existing pattern
    /// has its `count` incremented and `last_seen_ms` bumped, and `lesson`
    /// is overwritten when a new one is supplied (a `None` lesson never
    /// clears an existing one); otherwise a new pattern with `count == 1`
    /// is inserted. May evict the lowest-count pattern when the store is
    /// over capacity.
    ///
    /// `error` / `lesson` are redacted with [`redact_secrets`] **at entry**
    /// (before clustering / storage), so nothing secret ever reaches the
    /// in-memory store, the on-disk file, or [`Self::suggest`] output
    /// (spec §6.2: 回灌内容脱敏；对齐 diagnose.rs 先例)。Cluster keys are
    /// derived from the redacted error, so the same failure with different
    /// secret material normalizes into one cluster instead of fragmenting.
    pub fn ingest(
        &mut self,
        phase: &str,
        tool: Option<&str>,
        error: &str,
        lesson: Option<&str>,
        now_ms: u64,
    ) {
        let error = redact_secrets(error);
        let lesson = lesson.map(redact_secrets);
        let key = cluster_key(phase, tool, &error);
        let summary: String = error.chars().take(SUMMARY_MAX_CHARS).collect();
        match self.patterns.get_mut(&key) {
            Some(pattern) => {
                pattern.count = pattern.count.saturating_add(1);
                pattern.last_seen_ms = now_ms;
                if let Some(lesson) = lesson {
                    pattern.lesson = Some(lesson);
                }
            }
            None => {
                let pattern = FailurePattern {
                    key: key.clone(),
                    phase: phase.to_string(),
                    tool: tool.map(str::to_string),
                    count: 1,
                    last_seen_ms: now_ms,
                    lesson,
                };
                self.patterns.insert(key.clone(), pattern);
                self.evict();
            }
        }
        self.summaries.insert(key, summary);
    }

    /// Produce feedback suggestions for the next session.
    ///
    /// Returns at most `min(limit, [`SUGGEST_MAX`])` entries, sorted by
    /// `count` descending (most frequent first), formatted as
    /// `[失败模式] {phase}/{tool}: {lesson or error summary}`.
    ///
    /// 输出内容已在 [`Self::ingest`] 入口脱敏（error/lesson 均过
    /// [`redact_secrets`]），此处无需二次处理。
    pub fn suggest(&self, limit: usize) -> Vec<String> {
        let limit = limit.min(SUGGEST_MAX);
        if limit == 0 || self.patterns.is_empty() {
            return Vec::new();
        }
        let mut entries: Vec<&FailurePattern> = self.patterns.values().collect();
        entries.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| b.last_seen_ms.cmp(&a.last_seen_ms))
        });
        entries
            .into_iter()
            .take(limit)
            .map(|pattern| {
                let tool = pattern.tool.as_deref().unwrap_or("none");
                let detail = pattern
                    .lesson
                    .clone()
                    .or_else(|| self.summaries.get(&pattern.key).cloned())
                    .unwrap_or_else(|| "no lesson recorded".to_string());
                format!("[失败模式] {}/{}: {}", pattern.phase, tool, detail)
            })
            .collect()
    }

    /// Atomically persist the store to the configured path.
    ///
    /// Writes a temporary file in the same directory, then renames it over
    /// the target. Creates parent directories if missing.
    ///
    /// 临时文件名带进程 PID 与纳秒时间戳：多进程（serve 多会话 / CLI+serve
    /// 双进程共享工作区）并发写同一路径时 tmp 文件互不踩踏，rename 仍是
    /// 原子替换。已知限制：进程级并发写会丢更新（后写者覆盖整文件，写前
    /// 不 re-load 合并；单进程内由调用方串行保证）。刻意不引入文件锁依赖
    /// （不加 fs2/flock）。Unix 下写后把权限收敛到 0600（对齐 diagnose.rs
    /// 先例：报告内容可能含命令与错误细节）。
    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let file = FailurePatternFile {
            patterns: self.patterns.clone(),
            summaries: self.summaries.clone(),
        };
        let json = serde_json::to_string_pretty(&file)
            .context("failed to serialize failure pattern store")?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let tmp = PathBuf::from(format!(
            "{}.{}.{}.tmp",
            self.path.display(),
            std::process::id(),
            nanos
        ));
        std::fs::write(&tmp, json).with_context(|| format!("failed to write {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path).with_context(|| {
            format!(
                "failed to rename {} to {}",
                tmp.display(),
                self.path.display()
            )
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // 显式收敛 0600：默认 umask 可能放宽（对齐 diagnose.rs 先例）。
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))
                .with_context(|| format!("failed to chmod 0600 {}", self.path.display()))?;
        }
        Ok(())
    }

    /// Evict the lowest-count patterns while over capacity.
    ///
    /// Ties are broken by `last_seen_ms` ascending (oldest first).
    fn evict(&mut self) {
        while self.patterns.len() > MAX_PATTERNS {
            let victim = self
                .patterns
                .iter()
                .min_by(|(_, a), (_, b)| {
                    a.count
                        .cmp(&b.count)
                        .then_with(|| a.last_seen_ms.cmp(&b.last_seen_ms))
                })
                .map(|(key, _)| key.clone());
            if let Some(key) = victim {
                self.patterns.remove(&key);
                self.summaries.remove(&key);
            } else {
                break;
            }
        }
    }

    /// Number of patterns held (test helper).
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.patterns.len()
    }

    /// Look up a pattern by clustering key (test helper).
    #[cfg(test)]
    pub(crate) fn get(&self, key: &str) -> Option<&FailurePattern> {
        self.patterns.get(key)
    }
}

/// Normalize an error string for clustering.
///
/// Keeps the first [`NORMALIZE_MAX_CHARS`] characters, removes whitespace,
/// replaces ISO timestamps, trailing `:digits` / `line digits` markers and
/// bare digit runs with `N`, and lowercases the result.
fn normalize_error(err: &str) -> String {
    let mut normalized: String = err.chars().take(NORMALIZE_MAX_CHARS).collect();
    normalized.retain(|c| !c.is_whitespace());
    normalized = timestamp_re().replace_all(&normalized, "N").into_owned();
    normalized = line_re().replace_all(&normalized, "N").into_owned();
    normalized = digits_re().replace_all(&normalized, "N").into_owned();
    normalized.to_lowercase()
}

/// Compiled regex: ISO-8601 timestamps (`2026-08-05T10:00:00.123Z`).
fn timestamp_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\d{4}-\d{2}-\d{2}T[\d:.]+Z?").expect("valid timestamp regex"))
}

/// Compiled regex: trailing `:digits` or `line digits` markers.
fn line_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r":\d+$|line\d+").expect("valid line regex"))
}

/// Compiled regex: bare digit runs.
fn digits_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\d+").expect("valid digits regex"))
}

/// Compute the clustering key for one failure observation.
///
/// `phase` and `tool` are joined verbatim (a missing tool becomes `"none"`);
/// the error is `normalize_error`d, hashed with
/// [`std::hash::DefaultHasher`], and the first 12 hex characters of the
/// digest are appended: `{phase}::{tool|"none"}::{hash}`.
pub fn cluster_key(phase: &str, tool: Option<&str>, error: &str) -> String {
    let mut hasher = DefaultHasher::new();
    normalize_error(error).hash(&mut hasher);
    let digest = format!("{:x}", hasher.finish());
    let short: String = digest.chars().take(12).collect();
    format!("{}::{}::{}", phase, tool.unwrap_or("none"), short)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Unique path under `std::env::temp_dir()` (pid + timestamp + counter).
    fn unique_temp_path() -> PathBuf {
        let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "failure-patterns-test-{}-{}-{}.json",
            std::process::id(),
            nanos,
            n
        ))
    }

    /// Errors that stay distinct after normalization: a fixed-width letter
    /// label at the front keeps every string well under 64 chars and unique
    /// for `i < 676` (digits would be normalized away, so letters only).
    fn distinct_error(i: usize) -> String {
        let tens = char::from(b'a' + (i / 26) as u8);
        let ones = char::from(b'a' + (i % 26) as u8);
        format!("{tens}{ones} failure")
    }

    #[test]
    fn cluster_key_normalizes_timestamps_lines_and_digits() {
        // Different timestamp, line number and exit code -> same key.
        let a = "ls: no such file 2026-08-05T10:00:00Z line 42";
        let b = "ls: no such file 2026-08-05T11:30:00Z line 7";
        assert_eq!(
            cluster_key("execute", Some("bash"), a),
            cluster_key("execute", Some("bash"), b)
        );
        // Bare digit runs -> same key.
        let c = "cache miss 3 times";
        let d = "cache miss 17 times";
        assert_eq!(
            cluster_key("execute", Some("bash"), c),
            cluster_key("execute", Some("bash"), d)
        );
        // Trailing `:digits` -> same key.
        let e = "error in foo.rs:42";
        let f = "error in foo.rs:7";
        assert_eq!(
            cluster_key("execute", Some("bash"), e),
            cluster_key("execute", Some("bash"), f)
        );
        // Truly different errors -> different keys.
        assert_ne!(
            cluster_key("execute", Some("bash"), "permission denied"),
            cluster_key("execute", Some("bash"), "connection refused")
        );
    }

    #[test]
    fn cluster_key_distinguishes_phase_and_tool() {
        let err = "command failed";
        assert_ne!(
            cluster_key("execute", Some("bash"), err),
            cluster_key("verify", Some("bash"), err)
        );
        assert_ne!(
            cluster_key("execute", Some("bash"), err),
            cluster_key("execute", Some("fs"), err)
        );
        assert_ne!(
            cluster_key("execute", Some("bash"), err),
            cluster_key("execute", None, err)
        );
        // Deterministic for identical inputs.
        assert_eq!(
            cluster_key("execute", Some("bash"), err),
            cluster_key("execute", Some("bash"), err)
        );
    }

    #[test]
    fn ingest_clusters_repeated_errors() {
        let mut store =
            FailurePatternStore::load(Path::new("/nonexistent/failure-patterns.json")).unwrap();
        let err = "bash: command not found at 2026-08-05T10:00:00Z";
        store.ingest("execute", Some("bash"), err, None, 100);
        store.ingest("execute", Some("bash"), err, None, 200);
        assert_eq!(store.len(), 1);
        let key = cluster_key("execute", Some("bash"), err);
        let pattern = store.get(&key).expect("pattern present");
        assert_eq!(pattern.count, 2);
        assert_eq!(pattern.last_seen_ms, 200);
        // A different error creates a new entry.
        store.ingest("execute", Some("bash"), "a different failure", None, 300);
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn suggest_sorts_by_count_and_respects_limit() {
        let mut store =
            FailurePatternStore::load(Path::new("/nonexistent/failure-patterns.json")).unwrap();
        store.ingest("execute", Some("bash"), "top error", None, 100);
        store.ingest("execute", Some("bash"), "top error", None, 200);
        store.ingest("execute", Some("bash"), "top error", None, 300);
        store.ingest("execute", Some("bash"), "mid error", None, 400);
        store.ingest("execute", Some("bash"), "mid error", None, 500);
        store.ingest("execute", Some("bash"), "low error", None, 600);

        let top_two = store.suggest(2);
        assert_eq!(top_two.len(), 2);
        assert!(top_two[0].contains("top error"));
        assert!(top_two[1].contains("mid error"));

        // limit 0 -> empty even with patterns present.
        assert!(store.suggest(0).is_empty());
        // limit above the cap is clamped to 3.
        assert_eq!(store.suggest(100).len(), 3);
    }

    #[test]
    fn suggest_prefers_lesson_over_error_summary() {
        let mut store =
            FailurePatternStore::load(Path::new("/nonexistent/failure-patterns.json")).unwrap();
        store.ingest(
            "execute",
            Some("bash"),
            "boom at 2026-08-05T10:00:00Z",
            Some("check flags before running"),
            100,
        );
        store.ingest("execute", Some("bash"), "plain failure", None, 200);

        let suggestions = store.suggest(3);
        assert!(suggestions
            .iter()
            .any(|s| s.contains("[失败模式] execute/bash: check flags before running")));
        assert!(suggestions
            .iter()
            .any(|s| s.contains("[失败模式] execute/bash: plain failure")));
    }

    #[test]
    fn capacity_evicts_lowest_count_patterns() {
        let mut store =
            FailurePatternStore::load(Path::new("/nonexistent/failure-patterns.json")).unwrap();
        // 201 distinct errors, each seen once (over the 200 limit).
        for i in 0..201usize {
            store.ingest("execute", None, &distinct_error(i), None, i as u64);
        }
        // Re-see err_0 a few times so it becomes the high-count survivor.
        for k in 0..3usize {
            store.ingest("execute", None, &distinct_error(0), None, 300 + k as u64);
        }
        assert_eq!(store.len(), 200);
        let survivor = cluster_key("execute", None, &distinct_error(0));
        let kept = store
            .get(&survivor)
            .expect("high-count pattern must survive eviction");
        assert_eq!(kept.count, 3);
        // A lowest-count, earliest-seen pattern was evicted instead.
        let evicted = cluster_key("execute", None, &distinct_error(1));
        assert!(store.get(&evicted).is_none());
    }

    #[test]
    fn save_then_load_roundtrip() {
        let path = unique_temp_path();
        {
            let mut store = FailurePatternStore::load(&path).unwrap();
            store.ingest(
                "execute",
                Some("bash"),
                "boom at 2026-08-05T10:00:00Z",
                Some("check flags first"),
                100,
            );
            store.ingest(
                "execute",
                Some("bash"),
                "boom at 2026-08-05T10:00:00Z",
                None,
                200,
            );
            store.ingest("verify", None, "mismatch", None, 300);
            store.save().unwrap();
        }
        let loaded = FailurePatternStore::load(&path).unwrap();
        assert_eq!(loaded.len(), 2);
        let key = cluster_key("execute", Some("bash"), "boom at 2026-08-05T10:00:00Z");
        let pattern = loaded.get(&key).expect("pattern present after reload");
        assert_eq!(pattern.count, 2);
        assert_eq!(pattern.last_seen_ms, 200);
        assert_eq!(pattern.lesson.as_deref(), Some("check flags first"));
        // Suggestion text survives the roundtrip (lesson and error summary).
        let suggestions = loaded.suggest(3);
        assert!(suggestions.iter().any(|s| s.contains("check flags first")));
        assert!(suggestions.iter().any(|s| s.contains("mismatch")));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ingest_overwrites_lesson_with_new_value() {
        let mut store =
            FailurePatternStore::load(Path::new("/nonexistent/failure-patterns.json")).unwrap();
        let err = "boom";
        store.ingest("execute", Some("bash"), err, Some("old lesson"), 100);
        store.ingest("execute", Some("bash"), err, Some("new lesson"), 200);
        let key = cluster_key("execute", Some("bash"), err);
        assert_eq!(
            store.get(&key).unwrap().lesson.as_deref(),
            Some("new lesson")
        );
        // A `None` lesson does not clear an existing one.
        store.ingest("execute", Some("bash"), err, None, 300);
        assert_eq!(
            store.get(&key).unwrap().lesson.as_deref(),
            Some("new lesson")
        );
    }

    #[test]
    fn load_missing_or_corrupt_file_yields_empty_store() {
        let missing = unique_temp_path();
        let store = FailurePatternStore::load(&missing).unwrap();
        assert_eq!(store.len(), 0);

        let corrupt = unique_temp_path();
        std::fs::write(&corrupt, "{ not json").unwrap();
        let store = FailurePatternStore::load(&corrupt).unwrap();
        assert_eq!(store.len(), 0);
        let _ = std::fs::remove_file(&corrupt);
    }

    #[test]
    fn ingest_redacts_secrets_before_storing() {
        // 含密钥串的 error/lesson 在 ingest 入口即脱敏：suggest 输出与
        // 落盘内容都不含密钥原文（spec §6.2；sk- 前缀 API key）。
        let mut store =
            FailurePatternStore::load(Path::new("/nonexistent/failure-patterns.json")).unwrap();
        let err = "api failed with key sk-abcdef123456";
        store.ingest(
            "execute",
            Some("bash"),
            err,
            Some("rotate key sk-abcdef123456 and retry"),
            100,
        );
        // 簇键基于脱敏后的 error 计算（不同密钥文本归一为同一簇）。
        let redacted_err = redact_secrets(err);
        let key = cluster_key("execute", Some("bash"), &redacted_err);
        let pattern = store.get(&key).expect("pattern present");
        assert!(
            !pattern
                .lesson
                .as_deref()
                .unwrap()
                .contains("sk-abcdef123456"),
            "lesson must be redacted"
        );
        for suggestion in store.suggest(3) {
            assert!(
                !suggestion.contains("sk-abcdef123456"),
                "suggest output must not leak the key: {suggestion}"
            );
        }
        // 同错误不同密钥文本 → 同一簇（脱敏归一化），不产生碎片簇。
        store.ingest(
            "execute",
            Some("bash"),
            "api failed with key sk-xyz987654321",
            None,
            200,
        );
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn save_writes_file_with_0600_permissions_on_unix() {
        let path = unique_temp_path();
        {
            let mut store = FailurePatternStore::load(&path).unwrap();
            store.ingest("execute", Some("bash"), "boom", None, 100);
            store.save().unwrap();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "failure-patterns.json must be 0600 on unix");
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn two_saves_to_same_path_do_not_conflict() {
        // 两次 save 到同路径：tmp 名带 PID+纳秒唯一后缀，互不踩踏，
        // 每次 save 均成功且最终文件存在可读。
        let path = unique_temp_path();
        {
            let mut store = FailurePatternStore::load(&path).unwrap();
            store.ingest("execute", Some("bash"), "err a", None, 100);
            store.save().unwrap();
        }
        {
            let mut store = FailurePatternStore::load(&path).unwrap();
            store.ingest("execute", Some("bash"), "err b", None, 200);
            store.save().unwrap();
        }
        assert!(path.exists());
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("err b"), "final file must contain latest data");
        let _ = std::fs::remove_file(&path);
    }
}

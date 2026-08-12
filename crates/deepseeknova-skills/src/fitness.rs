//! Skill fitness tracking and evolution suggestions.
//!
//! [`FitnessStore`] keeps per-skill usage statistics (loads, successes,
//! failures, last-used timestamp) in memory and persists them as JSON
//! (`{ "records": [...], "deprecated": [...] }`). [`evaluate`] turns those
//! records into non-destructive evolution suggestions (deprecate, merge,
//! promote) that a human confirms before acting on them.

use deepseeknova_core::DeepseeknovaError;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Maximum number of records kept in the store. When exceeded, the record
/// with the oldest `last_used_ms` is evicted (LRU semantics).
const MAX_RECORDS: usize = 500;

/// Milliseconds in 30 days — the inactivity threshold for deprecation.
const THIRTY_DAYS_MS: u64 = 30 * 24 * 60 * 60 * 1000;

// ---------------------------------------------------------------------------
// Records & persistence layout
// ---------------------------------------------------------------------------

/// Usage statistics for a single skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillFitnessRecord {
    /// Skill name (matches the skill's `name` field).
    pub skill: String,
    /// Number of times the skill was activated.
    pub uses: u32,
    /// Number of sessions using this skill that ended successfully.
    pub successes: u32,
    /// Number of sessions using this skill that failed.
    pub failures: u32,
    /// Unix timestamp (milliseconds) of the last use or result record.
    pub last_used_ms: u64,
}

/// On-disk layout of the fitness file. Both fields default to empty so old
/// files that predate the `deprecated` set still load.
#[derive(Debug, Serialize, Deserialize)]
struct FitnessFile {
    #[serde(default)]
    records: Vec<SkillFitnessRecord>,
    #[serde(default)]
    deprecated: Vec<String>,
}

// ---------------------------------------------------------------------------
// FitnessStore
// ---------------------------------------------------------------------------

/// In-memory store of per-skill fitness records with JSON persistence.
///
/// Records are capped at `MAX_RECORDS`; the least recently used record is
/// evicted when the cap is exceeded. The deprecated marker set persists in
/// the same file as the records (skills are marked, never deleted).
pub struct FitnessStore {
    records: HashMap<String, SkillFitnessRecord>,
    deprecated: HashSet<String>,
    path: PathBuf,
}

impl FitnessStore {
    /// Load the store from `path`.
    ///
    /// A missing file yields an empty store; a corrupt file is logged as a
    /// warning and also yields an empty store (never fails).
    pub fn load(path: &Path) -> Result<Self, DeepseeknovaError> {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::empty(path));
            }
            Err(e) => {
                return Err(DeepseeknovaError::Io(std::io::Error::new(
                    e.kind(),
                    format!("failed to read fitness file {}: {e}", path.display()),
                )));
            }
        };

        match serde_json::from_str::<FitnessFile>(&raw) {
            Ok(file) => {
                let records = file
                    .records
                    .into_iter()
                    .map(|r| (r.skill.clone(), r))
                    .collect();
                Ok(Self {
                    records,
                    deprecated: file.deprecated.into_iter().collect(),
                    path: path.to_path_buf(),
                })
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "fitness file corrupt — starting with an empty store"
                );
                Ok(Self::empty(path))
            }
        }
    }

    /// Record a skill activation: increments `uses` and bumps `last_used_ms`.
    pub fn record_use(&mut self, skill: &str, now_ms: u64) {
        let entry = self
            .records
            .entry(skill.to_string())
            .or_insert_with(|| SkillFitnessRecord {
                skill: skill.to_string(),
                uses: 0,
                successes: 0,
                failures: 0,
                last_used_ms: now_ms,
            });
        entry.uses = entry.uses.saturating_add(1);
        entry.last_used_ms = now_ms;
        self.enforce_capacity();
    }

    /// Record a session outcome for a skill: increments `successes` or
    /// `failures` and bumps `last_used_ms`.
    pub fn record_result(&mut self, skill: &str, success: bool, now_ms: u64) {
        let entry = self
            .records
            .entry(skill.to_string())
            .or_insert_with(|| SkillFitnessRecord {
                skill: skill.to_string(),
                uses: 0,
                successes: 0,
                failures: 0,
                last_used_ms: now_ms,
            });
        if success {
            entry.successes = entry.successes.saturating_add(1);
        } else {
            entry.failures = entry.failures.saturating_add(1);
        }
        entry.last_used_ms = now_ms;
        self.enforce_capacity();
    }

    /// Persist the store to disk atomically (temp file + rename).
    ///
    /// Creates the parent directory if it does not exist.
    ///
    /// 临时文件名带进程 PID 与纳秒时间戳：多进程（serve 多会话 / CLI+serve
    /// 双进程共享工作区）并发写同一路径时 tmp 文件互不踩踏，rename 仍是
    /// 原子替换。已知限制：进程级并发写会丢更新（后写者覆盖整文件，写前
    /// 不 re-load 合并；单进程内由调用方串行保证）。刻意不引入文件锁依赖
    /// （不加 fs2/flock）。
    pub fn save(&self) -> Result<(), DeepseeknovaError> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    std::io::Error::new(
                        e.kind(),
                        format!("failed to create {}: {e}", parent.display()),
                    )
                })?;
            }
        }

        let file = FitnessFile {
            records: self.records.values().cloned().collect(),
            deprecated: self.deprecated.iter().cloned().collect(),
        };
        let json = serde_json::to_string_pretty(&file)?;

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let tmp_path = PathBuf::from(format!(
            "{}.{}.{}.tmp",
            self.path.display(),
            std::process::id(),
            nanos
        ));
        // tmp 创建即 0600 + rename 后收敛（对齐 diagnose/failure-patterns 先例：
        // 技能名为蒸馏产物，可能含会话内容信息；`std::fs::write` 默认 umask
        // 会留 0644 权限窗口）。
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create_new(true).mode(0o600);
            let mut f = opts.open(&tmp_path).map_err(|e| {
                std::io::Error::new(
                    e.kind(),
                    format!("failed to create {}: {e}", tmp_path.display()),
                )
            })?;
            f.write_all(json.as_bytes()).map_err(|e| {
                std::io::Error::new(
                    e.kind(),
                    format!("failed to write {}: {e}", tmp_path.display()),
                )
            })?;
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&tmp_path, &json).map_err(|e| {
                std::io::Error::new(
                    e.kind(),
                    format!("failed to write {}: {e}", tmp_path.display()),
                )
            })?;
        }
        std::fs::rename(&tmp_path, &self.path).map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!(
                    "failed to rename {} to {}: {e}",
                    tmp_path.display(),
                    self.path.display()
                ),
            )
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600)).map_err(
                |e| {
                    std::io::Error::new(
                        e.kind(),
                        format!("failed to chmod 0600 {}: {e}", self.path.display()),
                    )
                },
            )?;
        }
        Ok(())
    }

    /// Return a copy of all records sorted by `uses` descending (ties broken
    /// by skill name for deterministic output).
    pub fn snapshot(&self) -> Vec<SkillFitnessRecord> {
        let mut records: Vec<SkillFitnessRecord> = self.records.values().cloned().collect();
        records.sort_by(|a, b| b.uses.cmp(&a.uses).then_with(|| a.skill.cmp(&b.skill)));
        records
    }

    /// Whether `name` is in the persisted deprecated marker set.
    pub fn is_deprecated(&self, name: &str) -> bool {
        self.deprecated.contains(name)
    }

    /// 标记某技能为已弃用（下次 [`FitnessStore::save`] 时持久化）。
    ///
    /// 幂等：重复标记同一技能名无副作用。已弃用技能不会被删除，只从
    /// 加载/解析结果中过滤。
    pub fn mark_deprecated(&mut self, name: &str) {
        self.deprecated.insert(name.to_string());
    }

    fn empty(path: &Path) -> Self {
        Self {
            records: HashMap::new(),
            deprecated: HashSet::new(),
            path: path.to_path_buf(),
        }
    }

    /// Evict the record with the oldest `last_used_ms` until the store is
    /// within `MAX_RECORDS`.
    fn enforce_capacity(&mut self) {
        while self.records.len() > MAX_RECORDS {
            let oldest = self
                .records
                .iter()
                .min_by_key(|(_, r)| (r.last_used_ms, &r.skill))
                .map(|(name, _)| name.clone());
            if let Some(name) = oldest {
                self.records.remove(&name);
            } else {
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Evolution suggestions
// ---------------------------------------------------------------------------

/// A non-destructive evolution suggestion produced by [`evaluate`].
///
/// Suggestions are advisory only — nothing is deprecated, merged or promoted
/// automatically.
#[derive(Debug, Clone)]
pub enum EvolutionSuggestion {
    /// Skill is unused or underperforming; consider deprecating it.
    Deprecate {
        /// The skill name.
        skill: String,
        /// Why this skill was flagged.
        reason: String,
    },
    /// Two skills look similar; consider merging them.
    MergeCandidate {
        /// The two candidate skill names.
        skills: Vec<String>,
        /// Why this pair was flagged.
        reason: String,
    },
    /// Skill is highly successful; consider promoting it (load order front).
    Promote {
        /// The skill name.
        skill: String,
        /// Why this skill was flagged.
        reason: String,
    },
}

/// Compute evolution suggestions from fitness records (pure function).
///
/// Rules:
/// - `Deprecate`: `now_ms - last_used_ms > 30 days`, or `uses >= 5` with a
///   success rate below 0.3.
/// - `Promote`: success rate >= 0.8 with `uses >= 10`.
/// - `MergeCandidate`: each pair of skills whose names share at least 50% of
///   their character bigrams (the smaller set) and whose success rates differ
///   by less than 0.15.
///
/// A skill idle for over 30 days is only flagged for deprecation, never for
/// promotion, even if it was historically successful.
pub fn evaluate(records: &[SkillFitnessRecord], now_ms: u64) -> Vec<EvolutionSuggestion> {
    let mut suggestions = Vec::new();

    for record in records {
        let idle_ms = now_ms.saturating_sub(record.last_used_ms);
        if idle_ms > THIRTY_DAYS_MS {
            suggestions.push(EvolutionSuggestion::Deprecate {
                skill: record.skill.clone(),
                reason: format!("not used for {} days", idle_ms / 86_400_000),
            });
            continue;
        }

        let rate = success_rate(record);
        if record.uses >= 5 && rate < 0.3 {
            suggestions.push(EvolutionSuggestion::Deprecate {
                skill: record.skill.clone(),
                reason: format!("success rate {rate:.2} below 0.3 with {} uses", record.uses),
            });
        }
        if record.uses >= 10 && rate >= 0.8 {
            suggestions.push(EvolutionSuggestion::Promote {
                skill: record.skill.clone(),
                reason: format!("success rate {rate:.2} with {} uses", record.uses),
            });
        }
    }

    for i in 0..records.len() {
        for j in (i + 1)..records.len() {
            let a = &records[i];
            let b = &records[j];
            let similarity = name_similarity(&a.skill, &b.skill);
            if similarity < 0.5 {
                continue;
            }
            let rate_gap = (success_rate(a) - success_rate(b)).abs();
            if rate_gap < 0.15 {
                suggestions.push(EvolutionSuggestion::MergeCandidate {
                    skills: vec![a.skill.clone(), b.skill.clone()],
                    reason: format!(
                        "name similarity {similarity:.2}, success rate gap {rate_gap:.2}"
                    ),
                });
            }
        }
    }

    suggestions
}

/// Success rate in `[0, 1]`; `0.0` when no results have been recorded yet.
fn success_rate(record: &SkillFitnessRecord) -> f64 {
    let total = record.successes as u64 + record.failures as u64;
    if total == 0 {
        0.0
    } else {
        record.successes as f64 / total as f64
    }
}

/// Character bigram overlap between two names: `|A ∩ B| / min(|A|, |B|)`.
fn name_similarity(a: &str, b: &str) -> f64 {
    let bigrams = |s: &str| -> HashSet<(char, char)> {
        let chars: Vec<char> = s.chars().collect();
        chars.windows(2).map(|w| (w[0], w[1])).collect()
    };
    let a_set = bigrams(a);
    let b_set = bigrams(b);
    if a_set.is_empty() || b_set.is_empty() {
        return 0.0;
    }
    let overlap = a_set.intersection(&b_set).count();
    overlap as f64 / a_set.len().min(b_set.len()) as f64
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn record(
        skill: &str,
        uses: u32,
        successes: u32,
        failures: u32,
        last_used_ms: u64,
    ) -> SkillFitnessRecord {
        SkillFitnessRecord {
            skill: skill.to_string(),
            uses,
            successes,
            failures,
            last_used_ms,
        }
    }

    #[test]
    fn record_use_tracks_count_and_timestamp() {
        let mut store = FitnessStore::load(Path::new("/nonexistent/fitness.json")).unwrap();
        store.record_use("alpha", 1_000);
        store.record_use("alpha", 2_000);
        store.record_use("beta", 3_000);

        let snap = store.snapshot();
        assert_eq!(snap.len(), 2);
        let alpha = snap.iter().find(|r| r.skill == "alpha").unwrap();
        assert_eq!(alpha.uses, 2);
        assert_eq!(alpha.last_used_ms, 2_000);
        let beta = snap.iter().find(|r| r.skill == "beta").unwrap();
        assert_eq!(beta.uses, 1);
        assert_eq!(beta.last_used_ms, 3_000);
    }

    #[test]
    fn record_result_tracks_success_and_failure() {
        let mut store = FitnessStore::load(Path::new("/nonexistent/fitness.json")).unwrap();
        store.record_result("alpha", true, 1_000);
        store.record_result("alpha", true, 2_000);
        store.record_result("alpha", false, 3_000);

        let alpha = store
            .snapshot()
            .into_iter()
            .find(|r| r.skill == "alpha")
            .unwrap();
        assert_eq!(alpha.successes, 2);
        assert_eq!(alpha.failures, 1);
        assert_eq!(alpha.last_used_ms, 3_000);
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fitness.json");
        let mut store = FitnessStore::load(&path).unwrap();
        store.record_use("alpha", 1_000);
        store.record_result("alpha", true, 2_000);
        store.record_result("beta", false, 3_000);
        store.save().unwrap();

        let loaded = FitnessStore::load(&path).unwrap();
        let alpha = loaded
            .snapshot()
            .into_iter()
            .find(|r| r.skill == "alpha")
            .unwrap();
        assert_eq!(
            (
                alpha.uses,
                alpha.successes,
                alpha.failures,
                alpha.last_used_ms
            ),
            (1, 1, 0, 2_000)
        );
        let beta = loaded
            .snapshot()
            .into_iter()
            .find(|r| r.skill == "beta")
            .unwrap();
        assert_eq!((beta.successes, beta.failures), (0, 1));
    }

    #[test]
    fn two_saves_to_same_path_do_not_conflict() {
        // 两次 save 到同路径：tmp 名带 PID+纳秒唯一后缀，互不踩踏，
        // 每次 save 均成功且最终文件存在可读、内容为最新数据。
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fitness.json");
        {
            let mut store = FitnessStore::load(&path).unwrap();
            store.record_use("alpha", 1_000);
            store.save().unwrap();
        }
        {
            let mut store = FitnessStore::load(&path).unwrap();
            store.record_use("beta", 2_000);
            store.save().unwrap();
        }
        assert!(path.exists());
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("beta"), "final file must contain latest data");
    }

    #[test]
    fn load_missing_file_yields_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = FitnessStore::load(&dir.path().join("missing.json")).unwrap();
        assert!(store.snapshot().is_empty());
        assert!(!store.is_deprecated("anything"));
    }

    #[test]
    fn load_corrupt_file_yields_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fitness.json");
        std::fs::write(&path, "not json at all {{{").unwrap();
        let store = FitnessStore::load(&path).unwrap();
        assert!(store.snapshot().is_empty());
    }

    #[test]
    fn load_old_file_without_deprecated_field_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fitness.json");
        // Old layout: no `deprecated` key.
        std::fs::write(
            &path,
            r#"{"records":[{"skill":"alpha","uses":1,"successes":1,"failures":0,"last_used_ms":1000}]}"#,
        )
        .unwrap();
        let store = FitnessStore::load(&path).unwrap();
        assert_eq!(store.snapshot().len(), 1);
        assert!(!store.is_deprecated("alpha"));
    }

    #[test]
    fn capacity_evicts_oldest_record() {
        let mut store = FitnessStore::load(Path::new("/nonexistent/fitness.json")).unwrap();
        for i in 0..(MAX_RECORDS as u64 + 1) {
            store.record_use(&format!("skill-{i}"), i);
        }
        let snap = store.snapshot();
        assert_eq!(snap.len(), MAX_RECORDS);
        assert!(
            snap.iter().all(|r| r.skill != "skill-0"),
            "oldest record must be evicted"
        );
        assert!(snap.iter().any(|r| r.skill == "skill-500"));
    }

    #[test]
    fn snapshot_sorted_by_uses_descending() {
        let mut store = FitnessStore::load(Path::new("/nonexistent/fitness.json")).unwrap();
        store.record_use("a", 1);
        store.record_use("a", 2);
        store.record_use("b", 3);
        store.record_use("b", 4);
        store.record_use("b", 5);
        store.record_use("c", 6);
        let uses: Vec<u32> = store.snapshot().iter().map(|r| r.uses).collect();
        assert_eq!(uses, vec![3, 2, 1]);
    }

    #[test]
    fn evaluate_deprecates_idle_skill() {
        let now = 100_000_000_000;
        let records = vec![record("idle", 100, 90, 10, now - THIRTY_DAYS_MS - 1)];
        let suggestions = evaluate(&records, now);
        assert!(suggestions.iter().any(|s| matches!(
            s,
            EvolutionSuggestion::Deprecate { skill, .. } if skill == "idle"
        )));
        assert!(!suggestions
            .iter()
            .any(|s| matches!(s, EvolutionSuggestion::Promote { .. })));
    }

    #[test]
    fn evaluate_deprecates_low_success_rate() {
        let now = 100_000_000_000;
        let records = vec![record("flaky", 5, 1, 4, now)];
        let suggestions = evaluate(&records, now);
        assert!(matches!(
            &suggestions[..],
            [EvolutionSuggestion::Deprecate { skill, .. }] if skill == "flaky"
        ));
    }

    #[test]
    fn evaluate_promotes_high_success_high_use() {
        let now = 100_000_000_000;
        let records = vec![record("reliable", 10, 9, 1, now)];
        let suggestions = evaluate(&records, now);
        assert!(matches!(
            &suggestions[..],
            [EvolutionSuggestion::Promote { skill, .. }] if skill == "reliable"
        ));
    }

    #[test]
    fn evaluate_flags_merge_candidates_on_similar_names() {
        let now = 100_000_000_000;
        let records = vec![
            record("frontend-developer", 10, 8, 2, now),
            record("frontend-developer-v2", 10, 7, 3, now),
        ];
        let suggestions = evaluate(&records, now);
        assert!(
            suggestions.iter().any(|s| matches!(
                s,
                EvolutionSuggestion::MergeCandidate { skills, .. }
                    if skills == &vec!["frontend-developer".to_string(), "frontend-developer-v2".to_string()]
            )),
            "expected a merge candidate, got {suggestions:?}"
        );
    }

    #[test]
    fn evaluate_returns_nothing_for_healthy_records() {
        let now = 100_000_000_000;
        let records = vec![
            record("steady", 3, 2, 1, now),
            record("fresh", 1, 1, 0, now),
        ];
        let suggestions = evaluate(&records, now);
        assert!(
            suggestions.is_empty(),
            "expected no suggestions, got {suggestions:?}"
        );
    }

    #[test]
    fn evaluate_idle_at_exactly_thirty_days_is_not_deprecated() {
        let now = 100_000_000_000;
        let records = vec![record("borderline", 1, 1, 0, now - THIRTY_DAYS_MS)];
        let suggestions = evaluate(&records, now);
        assert!(
            suggestions.is_empty(),
            "expected no suggestions, got {suggestions:?}"
        );
    }

    #[test]
    fn deprecated_markers_persist_across_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fitness.json");
        // Seed a file with a deprecated marker, then round-trip
        // load → save → load.
        let file = FitnessFile {
            records: vec![],
            deprecated: vec!["legacy-skill".to_string()],
        };
        std::fs::write(&path, serde_json::to_string(&file).unwrap()).unwrap();

        let store = FitnessStore::load(&path).unwrap();
        assert!(store.is_deprecated("legacy-skill"));
        assert!(!store.is_deprecated("other"));
        store.save().unwrap();

        let reloaded = FitnessStore::load(&path).unwrap();
        assert!(reloaded.is_deprecated("legacy-skill"));
        assert!(!reloaded.is_deprecated("other"));
    }

    #[test]
    fn mark_deprecated_then_save_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fitness.json");
        let mut store = FitnessStore::load(&path).unwrap();
        store.mark_deprecated("legacy-skill");
        assert!(store.is_deprecated("legacy-skill"));
        assert!(!store.is_deprecated("other"));
        store.save().unwrap();

        let loaded = FitnessStore::load(&path).unwrap();
        assert!(loaded.is_deprecated("legacy-skill"));
        assert!(!loaded.is_deprecated("other"));
    }
}

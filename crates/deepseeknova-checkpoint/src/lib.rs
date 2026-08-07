//! # Checkpoint — File state snapshot and rollback manager
//!
//! Provides transactional file-system checkpoints so agents can
//! commit or revert batches of file changes safely.
//!
//! 内存快照受 [`crate::DEFAULT_MAX_SNAPSHOTS`]（可用
//! [`crate::CheckpointManager::with_max_snapshots`] 自定义）容量上限约束，
//! 超限按 FIFO 淘汰最旧快照，避免长会话无界增长。持久化采用增量追加
//! （只写新增快照），仅在淘汰 / 回滚 / 清空导致内存与文件失去对齐时
//! 才全量重写。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// CheckpointManager — file snapshot + rollback
// ---------------------------------------------------------------------------

/// A snapshot of a file's content identified by its SHA-256 hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub path: PathBuf,
    pub content: String,
    pub hash: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// 内存快照容量上限的默认值：超出后按 FIFO 淘汰最旧快照。
pub const DEFAULT_MAX_SNAPSHOTS: usize = 200;

/// `CheckpointManager` takes filesystem snapshots before mutations and
/// supports rollback to the most recent snapshot.
///
/// 内存快照受 [`crate::DEFAULT_MAX_SNAPSHOTS`]（或
/// [`crate::CheckpointManager::with_max_snapshots`] 自定义）上限约束：
/// 超限时按 FIFO 淘汰最旧快照。持久化采用增量追加，仅在淘汰 / 回滚 /
/// 清空等内存与文件失去对齐的场景才全量重写。
pub struct CheckpointManager {
    snapshots: Vec<Snapshot>,
    /// 可选持久化文件（JSONL）。设置后每次快照/回滚/清空都会落盘，
    /// 使 CLI 跨进程 `checkpoint list/rollback` 可用。
    persist_path: Option<PathBuf>,
    /// 已落盘快照条数（从 `snapshots` 头部起算），用于增量追加时跳过
    /// 已写部分。
    persisted_len: usize,
    /// 内存快照容量上限；超限按 FIFO 淘汰最旧。
    max_snapshots: usize,
}

impl CheckpointManager {
    pub fn new() -> Self {
        Self {
            snapshots: Vec::new(),
            persist_path: None,
            persisted_len: 0,
            max_snapshots: DEFAULT_MAX_SNAPSHOTS,
        }
    }

    /// 从 JSONL 文件恢复快照（文件不存在 → 空管理器）。
    pub fn load_from(path: &Path) -> anyhow::Result<Self> {
        let mut manager = Self::new();
        manager.persist_path = Some(path.to_path_buf());
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(manager),
            Err(e) => return Err(e.into()),
        };
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let snap: Snapshot = serde_json::from_str(line)?;
            manager.snapshots.push(snap);
        }
        // 文件内容即为磁盘已落盘状态，游标对齐文件末尾。
        manager.persisted_len = manager.snapshots.len();
        Ok(manager)
    }

    /// 开启持久化（路径父目录自动创建）。
    pub fn with_persistence(mut self, path: PathBuf) -> Self {
        self.persist_path = Some(path);
        // 重置落盘游标：若中途改指新文件，后续首次写入以全量重写对齐，
        // 避免与既有文件内容重复拼接。
        self.persisted_len = 0;
        self
    }

    /// 设置内存快照容量上限（默认 [`crate::DEFAULT_MAX_SNAPSHOTS`]）。
    /// 超出后按 FIFO 淘汰最旧快照；`0` 表示任何新增快照都会被立即淘汰。
    pub fn with_max_snapshots(mut self, max: usize) -> Self {
        self.max_snapshots = max;
        self
    }

    /// 当前内存快照容量上限。
    pub fn max_snapshots(&self) -> usize {
        self.max_snapshots
    }

    /// 把当前全部快照全量重写回持久化文件（JSONL，truncate + 重写）。
    /// 用于淘汰、回滚、清空等内存与文件失去对齐的场景。
    /// 未配置持久化时为空操作。
    fn persist_all(&mut self) -> anyhow::Result<()> {
        let Some(path) = &self.persist_path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let mut f = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)?;
        for snap in &self.snapshots {
            writeln!(f, "{}", serde_json::to_string(snap)?)?;
        }
        self.persisted_len = self.snapshots.len();
        Ok(())
    }

    /// 增量追加自上次落盘以来新增的快照（JSONL append）。未配置持久化
    /// 或无新增快照时为空操作。
    ///
    /// 当磁盘文件与内存状态完全无对应（新开启持久化 / 清空后）时，以
    /// 截断重写代替追加，避免把既有文件内容与新状态重复拼接。
    fn persist_incremental(&mut self) -> anyhow::Result<()> {
        let Some(path) = &self.persist_path else {
            return Ok(());
        };
        let total = self.snapshots.len();
        if self.persisted_len >= total {
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let fresh = self.persisted_len == 0;
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(fresh)
            .append(!fresh)
            .open(path)?;
        for snap in &self.snapshots[self.persisted_len..] {
            writeln!(f, "{}", serde_json::to_string(snap)?)?;
        }
        self.persisted_len = total;
        Ok(())
    }

    /// 超过容量上限时按 FIFO 淘汰最旧快照。若发生淘汰，内存与文件失去
    /// 对齐（被淘汰行的旧内容仍在文件中），需全量重写持久化文件保持一致。
    fn enforce_capacity(&mut self) -> anyhow::Result<()> {
        let evicted = self.snapshots.len().saturating_sub(self.max_snapshots);
        if evicted > 0 {
            self.snapshots.drain(..evicted);
            self.persist_all()?;
        }
        Ok(())
    }

    /// Take a snapshot of the file at `path`.
    pub async fn snapshot_file(&mut self, path: &Path) -> anyhow::Result<()> {
        let (content, hash) = if path.exists() {
            let bytes = tokio::fs::read(path).await?;
            let content = String::from_utf8_lossy(&bytes).to_string();
            let hash = hex::encode(Sha256::digest(&bytes));
            (content, hash)
        } else {
            (String::new(), hex::encode(Sha256::digest(b"")))
        };

        self.snapshots.push(Snapshot {
            path: path.to_path_buf(),
            content,
            hash,
            created_at: chrono::Utc::now(),
        });

        self.enforce_capacity()?;
        self.persist_incremental()?;
        Ok(())
    }

    /// Take snapshots of multiple files.
    pub async fn snapshot_files(&mut self, paths: &[&Path]) -> anyhow::Result<()> {
        for path in paths {
            self.snapshot_file(path).await?;
        }
        Ok(())
    }

    /// Take snapshots of all files under a directory (recursive).
    /// 返回实际快照的文件数（容量淘汰不影响计数）。
    pub async fn snapshot_dir(&mut self, root: &Path) -> anyhow::Result<usize> {
        let mut count = 0;
        for entry in walkdir::WalkDir::new(root)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file() {
                self.snapshot_file(entry.path()).await?;
                count += 1;
            }
        }
        Ok(count)
    }

    /// 执行单次回滚的文件系统恢复，不做持久化（由调用方统一落盘）。
    async fn rollback_inner(&mut self) -> anyhow::Result<Option<(PathBuf, String)>> {
        match self.snapshots.pop() {
            Some(snap) => {
                if snap.content.is_empty() {
                    // File was absent before — remove it if it now exists
                    if snap.path.exists() {
                        tokio::fs::remove_file(&snap.path).await?;
                    }
                } else {
                    // Restore original content atomically
                    let ext = snap
                        .path
                        .extension()
                        .map(|e| format!(".{}.rollback", e.to_string_lossy()))
                        .unwrap_or_else(|| ".rollback".to_string());
                    let tmp = snap.path.with_extension(&ext[1..]);
                    tokio::fs::write(&tmp, snap.content.as_bytes()).await?;
                    tokio::fs::rename(&tmp, &snap.path).await?;
                }
                tracing::info!(
                    "rolled back {} (hash {})",
                    snap.path.display(),
                    &snap.hash[..8.min(snap.hash.len())]
                );
                Ok(Some((snap.path, snap.hash)))
            }
            None => Ok(None),
        }
    }

    /// Rollback: restore the most recent snapshot and remove it from the stack.
    /// Returns the path that was rolled back, or `None` if the stack is empty.
    pub async fn rollback(&mut self) -> anyhow::Result<Option<(PathBuf, String)>> {
        let result = self.rollback_inner().await?;
        // 内存弹出一条后与文件失去对齐，全量重写以截断末尾行。
        if result.is_some() {
            self.persist_all()?;
        }
        Ok(result)
    }

    /// Rollback ALL snapshots in reverse order.
    pub async fn rollback_all(&mut self) -> anyhow::Result<usize> {
        let count = self.snapshots.len();
        while !self.snapshots.is_empty() {
            self.rollback_inner().await?;
        }
        // 全部弹完后统一落盘一次，避免逐条全量重写的 O(n²) 放大。
        if count > 0 {
            self.persist_all()?;
        }
        Ok(count)
    }

    /// Check if the current content of all snapshotted files still matches
    /// their snapshot hashes.
    pub fn snapshots(&self) -> &[Snapshot] {
        &self.snapshots
    }

    /// Return an owned list of all snapshots for verification.
    pub fn all_snapshots(&self) -> Vec<&Snapshot> {
        self.snapshots.iter().collect()
    }

    /// Verify all snapshots against current filesystem state.
    /// Returns (snapshot, is_clean).
    pub async fn verify(&self) -> anyhow::Result<Vec<(&Snapshot, bool)>> {
        let mut results = Vec::new();
        for snap in &self.snapshots {
            let current_hash = if snap.path.exists() {
                let bytes = tokio::fs::read(&snap.path).await?;
                hex::encode(Sha256::digest(&bytes))
            } else {
                hex::encode(Sha256::digest(b""))
            };
            let clean = current_hash == snap.hash;
            results.push((snap, clean));
        }
        Ok(results)
    }

    /// Number of active snapshots.
    pub fn len(&self) -> usize {
        self.snapshots.len()
    }

    /// Whether there are no active snapshots.
    pub fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }

    /// Discard all snapshots without restoring.
    pub fn clear(&mut self) {
        self.snapshots.clear();
        self.persist_all().ok();
    }

    /// Build a diff summary: what changed between snapshots and current state.
    pub async fn diff_summary(&self) -> anyhow::Result<String> {
        if self.snapshots.is_empty() {
            return Ok("no snapshots".to_string());
        }

        let mut lines = Vec::new();
        let verify = self.verify().await?;
        for (snap, clean) in &verify {
            let status = if *clean { "unchanged" } else { "modified" };
            lines.push(format!(
                "  {}: {} ({})",
                snap.path.display(),
                &snap.hash[..8.min(snap.hash.len())],
                status
            ));
        }
        Ok(format!(
            "{} file(s) snapshotted:\n{}",
            verify.len(),
            lines.join("\n")
        ))
    }
}

impl Default for CheckpointManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_dir() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "deepseeknova-ck-test-{}-{}",
            std::process::id(),
            id
        ));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn write_file(path: &Path, content: &str) {
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    #[tokio::test]
    async fn snapshot_and_rollback() {
        let dir = temp_dir();
        let file = dir.join("test.txt");
        write_file(&file, "original content");

        let mut ck = CheckpointManager::new();
        ck.snapshot_file(&file).await.unwrap();
        assert_eq!(ck.len(), 1);

        // Mutate
        write_file(&file, "modified content");

        // Rollback
        let result = ck.rollback().await.unwrap();
        assert!(result.is_some());

        let restored = std::fs::read_to_string(&file).unwrap();
        assert_eq!(restored, "original content");
        assert!(ck.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn snapshot_absent_file_rollback_deletes() {
        let dir = temp_dir();
        let file = dir.join("absent.txt");

        let mut ck = CheckpointManager::new();
        ck.snapshot_file(&file).await.unwrap();

        // Create file after snapshot
        write_file(&file, "new file");
        assert!(file.exists());

        // Rollback should delete it
        ck.rollback().await.unwrap();
        assert!(!file.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn rollback_all_restores_everything() {
        let dir = temp_dir();
        let f1 = dir.join("a.txt");
        let f2 = dir.join("b.txt");
        write_file(&f1, "A");
        write_file(&f2, "B");

        let mut ck = CheckpointManager::new();
        ck.snapshot_file(&f1).await.unwrap();
        ck.snapshot_file(&f2).await.unwrap();

        // Mutate both
        write_file(&f1, "A modified");
        write_file(&f2, "B modified");

        let count = ck.rollback_all().await.unwrap();
        assert_eq!(count, 2);

        assert_eq!(std::fs::read_to_string(&f1).unwrap(), "A");
        assert_eq!(std::fs::read_to_string(&f2).unwrap(), "B");
        assert!(ck.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn verify_detects_modifications() {
        let dir = temp_dir();
        let file = dir.join("v.txt");
        write_file(&file, "data");

        let mut ck = CheckpointManager::new();
        ck.snapshot_file(&file).await.unwrap();

        // Not modified — should be clean
        let results = ck.verify().await.unwrap();
        assert!(results[0].1);

        // Modify
        write_file(&file, "modified data");
        let results = ck.verify().await.unwrap();
        assert!(!results[0].1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_manager_is_empty() {
        let ck = CheckpointManager::new();
        assert!(ck.is_empty());
        assert_eq!(ck.len(), 0);
    }

    #[test]
    fn clear_discards_snapshots() {
        let mut ck = CheckpointManager::new();
        // Simulate a snapshot without going through async snapshot_file
        ck.snapshots.push(Snapshot {
            path: PathBuf::from("x"),
            content: "hello".into(),
            hash: "abcdef0123456789".into(),
            created_at: chrono::Utc::now(),
        });
        ck.clear();
        assert!(ck.is_empty());
    }

    #[tokio::test]
    async fn rollback_empty_returns_none() {
        let mut ck = CheckpointManager::new();
        let result = ck.rollback().await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn snapshot_multiple_files() {
        let dir = temp_dir();
        let f1 = dir.join("x.txt");
        let f2 = dir.join("y.txt");
        write_file(&f1, "x");
        write_file(&f2, "y");

        let mut ck = CheckpointManager::new();
        ck.snapshot_files(&[&f1, &f2]).await.unwrap();
        assert_eq!(ck.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn diff_summary_after_verify() {
        let dir = temp_dir();
        let f = dir.join("diff.txt");
        write_file(&f, "original");

        let mut ck = CheckpointManager::new();
        ck.snapshot_file(&f).await.unwrap();

        write_file(&f, "changed");

        let summary = ck.diff_summary().await.unwrap();
        assert!(summary.contains("modified"));
        assert!(summary.contains("diff.txt"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn persistence_roundtrip_across_instances() {
        let dir = temp_dir();
        let file = dir.join("persist.txt");
        write_file(&file, "v1");
        let ck_path = dir.join("checkpoints.jsonl");

        {
            let mut ck = CheckpointManager::new().with_persistence(ck_path.clone());
            ck.snapshot_file(&file).await.unwrap();
            write_file(&file, "v2");
        }

        // 新实例（模拟进程重启）从文件恢复，可回滚。
        let mut ck2 = CheckpointManager::load_from(&ck_path).unwrap();
        assert_eq!(ck2.len(), 1, "snapshot must survive process restart");
        let restored = ck2.rollback().await.unwrap();
        assert!(restored.is_some());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "v1");
        assert!(ck2.is_empty());
        // 回滚后持久化文件同步为空。
        let ck3 = CheckpointManager::load_from(&ck_path).unwrap();
        assert!(ck3.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn persistence_survives_clear() {
        let dir = temp_dir();
        let file = dir.join("c.txt");
        write_file(&file, "x");
        let ck_path = dir.join("checkpoints.jsonl");
        let mut ck = CheckpointManager::new().with_persistence(ck_path.clone());
        ck.snapshot_file(&file).await.unwrap();
        ck.clear();
        let reloaded = CheckpointManager::load_from(&ck_path).unwrap();
        assert!(reloaded.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── 容量上限 ─────────────────────────────────────────────────────

    #[test]
    fn default_capacity_is_bounded() {
        assert_eq!(
            CheckpointManager::new().max_snapshots(),
            DEFAULT_MAX_SNAPSHOTS
        );
    }

    #[tokio::test]
    async fn capacity_evicts_oldest_fifo() {
        let dir = temp_dir();
        let f1 = dir.join("a.txt");
        let f2 = dir.join("b.txt");
        let f3 = dir.join("c.txt");
        write_file(&f1, "1");
        write_file(&f2, "2");
        write_file(&f3, "3");

        let mut ck = CheckpointManager::new().with_max_snapshots(2);
        ck.snapshot_file(&f1).await.unwrap();
        ck.snapshot_file(&f2).await.unwrap();
        assert_eq!(ck.len(), 2);
        ck.snapshot_file(&f3).await.unwrap();
        assert_eq!(ck.len(), 2, "超限后应淘汰最旧快照");

        let paths: Vec<PathBuf> = ck.all_snapshots().iter().map(|s| s.path.clone()).collect();
        assert_eq!(paths, vec![f2.clone(), f3.clone()], "应保留最近两条");

        // 回滚顺序：先弹最新（f3），再弹 f2。
        let (p, _) = ck.rollback().await.unwrap().unwrap();
        assert_eq!(p, f3);
        let (p, _) = ck.rollback().await.unwrap().unwrap();
        assert_eq!(p, f2);
        assert!(ck.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn capacity_zero_evicts_immediately() {
        let dir = temp_dir();
        let file = dir.join("z.txt");
        write_file(&file, "x");
        let mut ck = CheckpointManager::new().with_max_snapshots(0);
        ck.snapshot_file(&file).await.unwrap();
        assert!(ck.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn snapshot_dir_counts_files_and_respects_capacity() {
        let dir = temp_dir();
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        for i in 0..3 {
            write_file(&sub.join(format!("f{i}.txt")), "data");
        }
        let mut ck = CheckpointManager::new().with_max_snapshots(2);
        let n = ck.snapshot_dir(&dir).await.unwrap();
        assert_eq!(n, 3, "应返回实际快照的文件数，而非扣除淘汰后的差值");
        assert_eq!(ck.len(), 2, "超限应淘汰最旧");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── 增量持久化 ───────────────────────────────────────────────────

    #[tokio::test]
    async fn persistence_incremental_appends_and_reloads_complete() {
        let dir = temp_dir();
        let ck_path = dir.join("inc.jsonl");
        let f1 = dir.join("a.txt");
        let f2 = dir.join("b.txt");
        let f3 = dir.join("c.txt");
        write_file(&f1, "A");
        write_file(&f2, "B");
        write_file(&f3, "C");

        let mut ck = CheckpointManager::new().with_persistence(ck_path.clone());
        ck.snapshot_file(&f1).await.unwrap();
        ck.snapshot_file(&f2).await.unwrap();
        ck.snapshot_file(&f3).await.unwrap();
        drop(ck);

        // 增量追加：文件应恰好 3 行，无重复。
        let content = std::fs::read_to_string(&ck_path).unwrap();
        assert_eq!(content.lines().count(), 3);

        // 重载后快照完整、顺序正确，且可逐条回滚。
        let mut ck2 = CheckpointManager::load_from(&ck_path).unwrap();
        assert_eq!(ck2.len(), 3);
        let paths: Vec<PathBuf> = ck2.all_snapshots().iter().map(|s| s.path.clone()).collect();
        assert_eq!(paths, vec![f1.clone(), f2.clone(), f3.clone()]);

        write_file(&f1, "A2");
        write_file(&f2, "B2");
        write_file(&f3, "C2");
        let count = ck2.rollback_all().await.unwrap();
        assert_eq!(count, 3);
        assert_eq!(std::fs::read_to_string(&f1).unwrap(), "A");
        assert_eq!(std::fs::read_to_string(&f2).unwrap(), "B");
        assert_eq!(std::fs::read_to_string(&f3).unwrap(), "C");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn persistence_eviction_keeps_file_consistent() {
        let dir = temp_dir();
        let ck_path = dir.join("evict.jsonl");
        let f1 = dir.join("a.txt");
        let f2 = dir.join("b.txt");
        let f3 = dir.join("c.txt");
        let f4 = dir.join("d.txt");
        write_file(&f1, "1");
        write_file(&f2, "2");
        write_file(&f3, "3");
        write_file(&f4, "4");

        let mut ck = CheckpointManager::new()
            .with_persistence(ck_path.clone())
            .with_max_snapshots(3);
        ck.snapshot_file(&f1).await.unwrap();
        ck.snapshot_file(&f2).await.unwrap();
        ck.snapshot_file(&f3).await.unwrap();
        ck.snapshot_file(&f4).await.unwrap();
        assert_eq!(ck.len(), 3);
        drop(ck);

        // 文件只保留最近 3 条：被淘汰的 f1 不应再出现在持久化文件中。
        let ck2 = CheckpointManager::load_from(&ck_path).unwrap();
        assert_eq!(ck2.len(), 3);
        let paths: Vec<PathBuf> = ck2.all_snapshots().iter().map(|s| s.path.clone()).collect();
        assert_eq!(paths, vec![f2.clone(), f3.clone(), f4.clone()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn persistence_rollback_then_reappend_has_no_duplicates() {
        let dir = temp_dir();
        let ck_path = dir.join("cycle.jsonl");
        let file = dir.join("x.txt");
        write_file(&file, "v1");

        let mut ck = CheckpointManager::new().with_persistence(ck_path.clone());
        ck.snapshot_file(&file).await.unwrap();
        write_file(&file, "v2");
        ck.rollback().await.unwrap();
        write_file(&file, "v3");
        ck.snapshot_file(&file).await.unwrap();
        drop(ck);

        let content = std::fs::read_to_string(&ck_path).unwrap();
        assert_eq!(content.lines().count(), 1, "回滚后再快照不应产生重复行");
        let ck2 = CheckpointManager::load_from(&ck_path).unwrap();
        assert_eq!(ck2.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── 旧格式兼容 ───────────────────────────────────────────────────

    #[test]
    fn old_format_jsonl_still_loads() {
        // Snapshot 序列化契约未变；手写一条既有格式 JSONL，验证 load_from
        // 仍能读取并保持回滚语义，防止未来格式漂移破坏旧文件。
        let dir = temp_dir();
        let ck_path = dir.join("old.jsonl");
        let snap = Snapshot {
            path: PathBuf::from("legacy.txt"),
            content: "legacy".into(),
            hash: "deadbeef0123456789".into(),
            created_at: chrono::Utc::now(),
        };
        std::fs::write(
            &ck_path,
            format!("{}\n", serde_json::to_string(&snap).unwrap()),
        )
        .unwrap();

        let ck = CheckpointManager::load_from(&ck_path).unwrap();
        assert_eq!(ck.len(), 1);
        assert_eq!(ck.snapshots()[0].path, PathBuf::from("legacy.txt"));
        assert_eq!(ck.snapshots()[0].hash, "deadbeef0123456789");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

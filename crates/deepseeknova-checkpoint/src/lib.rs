//! # Checkpoint — File state snapshot and rollback manager
//!
//! Provides transactional file-system checkpoints so agents can
//! commit or revert batches of file changes safely.

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

/// `CheckpointManager` takes filesystem snapshots before mutations and
/// supports rollback to the most recent snapshot.
pub struct CheckpointManager {
    snapshots: Vec<Snapshot>,
    /// 可选持久化文件（JSONL）。设置后每次快照/回滚/清空都会落盘，
    /// 使 CLI 跨进程 `checkpoint list/rollback` 可用。
    persist_path: Option<PathBuf>,
}

impl CheckpointManager {
    pub fn new() -> Self {
        Self {
            snapshots: Vec::new(),
            persist_path: None,
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
        Ok(manager)
    }

    /// 开启持久化（路径父目录自动创建）。
    pub fn with_persistence(mut self, path: PathBuf) -> Self {
        self.persist_path = Some(path);
        self
    }

    /// 把当前快照全量写回持久化文件（JSONL）。未配置持久化时为空操作。
    fn persist_all(&self) -> anyhow::Result<()> {
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

        self.persist_all()?;
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
    pub async fn snapshot_dir(&mut self, root: &Path) -> anyhow::Result<usize> {
        let before = self.snapshots.len();
        for entry in walkdir::WalkDir::new(root)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file() {
                self.snapshot_file(entry.path()).await?;
            }
        }
        Ok(self.snapshots.len() - before)
    }

    /// Rollback: restore the most recent snapshot and remove it from the stack.
    /// Returns the path that was rolled back, or `None` if the stack is empty.
    pub async fn rollback(&mut self) -> anyhow::Result<Option<(PathBuf, String)>> {
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
                self.persist_all()?;
                Ok(Some((snap.path, snap.hash)))
            }
            None => Ok(None),
        }
    }

    /// Rollback ALL snapshots in reverse order.
    pub async fn rollback_all(&mut self) -> anyhow::Result<usize> {
        let count = self.snapshots.len();
        while !self.snapshots.is_empty() {
            self.rollback().await?;
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
}

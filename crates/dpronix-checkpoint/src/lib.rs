use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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
#[derive(Default)]
pub struct CheckpointManager {
    snapshots: Vec<Snapshot>,
}

impl CheckpointManager {
    pub fn new() -> Self {
        Self {
            snapshots: Vec::new(),
        }
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
        let dir = std::env::temp_dir().join(format!("reasonix-ck-test-{}-{}", std::process::id(), id));
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
}

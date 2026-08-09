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

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use deepseeknova_core::DeepseeknovaError;
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
    pub fn load_from(path: &Path) -> Result<Self, DeepseeknovaError> {
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
    fn persist_all(&mut self) -> Result<(), DeepseeknovaError> {
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
    fn persist_incremental(&mut self) -> Result<(), DeepseeknovaError> {
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
    fn enforce_capacity(&mut self) -> Result<(), DeepseeknovaError> {
        let evicted = self.snapshots.len().saturating_sub(self.max_snapshots);
        if evicted > 0 {
            self.snapshots.drain(..evicted);
            self.persist_all()?;
        }
        Ok(())
    }

    /// Take a snapshot of the file at `path`.
    pub async fn snapshot_file(&mut self, path: &Path) -> Result<(), DeepseeknovaError> {
        self.snapshots.push(snapshot_state(path).await?);
        self.enforce_capacity()?;
        self.persist_incremental()?;
        Ok(())
    }

    /// Take snapshots of multiple files.
    pub async fn snapshot_files(&mut self, paths: &[&Path]) -> Result<(), DeepseeknovaError> {
        for path in paths {
            self.snapshot_file(path).await?;
        }
        Ok(())
    }

    /// Take snapshots of all files under a directory (recursive).
    /// 返回实际快照的文件数（容量淘汰不影响计数）。
    pub async fn snapshot_dir(&mut self, root: &Path) -> Result<usize, DeepseeknovaError> {
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
    async fn rollback_inner(&mut self) -> Result<Option<(PathBuf, String)>, DeepseeknovaError> {
        match self.snapshots.pop() {
            Some(snap) => {
                restore_state(&snap).await?;
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
    pub async fn rollback(&mut self) -> Result<Option<(PathBuf, String)>, DeepseeknovaError> {
        let result = self.rollback_inner().await?;
        // 内存弹出一条后与文件失去对齐，全量重写以截断末尾行。
        if result.is_some() {
            self.persist_all()?;
        }
        Ok(result)
    }

    /// Rollback ALL snapshots in reverse order.
    pub async fn rollback_all(&mut self) -> Result<usize, DeepseeknovaError> {
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
    pub async fn verify(&self) -> Result<Vec<(&Snapshot, bool)>, DeepseeknovaError> {
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
    pub async fn diff_summary(&self) -> Result<String, DeepseeknovaError> {
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
// 共享快照原语（CheckpointManager 与 SessionCheckpointManager 共用）
// ---------------------------------------------------------------------------

/// 读取文件当前状态并计算 SHA-256，构造一条 [`Snapshot`]。
/// 文件不存在时按空内容快照（回滚时删除现有文件）。
async fn snapshot_state(path: &Path) -> Result<Snapshot, DeepseeknovaError> {
    let (content, hash) = if path.exists() {
        let bytes = tokio::fs::read(path).await?;
        let content = String::from_utf8_lossy(&bytes).to_string();
        let hash = hex::encode(Sha256::digest(&bytes));
        (content, hash)
    } else {
        (String::new(), hex::encode(Sha256::digest(b"")))
    };
    Ok(Snapshot {
        path: path.to_path_buf(),
        content,
        hash,
        created_at: chrono::Utc::now(),
    })
}

/// 把一条快照状态恢复到文件系统：文件原本不存在 → 删除现有文件；
/// 否则原子写（临时文件 + rename）。非 UTF-8 文件内容经
/// [`String::from_utf8_lossy`] 有损，与 [`CheckpointManager`] 的既有口径一致。
async fn restore_state(snap: &Snapshot) -> Result<(), DeepseeknovaError> {
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
    Ok(())
}

// ---------------------------------------------------------------------------
// SessionCheckpointManager — 会话级检查点（对话 + 可选文件快照）
// ---------------------------------------------------------------------------

/// 会话级检查点里的一条对话消息角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConversationRole {
    User,
    Assistant,
    System,
}

/// 会话级检查点中的一行对话（角色 + 文本）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationLine {
    pub role: ConversationRole,
    pub text: String,
}

impl ConversationLine {
    /// 构造一条会话行。
    pub fn new(role: ConversationRole, text: impl Into<String>) -> Self {
        Self {
            role,
            text: text.into(),
        }
    }
}

/// 一个会话级检查点：对话快照（`conversation`）+ 可选文件快照（`files`）。
///
/// `files` 用于"对话 + 必要文件状态"联合回退。TUI `/checkpoint save` 当前
/// 仅快照对话（文件回退成本高，见 DESIGN 注明）；`files` 字段为程序化调用
/// （[`SessionCheckpointManager::save_with_files`]）预留。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCheckpoint {
    pub id: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 用户可选标签（`/checkpoint save <label>`）。
    pub label: Option<String>,
    pub conversation: Vec<ConversationLine>,
    pub files: Vec<Snapshot>,
}

/// 会话级检查点容量上限的默认值：超出后按 FIFO 淘汰最旧检查点。
pub const DEFAULT_MAX_SESSION_CHECKPOINTS: usize = 20;

/// 会话级检查点管理器：把对话（+ 可选文件）快照成命名检查点，支持列表
/// 与按 id（或最新）回退。持久化 JSONL，每次变更全量重写——检查点受
/// [`Self::max_checkpoints`] 上限约束（默认 20），单次操作量级很小。
pub struct SessionCheckpointManager {
    checkpoints: Vec<SessionCheckpoint>,
    persist_path: Option<PathBuf>,
    max_checkpoints: usize,
}

impl SessionCheckpointManager {
    pub fn new() -> Self {
        Self {
            checkpoints: Vec::new(),
            persist_path: None,
            max_checkpoints: DEFAULT_MAX_SESSION_CHECKPOINTS,
        }
    }

    /// 从 JSONL 文件恢复检查点（文件不存在 → 空管理器）。
    pub fn load_from(path: &Path) -> Result<Self, DeepseeknovaError> {
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
            manager.checkpoints.push(serde_json::from_str(line)?);
        }
        Ok(manager)
    }

    /// 开启持久化（路径父目录自动创建）。
    pub fn with_persistence(mut self, path: PathBuf) -> Self {
        self.persist_path = Some(path);
        self
    }

    /// 设置检查点容量上限（默认 [`crate::DEFAULT_MAX_SESSION_CHECKPOINTS`]）。
    /// 超出后按 FIFO 淘汰最旧检查点；`0` 表示任何新增检查点都会被立即淘汰。
    pub fn with_max_checkpoints(mut self, max: usize) -> Self {
        self.max_checkpoints = max;
        self
    }

    /// 当前检查点容量上限。
    pub fn max_checkpoints(&self) -> usize {
        self.max_checkpoints
    }

    /// 全量重写持久化文件（JSONL，truncate + 重写）。未配置持久化时为空操作。
    fn persist(&mut self) -> Result<(), DeepseeknovaError> {
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
        for ck in &self.checkpoints {
            writeln!(f, "{}", serde_json::to_string(ck)?)?;
        }
        Ok(())
    }

    /// 超过容量上限时按 FIFO 淘汰最旧检查点（内存 + 持久化同步）。
    fn enforce_capacity(&mut self) -> Result<(), DeepseeknovaError> {
        let evicted = self.checkpoints.len().saturating_sub(self.max_checkpoints);
        if evicted > 0 {
            self.checkpoints.drain(..evicted);
            self.persist()?;
        }
        Ok(())
    }

    /// 保存一个会话检查点（对话快照，无文件）。返回生成的检查点 id。
    pub async fn save(
        &mut self,
        conversation: Vec<ConversationLine>,
        label: Option<String>,
    ) -> Result<String, DeepseeknovaError> {
        self.save_with_files(conversation, label, &[]).await
    }

    /// 保存对话 + 文件快照（联合回退）。`paths` 中的文件在保存时快照内容。
    pub async fn save_with_files(
        &mut self,
        conversation: Vec<ConversationLine>,
        label: Option<String>,
        paths: &[&Path],
    ) -> Result<String, DeepseeknovaError> {
        let mut files = Vec::new();
        for path in paths {
            files.push(snapshot_state(path).await?);
        }
        let id = self.next_id();
        self.checkpoints.push(SessionCheckpoint {
            id: id.clone(),
            created_at: chrono::Utc::now(),
            label,
            conversation,
            files,
        });
        self.enforce_capacity()?;
        self.persist()?;
        Ok(id)
    }

    /// 生成检查点 id（`ck-YYYYMMDD-HHMMSS`，字典序即时间序）。
    fn next_id(&self) -> String {
        format!("ck-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S"))
    }

    /// 检查点列表元信息（最新优先）。
    pub fn list(&self) -> Vec<CheckpointMeta> {
        let mut metas: Vec<CheckpointMeta> = self
            .checkpoints
            .iter()
            .map(|c| CheckpointMeta {
                id: c.id.clone(),
                created_at: c.created_at,
                label: c.label.clone(),
                message_count: c.conversation.len(),
                file_count: c.files.len(),
            })
            .collect();
        metas.reverse();
        metas
    }

    /// 按 id（或最新，`id = None`）回退：恢复该检查点的文件快照并弹出，
    /// 返回检查点内容供调用方恢复对话。未知 id 返回 `Ok(None)`。
    pub async fn rollback(
        &mut self,
        id: Option<&str>,
    ) -> Result<Option<SessionCheckpoint>, DeepseeknovaError> {
        let idx = match id {
            Some(id) => match self.checkpoints.iter().position(|c| c.id == id) {
                Some(i) => Some(i),
                None => return Ok(None),
            },
            None => self.checkpoints.len().checked_sub(1),
        };
        let Some(idx) = idx else {
            return Ok(None);
        };
        let ck = self.checkpoints.remove(idx);
        // 文件恢复：单条失败不阻塞对话回退（调用方已拿到对话内容）。
        for snap in &ck.files {
            if let Err(e) = restore_state(snap).await {
                tracing::warn!(
                    "session checkpoint file rollback failed for {}: {e}",
                    snap.path.display()
                );
            }
        }
        self.persist()?;
        Ok(Some(ck))
    }

    /// 全部检查点引用（原始顺序，最旧在前）。
    pub fn checkpoints(&self) -> &[SessionCheckpoint] {
        &self.checkpoints
    }

    /// 活动检查点数。
    pub fn len(&self) -> usize {
        self.checkpoints.len()
    }

    /// 是否没有活动检查点。
    pub fn is_empty(&self) -> bool {
        self.checkpoints.is_empty()
    }

    /// 丢弃全部检查点，不恢复任何内容。
    pub fn clear(&mut self) {
        self.checkpoints.clear();
        self.persist().ok();
    }
}

impl Default for SessionCheckpointManager {
    fn default() -> Self {
        Self::new()
    }
}

/// 检查点列表元信息（TUI `/checkpoint list` 展示用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointMeta {
    pub id: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub label: Option<String>,
    pub message_count: usize,
    pub file_count: usize,
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

    // ── 会话级检查点（SessionCheckpointManager）──────────────────────

    fn sample_conversation() -> Vec<ConversationLine> {
        vec![
            ConversationLine::new(ConversationRole::User, "第一问"),
            ConversationLine::new(ConversationRole::Assistant, "第一答"),
            ConversationLine::new(ConversationRole::User, "第二问"),
        ]
    }

    #[tokio::test]
    async fn session_checkpoint_save_list_rollback_roundtrip() {
        let dir = temp_dir();
        let ck_path = dir.join("session-ck.jsonl");
        let mut ck = SessionCheckpointManager::new().with_persistence(ck_path.clone());

        let id = ck
            .save(sample_conversation(), Some("阶段一".into()))
            .await
            .unwrap();
        assert!(id.starts_with("ck-"), "id 形状: {id}");
        assert_eq!(ck.len(), 1);

        let metas = ck.list();
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].id, id);
        assert_eq!(metas[0].message_count, 3);
        assert_eq!(metas[0].label.as_deref(), Some("阶段一"));

        // 回退（最新）→ 返回检查点内容。
        let popped = ck.rollback(None).await.unwrap().expect("应弹出检查点");
        assert_eq!(popped.id, id);
        assert_eq!(popped.conversation, sample_conversation());
        assert!(ck.is_empty());
        // 回退后持久化文件同步为空。
        assert!(SessionCheckpointManager::load_from(&ck_path)
            .unwrap()
            .is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn session_checkpoint_persists_across_instances() {
        let dir = temp_dir();
        let ck_path = dir.join("session-persist.jsonl");
        {
            let mut ck = SessionCheckpointManager::new().with_persistence(ck_path.clone());
            ck.save(sample_conversation(), None).await.unwrap();
            ck.save(
                vec![ConversationLine::new(ConversationRole::Assistant, "x")],
                None,
            )
            .await
            .unwrap();
        }
        let mut ck2 = SessionCheckpointManager::load_from(&ck_path).unwrap();
        assert_eq!(ck2.len(), 2, "检查点应跨进程存活");
        let rolled = ck2.rollback(None).await.unwrap().unwrap();
        assert_eq!(rolled.conversation.len(), 1, "最新（第二条）先回退");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn session_checkpoint_rollback_by_id() {
        let dir = temp_dir();
        let mut ck = SessionCheckpointManager::new();
        let id_a = ck.save(sample_conversation(), None).await.unwrap();
        let id_b = ck
            .save(
                vec![ConversationLine::new(ConversationRole::User, "b")],
                None,
            )
            .await
            .unwrap();
        // 按 id 回退旧的 → 弹出 id_a；id_b 仍在。
        let popped = ck.rollback(Some(&id_a)).await.unwrap().unwrap();
        assert_eq!(popped.id, id_a);
        assert_eq!(ck.len(), 1);
        assert_eq!(ck.checkpoints()[0].id, id_b);
        // 未知 id → Ok(None)。
        assert!(ck.rollback(Some("ck-unknown")).await.unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn session_checkpoint_capacity_evicts_fifo() {
        let dir = temp_dir();
        let ck_path = dir.join("session-cap.jsonl");
        let mut ck = SessionCheckpointManager::new()
            .with_persistence(ck_path.clone())
            .with_max_checkpoints(2);
        let mut ids = Vec::new();
        for i in 0..3 {
            let conv = vec![ConversationLine::new(
                ConversationRole::User,
                format!("q{i}"),
            )];
            ids.push(ck.save(conv, None).await.unwrap());
        }
        // 手动顺序执行：0→1→2，超限淘汰 0，保留 1、2。
        assert_eq!(ck.len(), 2);
        assert_eq!(ck.checkpoints()[0].id, ids[1]);
        assert_eq!(ck.checkpoints()[1].id, ids[2]);
        // 持久化文件一致。
        let reloaded = SessionCheckpointManager::load_from(&ck_path).unwrap();
        assert_eq!(reloaded.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn session_checkpoint_with_files_rolls_back_files() {
        let dir = temp_dir();
        let file = dir.join("doc.txt");
        write_file(&file, "v1");
        let mut ck = SessionCheckpointManager::new();
        ck.save_with_files(sample_conversation(), None, &[&file])
            .await
            .unwrap();
        write_file(&file, "v2");
        let popped = ck.rollback(None).await.unwrap().unwrap();
        assert_eq!(popped.files.len(), 1);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "v1",
            "文件随检查点恢复"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_checkpoint_default_capacity_bounded() {
        assert_eq!(
            SessionCheckpointManager::new().max_checkpoints(),
            DEFAULT_MAX_SESSION_CHECKPOINTS
        );
        assert!(SessionCheckpointManager::new().is_empty());
    }
}

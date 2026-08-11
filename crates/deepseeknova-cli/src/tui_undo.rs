//! TUI `/undo` 的 CLI 实现：把 deepseeknova-checkpoint 的快照库适配成
//! `deepseeknova_tui::UndoController`（路径与 `checkpoint` 子命令一致）。

use async_trait::async_trait;
use deepseeknova_checkpoint::{CheckpointManager, FileDiff};
use deepseeknova_tui::UndoController;
use std::path::PathBuf;

/// 每次调用都从磁盘重新加载快照库，天然支持多进程共享与 `&self` 接口。
pub struct TuiUndoController {
    pub path: PathBuf,
}

#[async_trait]
impl UndoController for TuiUndoController {
    async fn list(&self) -> Result<Vec<String>, deepseeknova_core::DeepseeknovaError> {
        let ck = CheckpointManager::load_from(&self.path)?;
        if ck.is_empty() {
            return Ok(Vec::new());
        }
        let mut lines = Vec::new();
        for (snap, clean) in ck.verify().await? {
            let status = if clean { "unchanged" } else { "modified" };
            lines.push(format!(
                "{} [{}] {} ({})",
                if clean { "✓" } else { "✗" },
                status,
                snap.path.display(),
                &snap.hash[..8.min(snap.hash.len())]
            ));
        }
        Ok(lines)
    }

    async fn rollback_one(&self) -> Result<Option<String>, deepseeknova_core::DeepseeknovaError> {
        let mut ck = CheckpointManager::load_from(&self.path)?;
        match ck.rollback().await? {
            Some((path, hash)) => Ok(Some(format!(
                "已回滚 {} (hash {})",
                path.display(),
                &hash[..8.min(hash.len())]
            ))),
            None => Ok(None),
        }
    }

    async fn rollback_all(&self) -> Result<usize, deepseeknova_core::DeepseeknovaError> {
        let mut ck = CheckpointManager::load_from(&self.path)?;
        Ok(ck.rollback_all().await?)
    }

    async fn diffs(&self) -> Result<Vec<String>, deepseeknova_core::DeepseeknovaError> {
        let ck = CheckpointManager::load_from(&self.path)?;
        let entries = ck.diff_entries(crate::DEFAULT_DIFF_MAX_BYTES).await?;
        let mut out = Vec::new();
        for entry in entries.into_iter().flatten() {
            out.push(format!("--- {} ---", entry.path.display()));
            if entry.truncated {
                out.push(format!("(truncated, +{}/-{})", entry.added, entry.removed));
            } else {
                out.extend(diff_lines(&entry));
            }
        }
        Ok(out)
    }
}

/// 把一条 [`FileDiff`] 的 diff 文本切成行，供 TUI 逐行展示。
/// diff 文本每行已带 `+` / `-` / ` ` 前缀；`str::lines()` 不会为末尾换行
/// 产生空尾行，无需额外裁剪。
fn diff_lines(diff: &FileDiff) -> Vec<String> {
    if diff.diff_text.is_empty() {
        return Vec::new();
    }
    diff.diff_text.lines().map(str::to_string).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_file(dir: &std::path::Path, name: &str, content: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        fs::write(&path, content).unwrap();
        path
    }

    #[tokio::test]
    async fn rollback_one_restores_modified_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_file(dir.path(), "a.txt", "AAA");
        let ck_path = dir.path().join("ck.jsonl");
        {
            let mut ck = CheckpointManager::new().with_persistence(ck_path.clone());
            ck.snapshot_file(&file).await.unwrap();
        }
        fs::write(&file, "BBB").unwrap();

        let ctrl = TuiUndoController { path: ck_path };
        let lines = ctrl.list().await.unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("✗") && l.contains("modified")),
            "verify 应报告 modified: {lines:?}"
        );

        let msg = ctrl.rollback_one().await.unwrap().expect("应回滚一个快照");
        assert!(msg.contains("a.txt"));
        assert_eq!(fs::read_to_string(&file).unwrap(), "AAA");

        assert!(
            ctrl.rollback_one().await.unwrap().is_none(),
            "快照弹完后无可回滚"
        );
    }

    #[tokio::test]
    async fn rollback_all_restores_multiple_files() {
        let dir = tempfile::tempdir().unwrap();
        let f1 = write_file(dir.path(), "a.txt", "AAA");
        let f2 = write_file(dir.path(), "b.txt", "BBB");
        let ck_path = dir.path().join("ck.jsonl");
        {
            let mut ck = CheckpointManager::new().with_persistence(ck_path.clone());
            ck.snapshot_file(&f1).await.unwrap();
            ck.snapshot_file(&f2).await.unwrap();
        }
        fs::write(&f1, "X").unwrap();
        fs::write(&f2, "Y").unwrap();

        let ctrl = TuiUndoController { path: ck_path };
        assert_eq!(ctrl.rollback_all().await.unwrap(), 2);
        assert_eq!(fs::read_to_string(&f1).unwrap(), "AAA");
        assert_eq!(fs::read_to_string(&f2).unwrap(), "BBB");
    }

    #[tokio::test]
    async fn diffs_returns_content_diff_lines() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_file(dir.path(), "a.txt", "keep\nold\n");
        let ck_path = dir.path().join("ck.jsonl");
        {
            let mut ck = CheckpointManager::new().with_persistence(ck_path.clone());
            ck.snapshot_file(&file).await.unwrap();
        }
        fs::write(&file, "keep\nnew\n").unwrap();

        let ctrl = TuiUndoController { path: ck_path };
        let lines = ctrl.diffs().await.unwrap();
        assert!(lines
            .iter()
            .any(|l| l.contains("---") && l.contains("a.txt")));
        assert!(lines.iter().any(|l| l == "-old"), "应含删除行: {lines:?}");
        assert!(lines.iter().any(|l| l == "+new"), "应含新增行: {lines:?}");
    }

    #[tokio::test]
    async fn diffs_empty_when_no_changes() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_file(dir.path(), "a.txt", "same");
        let ck_path = dir.path().join("ck.jsonl");
        {
            let mut ck = CheckpointManager::new().with_persistence(ck_path.clone());
            ck.snapshot_file(&file).await.unwrap();
        }

        let ctrl = TuiUndoController { path: ck_path };
        assert!(ctrl.diffs().await.unwrap().is_empty(), "无变更应返回空");
    }
}

//! TUI `/undo` 的 CLI 实现：把 deepseeknova-checkpoint 的快照库适配成
//! `deepseeknova_tui::UndoController`（路径与 `checkpoint` 子命令一致）。

use async_trait::async_trait;
use deepseeknova_checkpoint::CheckpointManager;
use deepseeknova_tui::UndoController;
use std::path::PathBuf;

/// 每次调用都从磁盘重新加载快照库，天然支持多进程共享与 `&self` 接口。
pub struct TuiUndoController {
    pub path: PathBuf,
}

#[async_trait]
impl UndoController for TuiUndoController {
    async fn list(&self) -> anyhow::Result<Vec<String>> {
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

    async fn rollback_one(&self) -> anyhow::Result<Option<String>> {
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

    async fn rollback_all(&self) -> anyhow::Result<usize> {
        let mut ck = CheckpointManager::load_from(&self.path)?;
        ck.rollback_all().await
    }
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
}

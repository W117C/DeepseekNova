use super::*;

// ===========================================================================
// Commands — 代码改动审查 (Review)
//
// 批次 A：只读命令（get_changed_files）。
// 批次 B 的 accept/reject_file_change 在本文件追加（涉 git 写操作）。
// ===========================================================================

/// A single changed file in the working tree (staged + unstaged + untracked).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    /// "M" modified / "A" added (incl. untracked) / "D" deleted
    pub tag: String,
    pub additions: u64,
    pub deletions: u64,
}

async fn git(args: &[&str]) -> Result<String, String> {
    let output = tokio::process::Command::new("git")
        .args(args)
        .output()
        .await
        .map_err(|e| format!("git error: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// List changed files with per-file addition/deletion counts.
///
/// Combines `git status --porcelain` (tags, untracked) with
/// `git diff --numstat` + `git diff --cached --numstat` (line counts).
#[tauri::command]
pub async fn get_changed_files() -> Result<Vec<ChangedFile>, String> {
    let status = git(&["status", "--porcelain"]).await?;

    // path -> (additions, deletions)，staged 与 unstaged 数字合并
    let mut numstat: std::collections::HashMap<String, (u64, u64)> =
        std::collections::HashMap::new();
    for source in [
        git(&["diff", "--numstat"]).await.unwrap_or_default(),
        git(&["diff", "--cached", "--numstat"])
            .await
            .unwrap_or_default(),
    ] {
        for line in source.lines() {
            let mut parts = line.split('\t');
            let adds = parts.next().and_then(|s| s.parse::<u64>().ok());
            let dels = parts.next().and_then(|s| s.parse::<u64>().ok());
            if let (Some(a), Some(d), Some(path)) = (adds, dels, parts.next()) {
                let entry = numstat.entry(path.to_string()).or_insert((0, 0));
                entry.0 += a;
                entry.1 += d;
            }
        }
    }

    let mut files = Vec::new();
    for line in status.lines() {
        if line.len() < 4 {
            continue;
        }
        let (xy, rest) = line.split_at(2);
        // rename 格式 "old -> new"，取新路径
        let path = rest
            .trim()
            .rsplit(" -> ")
            .next()
            .unwrap_or(rest.trim())
            .to_string();
        let tag = if xy == "??" || xy.contains('A') {
            "A"
        } else if xy.contains('D') {
            "D"
        } else {
            "M"
        };
        let (additions, deletions) = numstat.get(&path).copied().unwrap_or((0, 0));
        files.push(ChangedFile {
            path,
            tag: tag.into(),
            additions,
            deletions,
        });
    }
    Ok(files)
}

// ---------------------------------------------------------------------------
// 批次 B：接受 / 拒绝文件改动（涉 git 写操作）
// ---------------------------------------------------------------------------

/// 路径安全校验：必须是工作区内的相对路径，且当前确实有改动。
async fn validate_changed_path(path: &str) -> Result<(), String> {
    if path.is_empty() || path.starts_with('/') || path.split('/').any(|seg| seg == "..") {
        return Err(format!("invalid path: {path}"));
    }
    let changed = get_changed_files().await?;
    if !changed.iter().any(|f| f.path == path) {
        return Err(format!("not a changed file: {path}"));
    }
    Ok(())
}

/// Accept a file change: keep it in the working tree (no-op on disk, the
/// decision is recorded by the frontend review state).
#[tauri::command]
pub async fn accept_file_change(path: String) -> Result<(), String> {
    validate_changed_path(&path).await?;
    info!("accepted file change: {path}");
    Ok(())
}

/// Reject a file change: revert it via `git stash push -- <path>`.
///
/// 用 stash 而非 `checkout --`：回滚同时保留可恢复快照（`git stash pop` 可撤销），
/// 未跟踪新文件由 `--include-untracked` 覆盖。
#[tauri::command]
pub async fn reject_file_change(path: String) -> Result<(), String> {
    validate_changed_path(&path).await?;
    let msg = format!("deepseeknova-reject {path}");
    git(&[
        "stash",
        "push",
        "--include-untracked",
        "-m",
        &msg,
        "--",
        &path,
    ])
    .await?;
    info!("rejected file change (stashed): {path}");
    Ok(())
}

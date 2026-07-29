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

/// 纯函数核心：解析 `git diff --numstat` 输出，叠加到 path -> (增, 删) 表。
/// 二进制文件（"-\t-\tpath"）无行计数，跳过。
fn merge_numstat(source: &str, numstat: &mut std::collections::HashMap<String, (u64, u64)>) {
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

/// 纯函数核心：解析 `git status --porcelain` 单行为 (路径, 标签)。
/// rename 格式 "old -> new" 取新路径；短行返回 None。
fn parse_status_line(line: &str) -> Option<(String, &'static str)> {
    if line.len() < 4 {
        return None;
    }
    let (xy, rest) = line.split_at(2);
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
    Some((path, tag))
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
        merge_numstat(&source, &mut numstat);
    }

    let mut files = Vec::new();
    for line in status.lines() {
        let Some((path, tag)) = parse_status_line(line) else {
            continue;
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

/// 纯函数核心：路径形态校验（非空、非绝对、不含 `..` 段）。
fn is_safe_relative_path(path: &str) -> bool {
    !path.is_empty() && !path.starts_with('/') && !path.split('/').any(|seg| seg == "..")
}

/// 路径安全校验：必须是工作区内的相对路径，且当前确实有改动。
async fn validate_changed_path(path: &str) -> Result<(), String> {
    if !is_safe_relative_path(path) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_status_line_classifies_tags_and_renames() {
        assert_eq!(
            parse_status_line("?? new.txt"),
            Some(("new.txt".to_string(), "A"))
        );
        assert_eq!(
            parse_status_line(" M src/lib.rs"),
            Some(("src/lib.rs".to_string(), "M"))
        );
        assert_eq!(
            parse_status_line(" D gone.rs"),
            Some(("gone.rs".to_string(), "D"))
        );
        // rename 取新路径；R 不含 A/D，归类为 M
        assert_eq!(
            parse_status_line("R  old.rs -> new.rs"),
            Some(("new.rs".to_string(), "M"))
        );
        // 短行直接丢弃
        assert_eq!(parse_status_line("M"), None);
    }

    #[test]
    fn merge_numstat_sums_sources_and_skips_binary() {
        let mut map = std::collections::HashMap::new();
        merge_numstat("3\t1\ta.rs\n-\t-\timage.png\n", &mut map);
        // staged 与 unstaged 同文件计数合并
        merge_numstat("2\t4\ta.rs\n", &mut map);
        assert_eq!(map.get("a.rs"), Some(&(5, 5)));
        assert!(!map.contains_key("image.png"));
    }

    #[test]
    fn is_safe_relative_path_rejects_escape_attempts() {
        assert!(is_safe_relative_path("src/main.rs"));
        assert!(!is_safe_relative_path(""));
        assert!(!is_safe_relative_path("/etc/passwd"));
        assert!(!is_safe_relative_path("../outside.rs"));
        assert!(!is_safe_relative_path("src/../../outside.rs"));
        // 目录名含点号但非 `..` 段，应放行
        assert!(is_safe_relative_path("src/..data/file.rs"));
    }
}

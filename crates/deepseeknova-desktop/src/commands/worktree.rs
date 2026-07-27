use super::*;

// ===========================================================================
// Commands — Git 工作树 (Worktree)
//
// list_worktrees：`git worktree list --porcelain` 解析。
// switch_worktree：一期只读，返回提示（真切换二期实现）。
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeInfo {
    pub branch: String,
    pub path: String,
    pub is_current: bool,
    pub dirty: bool,
}

#[tauri::command]
pub async fn list_worktrees() -> Result<Vec<WorktreeInfo>, String> {
    let output = tokio::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .output()
        .await
        .map_err(|e| format!("git error: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git worktree failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout).to_string();

    let cwd = std::env::current_dir()
        .ok()
        .and_then(|p| p.canonicalize().ok());

    // 当前工作树是否有改动（仅对当前 cwd 判定）
    let dirty_now = tokio::process::Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .await
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);

    let mut result = Vec::new();
    let mut path: Option<String> = None;
    let mut branch: Option<String> = None;

    let mut flush = |path: &mut Option<String>, branch: &mut Option<String>| {
        if let Some(p) = path.take() {
            let is_current = cwd
                .as_ref()
                .and_then(|c| {
                    std::path::Path::new(&p)
                        .canonicalize()
                        .ok()
                        .map(|w| c.starts_with(w))
                })
                .unwrap_or(false);
            result.push(WorktreeInfo {
                branch: branch.take().unwrap_or_else(|| "(detached)".into()),
                path: p,
                is_current,
                dirty: is_current && dirty_now,
            });
        }
    };

    for line in text.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            flush(&mut path, &mut branch);
            path = Some(p.to_string());
        } else if let Some(b) = line.strip_prefix("branch ") {
            branch = Some(b.trim_start_matches("refs/heads/").to_string());
        }
    }
    flush(&mut path, &mut branch);
    Ok(result)
}

/// 一期只读：切换工作树涉及进程工作目录与会话状态迁移，二期实现。
#[tauri::command]
pub async fn switch_worktree(branch: String) -> Result<(), String> {
    Err(format!(
        "switch_worktree('{branch}') 暂未启用：一期仅支持查看工作树列表"
    ))
}

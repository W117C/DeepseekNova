//! Worktree 管理（P2-7）：用 git worktree 提供隔离的并行会话。
//!
//! 语义：`worktree new` 在主 worktree 根的 `.deepseeknova/worktrees/<name>`
//! 下 `git worktree add` 一个隔离副本（该路径已被仓库 `.gitignore` 的
//! `.deepseeknova/*` 覆盖，不会污染主工作树状态）。在该 worktree 内启动的
//! 会话，其运行时状态（graph.db / memory.db / metrics / sessions 等）按
//! 工作区根落盘，天然隔离互不干扰。
//!
//! 全部 git 交互经 [`git_in`] 封装：std::process::Command 执行，非零退出透传
//! stderr；非 git 仓库（`git rev-parse` 失败）与 git 缺失分别给出清晰报错。
//!
//! 产生面向用户的输出的函数返回 `String`（由 main.rs 打印），便于测试断言；
//! `run_new` 返回解析后的名称与路径供调用方拼装提示。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

/// 由本 CLI 创建 worktree 的子目录（相对主 worktree 根）。已被仓库
/// `.gitignore` 的 `.deepseeknova/*` 覆盖，创建后主工作树 `git status` 不受扰。
pub const WORKTREES_SUBDIR: &str = ".deepseeknova/worktrees";

/// `worktree new` 成功的结果：实际使用的名称与 worktree 路径。
#[derive(Debug)]
pub struct NewWorktree {
    pub name: String,
    pub path: PathBuf,
    pub base: String,
}

/// 单条 git 命令输出快照。
struct GitOut {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

/// 在 `dir` 下执行一条 git 命令并捕获输出。git 缺失（不在 PATH）给出明确报错；
/// 非零退出码由调用方决定如何处理（错误透传含 stderr）。
fn git_in(dir: &Path, args: &[&str]) -> Result<GitOut, deepseeknova_core::DeepseeknovaError> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                deepseeknova_core::DeepseeknovaError::Runner(format!(
                    "`git` was not found in PATH — worktree management requires git (cwd: {})",
                    dir.display()
                ))
            } else {
                deepseeknova_core::DeepseeknovaError::Runner(format!(
                    "failed to run `git` in {}: {e}",
                    dir.display()
                ))
            }
        })?;
    Ok(GitOut {
        status: out.status,
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// 执行一条必须成功的 git 命令，返回 stdout；失败时透传 git stderr。
fn git_ok(dir: &Path, args: &[&str]) -> Result<String, deepseeknova_core::DeepseeknovaError> {
    let out = git_in(dir, args)?;
    if !out.status.success() {
        let code = out
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".to_string());
        let stderr = out.stderr.trim();
        if stderr.is_empty() {
            return Err(deepseeknova_core::DeepseeknovaError::Runner(format!(
                "git {} failed (exit {code})",
                args.first().unwrap_or(&"")
            )));
        } else {
            return Err(deepseeknova_core::DeepseeknovaError::Runner(format!(
                "git {} failed (exit {code}): {stderr}",
                args.first().unwrap_or(&"")
            )));
        }
    }
    Ok(out.stdout)
}

/// 校验 `cwd` 位于 git 仓库内（含子目录与 worktree）。`git rev-parse` 失败即
/// 非 git 仓库，给出清晰报错。
fn require_git_repo(cwd: &Path) -> Result<(), deepseeknova_core::DeepseeknovaError> {
    let out = git_in(cwd, &["rev-parse", "--is-inside-work-tree"])?;
    if !out.status.success() {
        let stderr = out.stderr.trim();
        return Err(deepseeknova_core::DeepseeknovaError::Runner(format!(
            "not a git repository (or any parent directory): {} — run `git init` first{}",
            cwd.display(),
            if stderr.is_empty() {
                String::new()
            } else {
                format!(" ({stderr})")
            }
        )));
    }
    Ok(())
}

/// 主 worktree 根：`git rev-parse --git-common-dir` 的父目录。无论从主工作树、
/// 子目录还是已链接 worktree 内调用，都解析到同一主根，保证 CLI 创建的
/// worktree 集中在同一处。
fn main_worktree_root(cwd: &Path) -> Result<PathBuf, deepseeknova_core::DeepseeknovaError> {
    let common = git_ok(cwd, &["rev-parse", "--git-common-dir"])?;
    let common_path = PathBuf::from(common.trim());
    let abs = if common_path.is_absolute() {
        common_path
    } else {
        cwd.join(common_path)
    };
    abs.parent().map(|p| p.to_path_buf()).ok_or_else(|| {
        deepseeknova_core::DeepseeknovaError::Config(format!(
            "cannot locate main worktree root from {}",
            cwd.display()
        ))
    })
}

/// 本 CLI 管理的 worktree 基目录（主 worktree 根的 `.deepseeknova/worktrees`）。
///
/// git（如 `worktree list --porcelain`）会解析符号链接规范化路径（macOS 上
/// `/var` → `/private/var`），此处同步 canonicalize，保证 `starts_with` /
/// 相等比较两侧一致；解析失败退回原值。
fn worktrees_base(cwd: &Path) -> Result<PathBuf, deepseeknova_core::DeepseeknovaError> {
    Ok(canon(&main_worktree_root(cwd)?.join(WORKTREES_SUBDIR)))
}

/// 校验 worktree 名可安全用作目录名（路径分量合法性；git 分支名由
/// `check-ref-format` 另行校验）。
fn validate_name(name: &str) -> Result<(), deepseeknova_core::DeepseeknovaError> {
    if name.is_empty() {
        return Err(deepseeknova_core::DeepseeknovaError::Config(
            "worktree name must not be empty".to_string(),
        ));
    }
    if name == "." || name == ".." {
        return Err(deepseeknova_core::DeepseeknovaError::Config(
            "worktree name must not be `.` or `..`".to_string(),
        ));
    }
    if name.contains(['/', '\\']) {
        return Err(deepseeknova_core::DeepseeknovaError::Config(format!(
            "worktree name must not contain path separators: `{name}`"
        )));
    }
    if name.chars().any(char::is_whitespace) {
        return Err(deepseeknova_core::DeepseeknovaError::Config(format!(
            "worktree name must not contain whitespace: `{name}`"
        )));
    }
    Ok(())
}

/// 校验 worktree 名可作为 git 分支名（`refs/heads/<name>` 合法）。
fn validate_branch_name(
    cwd: &Path,
    name: &str,
) -> Result<(), deepseeknova_core::DeepseeknovaError> {
    let out = git_in(cwd, &["check-ref-format", &format!("refs/heads/{name}")])?;
    if !out.status.success() {
        return Err(deepseeknova_core::DeepseeknovaError::Config(format!(
            "invalid worktree name `{name}`: not a valid git branch name"
        )));
    }
    Ok(())
}

/// 缺省 worktree 名：`wt-<ts>-<seq>`，与 `cli_session_label`（`session-<ts>-<seq>`）
/// 风格一致，同一秒内多次创建也唯一。
fn default_name() -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "wt-{}-{}",
        chrono::Utc::now().format("%Y%m%d-%H%M%S"),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// `git worktree list --porcelain` 的单个条目。
#[derive(Debug)]
struct WorktreeInfo {
    path: PathBuf,
    /// 分支全名（如 `refs/heads/foo`）；None = detached。
    branch: Option<String>,
}

/// 解析 `git worktree list --porcelain` 输出（条目以空行分隔；忽略
/// detached/bare/locked/prunable 标记行）。
fn parse_worktree_list(output: &str) -> Vec<WorktreeInfo> {
    let mut out = Vec::new();
    let mut cur: Option<WorktreeInfo> = None;
    for line in output.lines() {
        if line.is_empty() {
            if let Some(info) = cur.take() {
                out.push(info);
            }
            continue;
        }
        if let Some(p) = line.strip_prefix("worktree ") {
            cur = Some(WorktreeInfo {
                path: PathBuf::from(p),
                branch: None,
            });
        } else if let Some(b) = line.strip_prefix("branch ") {
            if let Some(info) = cur.as_mut() {
                info.branch = Some(b.to_string());
            }
        }
    }
    if let Some(info) = cur.take() {
        out.push(info);
    }
    out
}

/// 最佳努力规范化（canonicalize）用于相等比较；失败时退回原值。
fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// `worktree new [--name <name>] [--base <ref>]`：创建 git worktree。
///
/// 默认 `git worktree add -b <name> <dest> HEAD` 到主根
/// `.deepseeknova/worktrees/<name>`；`--base` 指定基础 ref。成功后返回实际
/// 名称与路径（main.rs 负责打印"在该 worktree 内启动隔离会话"的指引）。
pub fn run_new(
    cwd: &Path,
    name: Option<&str>,
    base: Option<&str>,
) -> Result<NewWorktree, deepseeknova_core::DeepseeknovaError> {
    require_git_repo(cwd)?;
    let name = name.map(str::to_string).unwrap_or_else(default_name);
    validate_name(&name)?;
    validate_branch_name(cwd, &name)?;

    let dest = worktrees_base(cwd)?.join(&name);
    if dest.exists() {
        return Err(deepseeknova_core::DeepseeknovaError::Config(format!(
            "worktree `{name}` already exists at {}",
            dest.display()
        )));
    }

    let base_ref = base.unwrap_or("HEAD").to_string();
    let dest_str = dest.to_string_lossy().into_owned();
    let out = git_in(cwd, &["worktree", "add", "-b", &name, &dest_str, &base_ref])?;
    if !out.status.success() {
        let code = out
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".to_string());
        let stderr = out.stderr.trim();
        if stderr.is_empty() {
            return Err(deepseeknova_core::DeepseeknovaError::Runner(format!(
                "git worktree add failed (exit {code})"
            )));
        } else {
            return Err(deepseeknova_core::DeepseeknovaError::Runner(format!(
                "git worktree add failed (exit {code}): {stderr}"
            )));
        }
    }

    Ok(NewWorktree {
        name,
        path: dest,
        base: base_ref,
    })
}

/// `worktree list`：渲染全部 git worktree（路径 / 分支 / 当前标记 / \[cli\] 标记）。
pub fn run_list(cwd: &Path) -> Result<String, deepseeknova_core::DeepseeknovaError> {
    require_git_repo(cwd)?;
    let out = git_ok(cwd, &["worktree", "list", "--porcelain"])?;
    let infos = parse_worktree_list(&out);
    let current = canon(&PathBuf::from(
        git_ok(cwd, &["rev-parse", "--show-toplevel"])?.trim(),
    ));
    let base = worktrees_base(cwd)?;

    if infos.is_empty() {
        return Ok("(no worktrees)".to_string());
    }
    let mut lines = vec!["Worktrees:".to_string()];
    for info in &infos {
        let path = canon(&info.path);
        let mark = if path == current { "*" } else { " " };
        let branch = info
            .branch
            .as_deref()
            .map(|b| b.strip_prefix("refs/heads/").unwrap_or(b).to_string())
            .unwrap_or_else(|| "detached".to_string());
        let cli = if info.path.starts_with(&base) {
            " [cli]"
        } else {
            ""
        };
        let cur = if path == current { " (current)" } else { "" };
        lines.push(format!(
            "{mark} {}  {branch}{cli}{cur}",
            info.path.display()
        ));
    }
    lines.push(String::new());
    lines.push(
        "* = current worktree · [cli] = managed by this CLI (use `worktree switch 名称` to cd)"
            .to_string(),
    );
    Ok(lines.join("\n"))
}

/// `worktree switch 名称`：返回目标目录供用户 cd 进入（CLI 无法改变父进程
/// 的 cwd，故只引导）。
pub fn run_switch(cwd: &Path, name: &str) -> Result<String, deepseeknova_core::DeepseeknovaError> {
    require_git_repo(cwd)?;
    validate_name(name)?;
    let dest = worktrees_base(cwd)?.join(name);
    if !dest.is_dir() {
        return Err(deepseeknova_core::DeepseeknovaError::Config(format!(
            "worktree `{name}` not found at {}",
            dest.display()
        )));
    }
    let out = git_in(&dest, &["rev-parse", "--is-inside-work-tree"])?;
    if !out.status.success() {
        return Err(deepseeknova_core::DeepseeknovaError::Runner(format!(
            "`{}` exists but is not a git worktree",
            dest.display()
        )));
    }
    Ok(format!(
        "worktree `{name}` is at:\n  {}\n\ncd into it and start an isolated session:\n  cd {}\n  deepseeknova chat --tui",
        dest.display(),
        dest.display()
    ))
}

/// `worktree delete 名称 [--force]`：删除 worktree。有未提交/未跟踪变更时
/// 拒绝（除非 `--force`）；成功删除后保留分支（提示用 `git branch -D` 清理）。
/// 返回成功提示文本。
pub fn run_delete(
    cwd: &Path,
    name: &str,
    force: bool,
) -> Result<String, deepseeknova_core::DeepseeknovaError> {
    require_git_repo(cwd)?;
    validate_name(name)?;
    let dest = worktrees_base(cwd)?.join(name);
    if !dest.is_dir() {
        return Err(deepseeknova_core::DeepseeknovaError::Config(format!(
            "worktree `{name}` not found at {}",
            dest.display()
        )));
    }

    let out = git_in(&dest, &["status", "--porcelain"])?;
    if !out.status.success() {
        return Err(deepseeknova_core::DeepseeknovaError::Runner(format!(
            "cannot inspect worktree `{name}` ({}): {}",
            dest.display(),
            out.stderr.trim()
        )));
    }
    if !out.stdout.trim().is_empty() && !force {
        let count = out.stdout.lines().count();
        return Err(deepseeknova_core::DeepseeknovaError::Runner(format!(
            "worktree `{name}` has {count} uncommitted change(s) — commit or stash them first, \
             or pass --force to discard"
        )));
    }

    let dest_str = dest.to_string_lossy().into_owned();
    let mut args: Vec<&str> = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&dest_str);
    let out = git_in(cwd, &args)?;
    if !out.status.success() {
        let stderr = out.stderr.trim();
        return Err(deepseeknova_core::DeepseeknovaError::Runner(format!(
            "git worktree remove failed (exit {}): {}",
            out.status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".to_string()),
            if stderr.is_empty() {
                "unknown reason"
            } else {
                stderr
            }
        )));
    }

    Ok(format!(
        "✓ removed worktree `{name}`\n  branch `{name}` was kept — delete it with `git branch -D {name}` if desired"
    ))
}

/// `worktree clean`：清理主根 `.deepseeknova/worktrees/` 下所有由本 CLI 创建的
/// worktree。有未提交变更的跳过并报告（不强制删除）。目录中 git 未登记的
/// 残留（如失败的 add）仅提示，不自动删除。
pub fn run_clean(cwd: &Path) -> Result<String, deepseeknova_core::DeepseeknovaError> {
    require_git_repo(cwd)?;
    let base = worktrees_base(cwd)?;
    if !base.is_dir() {
        return Ok(format!("no CLI worktrees to clean ({})", base.display()));
    }

    let out = git_ok(cwd, &["worktree", "list", "--porcelain"])?;
    let managed: Vec<WorktreeInfo> = parse_worktree_list(&out)
        .into_iter()
        .filter(|e| e.path.starts_with(&base))
        .collect();

    let mut removed = 0usize;
    let mut kept: Vec<(PathBuf, String)> = Vec::new();
    for info in &managed {
        let name = info
            .path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        match run_delete(cwd, &name, false) {
            Ok(msg) => {
                removed += 1;
                println!("{msg}");
            }
            Err(e) => kept.push((info.path.clone(), e.to_string())),
        }
    }
    let mut lines = vec![format!(
        "cleaned {removed} worktree(s) under {}",
        base.display()
    )];
    for (p, err) in &kept {
        lines.push(format!("  kept {}: {err}", p.display()));
    }

    // git 未登记的残留目录：仅提示，不自动删除（避免误删用户文件）。
    let registered: HashSet<PathBuf> = managed.iter().map(|i| i.path.clone()).collect();
    let mut orphans: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&base) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() && !registered.contains(&p) {
                orphans.push(p);
            }
        }
    }
    if !orphans.is_empty() {
        lines.push(String::new());
        lines.push(
            "leftover directories (not registered by git) — remove manually if unused:".to_string(),
        );
        for p in &orphans {
            lines.push(format!("  {}", p.display()));
        }
    }
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 建一个含一次提交的真实 git 仓库（tempfile::tempdir 持有自动清理）。
    fn temp_git_repo(tag: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .unwrap_or_else(|e| panic!("git must run in tests ({tag}): {e}"));
            assert!(
                out.status.success(),
                "git {args:?} failed ({tag}): {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "test"]);
        std::fs::write(dir.path().join("a.txt"), "v1\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "initial"]);
        dir
    }

    #[test]
    // Windows 不支持：canonicalize 产生 `\\?\` 扩展路径前缀，git worktree add
    // 无法创建（平台能力未排期，见 BLOCKED「Windows 沙箱排期」），Unix 语义
    // 由本测试锁定。
    #[cfg(unix)]
    fn new_list_switch_delete_roundtrip() {
        let repo = temp_git_repo("roundtrip");
        let root = repo.path().to_path_buf();

        let created = run_new(&root, Some("feat"), None).unwrap();
        assert_eq!(created.name, "feat");
        assert!(created.path.is_dir(), "worktree dir must exist");
        assert_eq!(
            created.path,
            root.join(WORKTREES_SUBDIR).join("feat"),
            "default location must be .deepseeknova/worktrees/<name>"
        );
        // 新 worktree 内确实是独立工作副本。
        let in_wt = git_in(&created.path, &["rev-parse", "--is-inside-work-tree"]).unwrap();
        assert!(in_wt.status.success());

        // list 包含主工作树与新建 worktree（带 \[cli\] 标记）。
        let listed = run_list(&root).unwrap();
        assert!(
            listed.contains("feat"),
            "list must include new worktree: {listed}"
        );
        assert!(
            listed.contains("[cli]"),
            "CLI-created worktree must be marked: {listed}"
        );

        // switch 打印目标目录。
        let switched = run_switch(&root, "feat").unwrap();
        assert!(
            switched.contains(created.path.to_string_lossy().as_ref()),
            "switch must print the target dir: {switched}"
        );

        // delete 后目录消失。
        let msg = run_delete(&root, "feat", false).unwrap();
        assert!(msg.contains("removed worktree `feat`"));
        assert!(
            !created.path.exists(),
            "worktree dir must be gone after delete"
        );
        // worktree 登记消失（分支保留，由提示说明）。
        let out = git_ok(&root, &["worktree", "list", "--porcelain"]).unwrap();
        assert!(
            !out.contains("worktrees/feat"),
            "worktree must be unregistered"
        );
    }

    #[test]
    fn default_name_is_unique_and_valid() {
        let a = default_name();
        let b = default_name();
        assert_ne!(a, b, "same-second creations must be unique");
        for n in [&a, &b] {
            assert!(n.starts_with("wt-"));
            assert!(n
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        }
    }

    #[test]
    fn not_a_git_repo_errors() {
        let dir = tempfile::tempdir().unwrap();
        let err = run_new(dir.path(), Some("x"), None).unwrap_err();
        assert!(
            err.to_string().contains("not a git repository"),
            "non-git dir must fail with clear error, got: {err}"
        );
        let err = run_list(dir.path()).unwrap_err();
        assert!(err.to_string().contains("not a git repository"));
        let err = run_delete(dir.path(), "x", false).unwrap_err();
        assert!(err.to_string().contains("not a git repository"));
    }

    #[test]
    fn duplicate_creation_conflicts() {
        let repo = temp_git_repo("dup");
        let root = repo.path().to_path_buf();
        run_new(&root, Some("same"), None).unwrap();
        let err = run_new(&root, Some("same"), None).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("already exists") || msg.contains("already"),
            "duplicate name must conflict, got: {msg}"
        );
    }

    #[test]
    fn delete_with_changes_refused_unless_force() {
        let repo = temp_git_repo("dirty");
        let root = repo.path().to_path_buf();
        let created = run_new(&root, Some("dirty"), None).unwrap();
        std::fs::write(created.path.join("a.txt"), "v2 modified\n").unwrap();

        let err = run_delete(&root, "dirty", false).unwrap_err();
        assert!(
            err.to_string().contains("uncommitted change"),
            "dirty delete must be refused, got: {err}"
        );
        assert!(
            created.path.exists(),
            "refused delete must leave worktree intact"
        );

        // --force 丢弃变更后成功删除。
        run_delete(&root, "dirty", true).unwrap();
        assert!(!created.path.exists());
    }

    #[test]
    // Windows 不支持：同上（`\\?\` 扩展路径导致 git worktree add 失败）。
    #[cfg(unix)]
    fn clean_removes_managed_worktrees_and_skips_dirty() {
        let repo = temp_git_repo("clean");
        let root = repo.path().to_path_buf();
        let clean_dest = run_new(&root, Some("clean-a"), None).unwrap();
        let dirty_dest = run_new(&root, Some("clean-b"), None).unwrap();
        std::fs::write(dirty_dest.path.join("a.txt"), "dirty\n").unwrap();

        let out = run_clean(&root).unwrap();
        assert!(
            out.contains("cleaned 1"),
            "one clean worktree removed: {out}"
        );
        assert!(
            !clean_dest.path.exists(),
            "clean must remove clean worktree"
        );
        assert!(dirty_dest.path.exists(), "clean must skip dirty worktree");
        assert!(
            out.contains("kept") && out.contains("clean-b"),
            "clean must report the kept worktree: {out}"
        );
    }

    #[test]
    fn invalid_names_rejected() {
        let repo = temp_git_repo("badname");
        let root = repo.path().to_path_buf();
        for bad in ["", ".", "..", "a/b", "a\\b", "has space"] {
            let err = run_new(&root, Some(bad), None).unwrap_err();
            assert!(
                err.to_string().contains("name"),
                "name `{bad:?}` must be rejected, got: {err}"
            );
        }
    }

    #[test]
    fn base_ref_creates_worktree_at_given_ref() {
        let repo = temp_git_repo("base");
        let root = repo.path().to_path_buf();
        // 打一个 tag 作为 base 目标。
        let out = Command::new("git")
            .args(["tag", "v1"])
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(out.status.success());
        let created = run_new(&root, Some("from-tag"), Some("v1")).unwrap();
        assert_eq!(created.base, "v1");
        let head = git_ok(&created.path, &["rev-parse", "HEAD"]).unwrap();
        let tag_head = git_ok(&root, &["rev-parse", "v1"]).unwrap();
        assert_eq!(
            head.trim(),
            tag_head.trim(),
            "worktree HEAD must match base ref"
        );
    }
}

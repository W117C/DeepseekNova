use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

/// Normalize path components by resolving `.` and `..` without hitting the disk.
pub fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::CurDir => {}
            _ => {
                normalized.push(component);
            }
        }
    }
    normalized
}

/// Resolve and validate a user-supplied path against a root workspace directory,
/// preventing path traversal and symlink escape attacks.
pub fn secure_resolve(root: &Path, input: &Path) -> Result<PathBuf> {
    // Resolve absolute path or join with root if relative
    let joined = if input.is_absolute() {
        input.to_path_buf()
    } else {
        root.join(input)
    };

    // 1. Raw memory normalization check (stops basic .. traversal)
    let normalized = normalize_path(&joined);

    if !normalized.starts_with(root) {
        tracing::warn!(
            security_event = "path_escape_attempt",
            requested = ?input.display(),
            resolved = ?normalized.display(),
            workspace = ?root.display(),
            reason = "escapes workspace root via raw memory check"
        );
        bail!(
            "path escapes workspace root (normalization check): {:?}",
            input
        );
    }

    // 2. Canonicalize the closest existing ancestor (or symlink) to prevent symlink escape
    let mut current = normalized.as_path();
    while !std::fs::symlink_metadata(current).is_ok() {
        if let Some(parent) = current.parent() {
            current = parent;
        } else {
            break;
        }
    }

    if std::fs::symlink_metadata(current).is_ok() {
        let canonical = match std::fs::canonicalize(current) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    security_event = "path_validation_failed",
                    requested = ?input.display(),
                    error = %e,
                    reason = "canonicalize failed"
                );
                return Err(e.into());
            }
        };
        // On Windows, canonicalize returns a UNC path (\\?\C:\...).
        // Normalize root the same way for a fair comparison.
        let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        if !canonical.starts_with(&canonical_root) {
            tracing::warn!(
                security_event = "path_escape_attempt",
                requested = ?input.display(),
                resolved = ?canonical.display(),
                workspace = ?canonical_root.display(),
                reason = "escapes workspace root via symlink"
            );
            bail!("path escapes workspace root via symlink: {:?}", input);
        }
    }

    Ok(normalized)
}

/// Basic path sanitization: reject obviously malicious paths
/// and ensure the path stays within the workspace root.
pub fn sanitize_path(workspace: &Path, raw: &str) -> anyhow::Result<PathBuf> {
    if raw.is_empty() {
        bail!("empty path");
    }
    if raw.contains('\0') {
        bail!("path contains null byte");
    }
    secure_resolve(workspace, Path::new(raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── normalize_path ───────────────────────────────────────────

    #[test]
    fn test_normalize_path_noop() {
        let p = normalize_path(Path::new("/a/b/c"));
        assert_eq!(p, PathBuf::from("/a/b/c"));
    }

    #[test]
    fn test_normalize_path_removes_dot() {
        let p = normalize_path(Path::new("/a/./b"));
        assert_eq!(p, PathBuf::from("/a/b"));
    }

    #[test]
    fn test_normalize_path_resolves_dotdot() {
        let p = normalize_path(Path::new("/a/b/../c"));
        assert_eq!(p, PathBuf::from("/a/c"));
    }

    #[test]
    fn test_normalize_path_escape_attempt() {
        let p = normalize_path(Path::new("/a/b/../../etc/passwd"));
        assert_eq!(p, PathBuf::from("/etc/passwd"));
    }

    // ── secure_resolve ───────────────────────────────────────────

    #[test]
    fn test_secure_resolve_normal_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let res = secure_resolve(&root, Path::new("src/main.rs")).unwrap();
        assert_eq!(res, root.join("src/main.rs"));
    }

    #[test]
    fn test_secure_resolve_rejects_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let res = secure_resolve(&root, Path::new("../../etc/passwd"));
        assert!(res.is_err(), "should block traversal escape");
    }

    #[test]
    fn test_secure_resolve_rejects_absolute_path_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let res = secure_resolve(&root, Path::new("/etc"));
        assert!(res.is_err(), "should block absolute path escape");
    }

    #[test]
    fn test_secure_resolve_allows_deep_nested_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        // Create deep nested dir
        let deep = root.join("a/b/c/d");
        std::fs::create_dir_all(&deep).unwrap();
        let res = secure_resolve(&root, Path::new("a/b/c/d/file.txt")).unwrap();
        assert_eq!(res, deep.join("file.txt"));
    }

    #[cfg(unix)]
    #[test]
    fn test_secure_resolve_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();

        let outside_tmp = tempfile::tempdir().unwrap();
        let outside_path = std::fs::canonicalize(outside_tmp.path()).unwrap();
        let link_path = root.join("bad_symlink");

        if symlink(&outside_path, &link_path).is_ok() {
            let res = secure_resolve(&root, Path::new("bad_symlink/some_file"));
            assert!(res.is_err(), "should block symlink escape: {:?}", res);
        }
    }

    // ── sanitize_path ────────────────────────────────────────────

    #[test]
    fn test_sanitize_path_rejects_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let res = sanitize_path(tmp.path(), "");
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("empty path"));
    }

    #[test]
    fn test_sanitize_path_rejects_null_byte() {
        let tmp = tempfile::tempdir().unwrap();
        let res = sanitize_path(tmp.path(), "safe\0.txt");
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("null byte"));
    }

    #[test]
    fn test_sanitize_path_accepts_valid_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let res = sanitize_path(&root, "valid/file.txt").unwrap();
        assert_eq!(res, root.join("valid/file.txt"));
    }
}

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
        if !canonical.starts_with(root) {
            tracing::warn!(
                security_event = "path_escape_attempt",
                requested = ?input.display(),
                resolved = ?canonical.display(),
                workspace = ?root.display(),
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

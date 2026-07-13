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
        bail!(
            "path escapes workspace root (normalization check): {:?}",
            input
        );
    }

    // 2. Canonicalize the closest existing ancestor to prevent symlink escape
    let mut current = normalized.as_path();
    while !current.exists() {
        if let Some(parent) = current.parent() {
            current = parent;
        } else {
            break;
        }
    }

    if current.exists() {
        let canonical = std::fs::canonicalize(current)?;
        if !canonical.starts_with(root) {
            bail!("path escapes workspace root via symlink: {:?}", input);
        }
    }

    Ok(normalized)
}

//! File discovery + line-by-line regex matching (zero AI).

use crate::finding::Finding;
use crate::rule::Rule;
use deepseeknova_graph::parser::Lang;
use std::path::Path;
use walkdir::WalkDir;

/// Directory names always excluded from scanning.
const HARD_EXCLUDES: &[&str] = &["target", "node_modules", ".git", "dist"];

/// Max file size scanned (bytes); larger files are skipped.
const MAX_FILE_BYTES: u64 = 1_000_000;

/// Scan every supported source file under `root`, returning findings with no
/// verdict. Unreadable / non-UTF-8 / oversized files are skipped silently.
pub fn scan_files(root: &Path, rules: &[Rule]) -> anyhow::Result<Vec<Finding>> {
    let ignores = load_gitignore(root);
    let mut out = Vec::new();
    for entry in WalkDir::new(root).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if is_excluded(root, path, &ignores) {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        let lang = match Lang::from_path(&rel) {
            Some(l) => l,
            None => continue,
        };
        if entry.metadata().map(|m| m.len()).unwrap_or(0) > MAX_FILE_BYTES {
            continue;
        }
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        scan_content(&rel, lang, &content, rules, &mut out);
    }
    Ok(out)
}

fn scan_content(rel: &str, lang: Lang, content: &str, rules: &[Rule], out: &mut Vec<Finding>) {
    for (idx, line) in content.lines().enumerate() {
        for rule in rules {
            if let Some(rl) = rule.lang {
                if rl != lang {
                    continue;
                }
            }
            if rule.pattern.is_match(line) {
                out.push(Finding {
                    rule_id: rule.id.clone(),
                    severity: rule.severity,
                    path: rel.to_string(),
                    line: idx + 1,
                    excerpt: line.trim().chars().take(200).collect(),
                    verdict: None,
                });
            }
        }
    }
}

fn is_excluded(root: &Path, path: &Path, ignores: &[String]) -> bool {
    let rel = path.strip_prefix(root).unwrap_or(path);
    for comp in rel.components() {
        let c = comp.as_os_str().to_string_lossy();
        if HARD_EXCLUDES.contains(&c.as_ref()) {
            return true;
        }
    }
    matches_gitignore(rel.to_string_lossy().as_ref(), ignores)
}

/// Minimal .gitignore prefix match. OS path separators are normalized to `/`
/// so patterns like `ignored/` also exclude `ignored\x.rs` on Windows.
fn matches_gitignore(rel: &str, ignores: &[String]) -> bool {
    let rel = rel.replace('\\', "/");
    ignores.iter().any(|ig| {
        let ig = ig.trim_end_matches('/');
        !ig.is_empty()
            && (rel == ig || rel.starts_with(&format!("{ig}/")) || rel.contains(&format!("/{ig}/")))
    })
}

/// Minimal .gitignore reader: non-comment, non-empty lines (prefix match).
fn load_gitignore(root: &Path) -> Vec<String> {
    std::fs::read_to_string(root.join(".gitignore"))
        .map(|s| {
            s.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::builtin_rules;

    fn tmp_with(files: &[(&str, &str)]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let root = dir.path().to_path_buf();
        for (rel, body) in files {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, body).unwrap();
        }
        (dir, root)
    }

    #[test]
    fn scans_secret_and_reports_line() {
        let (_dir, root) = tmp_with(&[(
            "src/config.rs",
            "fn a() {}\nlet api_key = \"sk-abcdefgh\";\n",
        )]);
        let findings = scan_files(&root, &builtin_rules()).unwrap();
        let f = findings
            .iter()
            .find(|f| f.rule_id == "hardcoded-secret")
            .unwrap();
        assert_eq!(f.line, 2, "line is 1-based");
        assert!(f.verdict.is_none(), "scan stage sets no verdict");
    }

    #[test]
    fn skips_gitignored_and_target() {
        let (_dir, root) = tmp_with(&[
            (".gitignore", "ignored/\n"),
            ("ignored/x.rs", "let secret = \"sk-longenough\";\n"),
            ("target/y.rs", "let secret = \"sk-longenough\";\n"),
        ]);
        let findings = scan_files(&root, &builtin_rules()).unwrap();
        assert!(findings.is_empty(), "gitignored + target excluded");
    }

    #[test]
    fn gitignore_matches_on_component_boundary_not_substring() {
        let (_dir, root) = tmp_with(&[
            (".gitignore", "dist/\n"),
            // 同前缀但不同组件的文件不应被排除
            ("distutils/x.rs", "let api_key = \"sk-abcdefgh\";\n"),
            // 真正在 dist/ 下的应被排除
            ("dist/y.rs", "let api_key = \"sk-abcdefgh\";\n"),
        ]);
        let findings = scan_files(&root, &builtin_rules()).unwrap();
        assert!(
            findings.iter().any(|f| f.path.contains("distutils")),
            "sibling dir sharing a prefix must still be scanned"
        );
        assert!(
            !findings.iter().any(|f| f.path.starts_with("dist/")),
            "真正 gitignored 的目录仍排除"
        );
    }

    #[test]
    fn gitignore_matches_windows_backslash_paths() {
        let ignores = vec!["ignored/".to_string()];
        let dist_ignore = ["dist/".to_string()];
        assert!(
            matches_gitignore(r"ignored\x.rs", &ignores),
            "backslash-separated rel path must match the gitignore dir"
        );
        assert!(
            !matches_gitignore(r"distutils\x.rs", &dist_ignore),
            "sibling dir sharing a prefix must not match on Windows-style paths either"
        );
    }

    #[test]
    fn rust_rule_skips_python_file() {
        let (_dir, root) = tmp_with(&[("a.py", "x = foo().unwrap()\n")]);
        let findings = scan_files(&root, &builtin_rules()).unwrap();
        assert!(
            !findings.iter().any(|f| f.rule_id == "rust-unwrap"),
            "rust-scoped rule must not fire on .py"
        );
    }
}

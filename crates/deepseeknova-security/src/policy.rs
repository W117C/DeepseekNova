use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct SecurityPolicy {
    pub allowed_paths: Vec<PathBuf>,
    pub denied_paths: Vec<PathBuf>,
    pub allowed_commands: Vec<String>,
    pub allowed_domains: Vec<String>,
}

impl SecurityPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    /// 是否配置了任何约束（路径/命令/域名任一非空）。
    ///
    /// 语义契约：**空列表 = 未配置 = 全放**（fail-open）。这是刻意权衡：
    /// 策略对象默认宽松，安全边界由调用方（CLI/serve）按场景显式收紧。
    /// 需要 fail-closed 的调用方应先检查 `is_configured()`，对未配置场景
    /// 自行拒绝，而不是依赖本结构内建默认拒绝。
    pub fn is_configured(&self) -> bool {
        !self.allowed_paths.is_empty()
            || !self.denied_paths.is_empty()
            || !self.allowed_commands.is_empty()
            || !self.allowed_domains.is_empty()
    }

    pub fn is_path_allowed(&self, path: &Path) -> bool {
        // Denied paths take precedence
        for denied in &self.denied_paths {
            if path.starts_with(denied) {
                return false;
            }
        }

        // If allowed list is not empty, path must match at least one allowed path prefix
        if !self.allowed_paths.is_empty() {
            let mut allowed = false;
            for ok_path in &self.allowed_paths {
                if path.starts_with(ok_path) {
                    allowed = true;
                    break;
                }
            }
            if !allowed {
                return false;
            }
        }

        true
    }

    pub fn is_command_allowed(&self, command: &str) -> bool {
        if self.allowed_commands.is_empty() {
            return true;
        }
        self.allowed_commands
            .iter()
            .any(|cmd| command.starts_with(cmd))
    }

    /// 域名是否允许：精确匹配或子域匹配（`example.com` 覆盖 `sub.example.com`）。
    ///
    /// 与 Claude Code 的 `WebFetch(domain:)` 前缀语义对齐。条目 `example.com`
    /// 可放行任意子域；条目本身是子域（`api.example.com`）时只精确匹配自己。
    /// 空列表 = 未配置 = 全放（见 [`SecurityPolicy::is_configured`]）。
    pub fn is_domain_allowed(&self, domain: &str) -> bool {
        if self.allowed_domains.is_empty() {
            return true;
        }
        self.allowed_domains
            .iter()
            .any(|d| d == domain || domain.ends_with(&format!(".{d}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_path_allowed ──────────────────────────────────────────

    #[test]
    fn test_path_allowed_when_no_lists_configured() {
        let policy = SecurityPolicy::new();
        assert!(policy.is_path_allowed(Path::new("/any/path")));
    }

    #[test]
    fn test_path_allowed_when_in_allowed_list() {
        let policy = SecurityPolicy {
            allowed_paths: vec![PathBuf::from("/safe")],
            ..SecurityPolicy::default()
        };
        assert!(policy.is_path_allowed(Path::new("/safe/dir/file.txt")));
    }

    #[test]
    fn test_path_denied_when_not_in_allowed_list() {
        let policy = SecurityPolicy {
            allowed_paths: vec![PathBuf::from("/safe")],
            ..SecurityPolicy::default()
        };
        assert!(!policy.is_path_allowed(Path::new("/etc/passwd")));
    }

    #[test]
    fn test_path_denied_when_in_denied_list() {
        let policy = SecurityPolicy {
            denied_paths: vec![PathBuf::from("/secret")],
            ..SecurityPolicy::default()
        };
        assert!(!policy.is_path_allowed(Path::new("/secret/data")));
    }

    #[test]
    fn test_denied_takes_precedence_over_allowed() {
        let policy = SecurityPolicy {
            allowed_paths: vec![PathBuf::from("/data")],
            denied_paths: vec![PathBuf::from("/data/secret")],
            ..SecurityPolicy::default()
        };
        assert!(policy.is_path_allowed(Path::new("/data/public")));
        assert!(!policy.is_path_allowed(Path::new("/data/secret/doc")));
    }

    // ── is_command_allowed ───────────────────────────────────────

    #[test]
    fn test_command_allowed_when_no_list() {
        let policy = SecurityPolicy::new();
        assert!(policy.is_command_allowed("rm -rf /"));
    }

    #[test]
    fn test_command_allowed_by_prefix() {
        let policy = SecurityPolicy {
            allowed_commands: vec!["cargo".into(), "git".into()],
            ..SecurityPolicy::default()
        };
        assert!(policy.is_command_allowed("cargo build"));
        assert!(policy.is_command_allowed("git push"));
    }

    #[test]
    fn test_command_denied_when_not_in_list() {
        let policy = SecurityPolicy {
            allowed_commands: vec!["cargo".into()],
            ..SecurityPolicy::default()
        };
        assert!(!policy.is_command_allowed("rm -rf /"));
        assert!(!policy.is_command_allowed("python3 script.py"));
    }

    // ── is_domain_allowed ────────────────────────────────────────

    #[test]
    fn test_domain_allowed_when_no_list() {
        let policy = SecurityPolicy::new();
        assert!(policy.is_domain_allowed("evil.com"));
    }

    #[test]
    fn test_domain_allowed_when_in_list() {
        let policy = SecurityPolicy {
            allowed_domains: vec!["example.com".into(), "api.example.com".into()],
            ..SecurityPolicy::default()
        };
        assert!(policy.is_domain_allowed("example.com"));
        assert!(policy.is_domain_allowed("api.example.com"));
    }

    #[test]
    fn test_domain_denied_when_not_in_list() {
        let policy = SecurityPolicy {
            allowed_domains: vec!["example.com".into()],
            ..SecurityPolicy::default()
        };
        assert!(!policy.is_domain_allowed("evil.com"));
    }

    #[test]
    fn test_domain_exact_match_required_not_substring() {
        // "example.com" 不应匹配 "notexample.com"（子域边界由 `.` 前缀保证）
        let policy = SecurityPolicy {
            allowed_domains: vec!["example.com".into()],
            ..SecurityPolicy::default()
        };
        assert!(!policy.is_domain_allowed("notexample.com"));
    }

    #[test]
    fn test_domain_allows_subdomains() {
        // 前缀语义（Claude Code WebFetch(domain:) 对齐）：父域覆盖子域
        let policy = SecurityPolicy {
            allowed_domains: vec!["example.com".into()],
            ..SecurityPolicy::default()
        };
        assert!(policy.is_domain_allowed("api.example.com"));
        assert!(policy.is_domain_allowed("a.b.example.com"));
        assert!(!policy.is_domain_allowed("example.com.evil.net"));
        assert!(!policy.is_domain_allowed("evil-example.com"));
    }

    #[test]
    fn test_domain_subdomain_entry_covers_deeper_subdomains() {
        // 前缀语义是单调的：条目 `api.example.com` 覆盖 `v1.api.example.com`
        //（Claude Code 同款：域名前缀匹配不区分"根域/子域"条目）
        let policy = SecurityPolicy {
            allowed_domains: vec!["api.example.com".into()],
            ..SecurityPolicy::default()
        };
        assert!(policy.is_domain_allowed("api.example.com"));
        assert!(policy.is_domain_allowed("v1.api.example.com"));
        assert!(!policy.is_domain_allowed("example.com"));
        assert!(!policy.is_domain_allowed("evil-api.example.com"));
    }

    // ── is_configured ─────────────────────────────────────────────

    #[test]
    fn test_is_configured_reflects_any_constraint() {
        assert!(!SecurityPolicy::new().is_configured());
        let mut p = SecurityPolicy::new();
        p.allowed_domains.push("example.com".into());
        assert!(p.is_configured());
        let mut p = SecurityPolicy::new();
        p.denied_paths.push("/secret".into());
        assert!(p.is_configured());
        let mut p = SecurityPolicy::new();
        p.allowed_commands.push("git".into());
        assert!(p.is_configured());
    }
}

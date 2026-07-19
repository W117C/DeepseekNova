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

    pub fn is_domain_allowed(&self, domain: &str) -> bool {
        if self.allowed_domains.is_empty() {
            return true;
        }
        self.allowed_domains.iter().any(|d| d == domain)
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
        // "example.com" should not match "notexample.com"
        let policy = SecurityPolicy {
            allowed_domains: vec!["example.com".into()],
            ..SecurityPolicy::default()
        };
        assert!(!policy.is_domain_allowed("notexample.com"));
    }
}

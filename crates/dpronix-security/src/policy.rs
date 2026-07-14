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

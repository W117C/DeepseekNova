use crate::Sandbox;

/// macOS sandbox using `sandbox-exec` with a seatbelt profile.
///
/// Wraps command execution inside `sandbox-exec -f <profile>` to restrict
/// filesystem access, network access, process spawning, and syscalls.
///
/// When the `sandbox-exec` binary is not found at runtime,
/// [`Sandbox::backend_available`] 返回 false，上层 ShellTool 会 fail-closed
/// 拒绝执行（不再静默降级为无沙箱）。
#[derive(Debug, Clone)]
pub struct SeatbeltSandbox {
    /// The seatbelt profile content. Use `-p` flag to pass inline.
    profile: String,
}

impl Default for SeatbeltSandbox {
    fn default() -> Self {
        Self {
            profile: default_profile(),
        }
    }
}

impl SeatbeltSandbox {
    /// Create a new `SeatbeltSandbox` with a custom profile string (inline).
    pub fn new(profile: impl Into<String>) -> Self {
        Self {
            profile: profile.into(),
        }
    }

    /// Build a sandbox whose profile extends the default with config-driven
    /// allowances. Appended rules take precedence (SBPL evaluates last match
    /// wins), so an empty policy is byte-identical to the default profile.
    pub fn with_policy(writable_paths: &[String], allow_network: bool) -> Self {
        let mut profile = default_profile();
        if !writable_paths.is_empty() || allow_network {
            profile.push_str("\n;; --- policy appended from config (last match wins) ---\n");
            for p in writable_paths {
                profile.push_str(&format!("(allow file-write* (subpath \"{p}\"))\n"));
            }
            if allow_network {
                profile.push_str("(allow network*)\n");
            }
        }
        Self { profile }
    }

    /// 按档位构建 seatbelt profile（三档模型）。
    ///
    /// - [`crate::SandboxTier::ReadOnly`]：默认 profile 原样（禁网 + 仅系统临时区可写），
    ///   忽略 `writable_paths` 与 `allow_network`——只读档不允许任何用户可写根
    /// - [`crate::SandboxTier::WorkspaceWrite`]：默认 + 可写根 + 按 `allow_network` 开网
    /// - [`crate::SandboxTier::FullAccess`]：全文件写 + 全网络（`allow_network` 无效）
    pub fn with_tier(
        tier: crate::SandboxTier,
        writable_paths: &[String],
        allow_network: bool,
    ) -> Self {
        let mut profile = default_profile();
        match tier {
            crate::SandboxTier::ReadOnly => {}
            crate::SandboxTier::WorkspaceWrite => {
                if !writable_paths.is_empty() || allow_network {
                    profile.push_str("\n;; --- tier: workspace-write (last match wins) ---\n");
                    for p in writable_paths {
                        profile.push_str(&format!("(allow file-write* (subpath \"{p}\"))\n"));
                    }
                    if allow_network {
                        profile.push_str("(allow network*)\n");
                    }
                }
            }
            crate::SandboxTier::FullAccess => {
                profile.push_str("\n;; --- tier: full-access (last match wins) ---\n");
                profile.push_str("(allow file-write*)\n");
                profile.push_str("(allow network*)\n");
            }
        }
        Self { profile }
    }

    /// Create a new `SeatbeltSandbox` from a profile file path.
    pub fn from_file(path: impl AsRef<std::path::Path>) -> std::io::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Ok(Self { profile: content })
    }

    /// Returns the content of the built-in default seatbelt profile.
    ///
    /// The default profile:
    /// - Allows reading the entire filesystem and writing to /tmp and /dev/null.
    /// - Allows the executed process to fork and exec.
    /// - Blocks all network access.
    /// - Blocks all mach-lookup and IOKit access.
    /// - Blocks sysctl writes.
    pub fn default_profile() -> &'static str {
        // Language: Apple Sandbox Scheme
        r#"(version 1)
;; deny everything by default
(deny default)

;; filesystem: allow reading everywhere
(allow file-read*)

;; filesystem: allow writing only to temp locations
(allow file-write*
    (subpath "/tmp")
    (subpath "/private/tmp")
    (literal "/dev/null")
    (literal "/dev/zero")
    (regex #"^/private/var/folders/[^/]+/[^/]+/T/")
)

;; allow reading/writing to the process's own temp dirs
(allow file-write*
    (subpath (param "DARWIN_USER_CACHE_DIR"))
    (subpath (param "DARWIN_USER_TEMP_DIR"))
)

;; process execution
(allow process-exec)
(allow process-fork)

;; signals
(allow signal)

;; sysctl (read-only)
(allow sysctl-read)

;; basic unix sockets for logging, etc.
(allow file-write-unlink)
(allow file-ioctl)

;; time info
(allow mach-lookup
    (global-name "com.apple.system.notification_center")
)

;; deny everything we haven't explicitly allowed above
(deny file-write* (with no-log))
(deny file-write-data (with no-log))
(deny file-write-create (with no-log))
(deny file-write-mode (with no-log))
(deny file-write-owner (with no-log))
(deny file-write-flags (with no-log))
(deny file-write-xattr (with no-log))
(deny network* (with no-log))
(deny mach-lookup* (with no-log))
(deny mach-register (with no-log))
(deny sysctl-write (with no-log))
(deny socket-ioctl (with no-log))
(deny process-info (with no-log))
(deny iokit-open (with no-log))
(deny system-fsctl (with no-log))
"#
    }
}

impl Sandbox for SeatbeltSandbox {
    fn sandbox(&self, cmd_executable: &str, cmd_args: &[String]) -> (String, Vec<String>) {
        let mut args = vec![
            "-p".to_string(),
            self.profile.clone(),
            cmd_executable.to_string(),
        ];
        args.extend_from_slice(cmd_args);

        ("sandbox-exec".to_string(), args)
    }

    fn name(&self) -> &str {
        "macos-seatbelt"
    }

    fn backend_available(&self) -> bool {
        sandbox_exec_available()
    }
}

/// Check whether `sandbox-exec` is available on the system (cached per process
/// so the probe subprocess is spawned at most once).
fn sandbox_exec_available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        std::process::Command::new("sandbox-exec")
            .arg("-n")
            .arg("true")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// Returns the default profile string (same as `SeatbeltSandbox::default_profile()`).
fn default_profile() -> String {
    String::from(SeatbeltSandbox::default_profile())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seatbelt_has_name() {
        let sb = SeatbeltSandbox::default();
        assert_eq!(sb.name(), "macos-seatbelt");
    }

    #[test]
    fn seatbelt_is_active() {
        let sb = SeatbeltSandbox::default();
        assert!(sb.is_active());
    }

    #[test]
    fn custom_profile() {
        let sb = SeatbeltSandbox::new("(version 1)\n(allow default)");
        assert_eq!(sb.profile, "(version 1)\n(allow default)");
    }

    #[test]
    fn default_profile_is_non_empty() {
        let profile = SeatbeltSandbox::default_profile();
        assert!(!profile.is_empty());
        assert!(profile.contains("(deny default)"));
        assert!(profile.contains("(allow file-read*)"));
    }

    // --- with_policy ---

    #[test]
    fn with_policy_appends_writable_subpath_rules() {
        let sb = SeatbeltSandbox::with_policy(&["/tmp/work".into(), "/var/cache".into()], false);
        assert!(sb
            .profile
            .contains("(allow file-write* (subpath \"/tmp/work\"))"));
        assert!(sb
            .profile
            .contains("(allow file-write* (subpath \"/var/cache\"))"));
        // 追加规则位于默认 profile 之后（SBPL last match wins）
        assert!(sb.profile.starts_with(&default_profile()));
    }

    #[test]
    fn with_policy_appends_network_allow() {
        let sb = SeatbeltSandbox::with_policy(&[], true);
        assert!(sb.profile.contains("(allow network*)"));
        // 默认的 (deny default) 仍保留在前缀中
        assert!(sb.profile.contains("(deny default)"));
    }

    #[test]
    fn with_policy_network_disallowed_adds_no_network_rule() {
        // 负例：未开网络时不得出现 (allow network*)
        let sb = SeatbeltSandbox::with_policy(&["/tmp/work".into()], false);
        assert!(!sb.profile.contains("(allow network*)"));
    }

    #[test]
    fn with_policy_empty_is_byte_identical_to_default() {
        // 负例：空策略不得改变默认 profile（也不带追加标记）
        let sb = SeatbeltSandbox::with_policy(&[], false);
        assert_eq!(sb.profile, default_profile());
        assert!(!sb.profile.contains("policy appended from config"));
    }

    // --- with_tier ---

    #[test]
    fn readonly_tier_ignores_writable_paths_and_network() {
        // 只读档：即使给了可写根与开网，profile 也必须与默认完全一致
        let sb =
            SeatbeltSandbox::with_tier(crate::SandboxTier::ReadOnly, &["/tmp/work".into()], true);
        assert_eq!(sb.profile, default_profile());
        assert!(!sb.profile.contains("(allow network*)"));
    }

    #[test]
    fn workspace_write_tier_allows_writable_paths() {
        let sb = SeatbeltSandbox::with_tier(
            crate::SandboxTier::WorkspaceWrite,
            &["/tmp/work".into()],
            false,
        );
        assert!(sb
            .profile
            .contains("(allow file-write* (subpath \"/tmp/work\"))"));
        // 未开网时禁网规则仍在
        assert!(!sb.profile.contains("(allow network*)"));
    }

    #[test]
    fn full_access_tier_opens_all() {
        let sb = SeatbeltSandbox::with_tier(
            crate::SandboxTier::FullAccess,
            &[],
            false, // allow_network 在 FullAccess 档无效
        );
        assert!(sb.profile.contains("(allow file-write*)"));
        assert!(sb.profile.contains("(allow network*)"));
    }
}

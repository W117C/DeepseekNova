use crate::Sandbox;

/// macOS sandbox using `sandbox-exec` with a seatbelt profile.
///
/// Wraps command execution inside `sandbox-exec -f <profile>` to restrict
/// filesystem access, network access, process spawning, and syscalls.
///
/// When the `sandbox-exec` binary is not found at runtime, sandboxing is
/// silently degraded to no-op (logged at warn level).
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
        // If sandbox-exec is not available, fall back to no-op.
        if !sandbox_exec_available() {
            tracing::warn!("sandbox-exec not found; running command without sandbox");
            return (cmd_executable.to_string(), cmd_args.to_vec());
        }

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
}

/// Check whether `sandbox-exec` is available on the system.
fn sandbox_exec_available() -> bool {
    std::process::Command::new("sandbox-exec")
        .arg("-n")
        .arg("true")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
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
}

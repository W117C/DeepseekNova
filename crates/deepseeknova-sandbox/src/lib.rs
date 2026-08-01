//! # Sandbox — OS-level execution sandboxing
//!
//! Restricts subprocess execution via platform-specific sandboxes:
//! macOS Seatbelt (`sandbox-exec`) and Linux bubblewrap (`bwrap`).

#[cfg(target_os = "linux")]
pub mod bubblewrap;
#[cfg(target_os = "macos")]
pub mod seatbelt;

/// Trait for sandboxing shell command execution.
///
/// Implementations wrap a shell command invocation inside a platform-specific
/// sandbox to restrict filesystem access, network access, process spawning,
/// and other capabilities.
pub trait Sandbox: Send + Sync {
    /// Given a command executable and its arguments, return a potentially
    /// sandboxed `(executable, args)` pair. The returned executable replaces
    /// the original; the returned args are prepended before the original
    /// command arguments.
    ///
    /// NoOpSandbox returns the input unchanged.
    fn sandbox(&self, cmd_executable: &str, cmd_args: &[String]) -> (String, Vec<String>);

    /// Human-readable name for logging and diagnostics.
    fn name(&self) -> &str;

    /// Whether this sandbox is active (capable of enforcing restrictions).
    /// Returns `false` for NoOpSandbox.
    fn is_active(&self) -> bool {
        true
    }
}

/// A sandbox that performs no isolation — commands run directly.
///
/// This is the default sandbox. It returns the command unchanged.
#[derive(Debug, Clone, Default)]
pub struct NoOpSandbox;

impl Sandbox for NoOpSandbox {
    fn sandbox(&self, cmd_executable: &str, cmd_args: &[String]) -> (String, Vec<String>) {
        (cmd_executable.to_string(), cmd_args.to_vec())
    }

    fn name(&self) -> &str {
        "noop"
    }

    fn is_active(&self) -> bool {
        false
    }
}

/// Returns the appropriate sandbox for the current platform.
///
/// - macOS: `SeatbeltSandbox` (uses `sandbox-exec`)
/// - Linux: `BubblewrapSandbox` (uses `bwrap`)
/// - Other: `NoOpSandbox`
pub fn platform_sandbox() -> Box<dyn Sandbox> {
    #[cfg(target_os = "macos")]
    {
        Box::new(seatbelt::SeatbeltSandbox::default())
    }
    #[cfg(target_os = "linux")]
    {
        Box::new(bubblewrap::BubblewrapSandbox::default())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Box::new(NoOpSandbox)
    }
}

/// Like [`platform_sandbox`] but applies a config-driven policy (writable
/// paths / bind mounts and an optional network share). An empty policy is
/// equivalent to [`platform_sandbox`].
pub fn platform_sandbox_with(writable_paths: &[String], allow_network: bool) -> Box<dyn Sandbox> {
    #[cfg(target_os = "macos")]
    {
        Box::new(seatbelt::SeatbeltSandbox::with_policy(
            writable_paths,
            allow_network,
        ))
    }
    #[cfg(target_os = "linux")]
    {
        Box::new(bubblewrap::BubblewrapSandbox::with_policy(
            writable_paths,
            allow_network,
        ))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (writable_paths, allow_network);
        Box::new(NoOpSandbox)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_sandbox_passes_through() {
        let s = NoOpSandbox;
        let (exe, args) = s.sandbox("sh", &["-c".into(), "echo hi".into()]);
        assert_eq!(exe, "sh");
        assert_eq!(args, vec!["-c", "echo hi"]);
    }

    #[test]
    fn noop_sandbox_is_not_active() {
        assert!(!NoOpSandbox.is_active());
    }

    #[test]
    fn noop_sandbox_name() {
        assert_eq!(NoOpSandbox.name(), "noop");
    }

    #[test]
    fn platform_sandbox_with_empty_policy_matches_platform_sandbox() {
        // 空策略下两个入口必须选择同一实现
        let plain = platform_sandbox();
        let with_policy = platform_sandbox_with(&[], false);
        assert_eq!(plain.name(), with_policy.name());
    }

    #[test]
    fn platform_sandbox_with_policy_keeps_platform_impl() {
        // 带策略时仍返回平台实现，不会退化为其他沙箱
        let sb = platform_sandbox_with(&["/tmp/work".to_string()], true);
        assert_eq!(sb.name(), platform_sandbox().name());
    }
}

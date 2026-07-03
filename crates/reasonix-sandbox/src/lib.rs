pub mod bubblewrap;
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
}

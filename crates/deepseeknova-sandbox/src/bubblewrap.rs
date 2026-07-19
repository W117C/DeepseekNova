use crate::Sandbox;

/// Linux sandbox using `bwrap` (bubblewrap).
///
/// Wraps command execution inside a bubblewrap container to restrict
/// filesystem access, network access, and process visibility.
///
/// When the `bwrap` binary is not found at runtime, sandboxing is silently
/// degraded to no-op (logged at warn level).
#[derive(Debug, Clone)]
pub struct BubblewrapSandbox {
    /// Additional directories to bind-mount read-only inside the sandbox.
    /// Defaults to `/usr`, `/lib`, `/lib64`, `/bin`, `/etc`.
    extra_readonly_binds: Vec<String>,
    /// Additional directories to bind-mount read-write inside the sandbox.
    readwrite_binds: Vec<String>,
}

impl Default for BubblewrapSandbox {
    fn default() -> Self {
        Self {
            extra_readonly_binds: default_ro_binds(),
            readwrite_binds: Vec::new(),
        }
    }
}

impl BubblewrapSandbox {
    /// Create a new `BubblewrapSandbox` with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an additional read-only bind mount (host_path=sandbox_path).
    pub fn with_readonly_bind(mut self, host_path: impl Into<String>) -> Self {
        self.extra_readonly_binds.push(host_path.into());
        self
    }

    /// Add an additional read-write bind mount (host_path=sandbox_path).
    pub fn with_readwrite_bind(mut self, host_path: impl Into<String>) -> Self {
        self.readwrite_binds.push(host_path.into());
        self
    }

    /// Build the bwrap arguments vector.
    fn build_args(&self, cmd_executable: &str, cmd_args: &[String]) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "--unshare-all".to_string(),
            "--dev".to_string(),
            "/dev".to_string(),
            "--proc".to_string(),
            "/proc".to_string(),
            "--tmpfs".to_string(),
            "/tmp".to_string(),
        ];

        // Bind common system directories read-only.
        for bind in &self.extra_readonly_binds {
            args.push("--ro-bind".to_string());
            args.push(bind.clone());
            args.push(bind.clone());
        }

        // Bind writable directories.
        for bind in &self.readwrite_binds {
            args.push("--bind".to_string());
            args.push(bind.clone());
            args.push(bind.clone());
        }

        // Disable network access.
        args.push("--unshare-net".to_string());

        // Disable IPC.
        args.push("--unshare-ipc".to_string());

        // Disable UTS (hostname) namespace.
        args.push("--unshare-uts".to_string());

        // Disable cgroup access.
        args.push("--unshare-cgroup".to_string());

        // New session (no controlling terminal).
        args.push("--new-session".to_string());

        // Clear environment.
        args.push("--clearenv".to_string());

        // Pass through essential environment variables.
        args.push("--setenv".to_string());
        args.push("PATH".to_string());
        args.push("/usr/bin:/bin:/usr/sbin:/sbin".to_string());

        args.push("--setenv".to_string());
        args.push("HOME".to_string());
        args.push("/tmp".to_string());

        args.push("--setenv".to_string());
        args.push("USER".to_string());
        args.push("sandbox".to_string());

        // Die with the parent if bwrap itself dies.
        args.push("--die-with-parent".to_string());

        // Separator between bwrap args and the command to run.
        args.push("--".to_string());

        // The actual command to execute.
        args.push(cmd_executable.to_string());
        args.extend_from_slice(cmd_args);

        args
    }
}

impl Sandbox for BubblewrapSandbox {
    fn sandbox(&self, cmd_executable: &str, cmd_args: &[String]) -> (String, Vec<String>) {
        // If bwrap is not available, fall back to no-op.
        if !bwrap_available() {
            tracing::warn!("bwrap not found; running command without sandbox");
            return (cmd_executable.to_string(), cmd_args.to_vec());
        }

        let args = self.build_args(cmd_executable, cmd_args);
        ("bwrap".to_string(), args)
    }

    fn name(&self) -> &str {
        "linux-bubblewrap"
    }
}

/// Returns the default set of read-only bind mount paths.
fn default_ro_binds() -> Vec<String> {
    vec![
        "/usr".to_string(),
        "/lib".to_string(),
        "/lib64".to_string(),
        "/bin".to_string(),
        "/sbin".to_string(),
        "/etc".to_string(),
        "/opt".to_string(),
    ]
}

/// Check whether `bwrap` is available on the system.
fn bwrap_available() -> bool {
    std::process::Command::new("bwrap")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bubblewrap_has_name() {
        let bw = BubblewrapSandbox::default();
        assert_eq!(bw.name(), "linux-bubblewrap");
    }

    #[test]
    fn bubblewrap_is_active() {
        let bw = BubblewrapSandbox::default();
        assert!(bw.is_active());
    }

    #[test]
    fn builder_adds_readonly_bind() {
        let bw = BubblewrapSandbox::new().with_readonly_bind("/custom");
        assert!(bw.extra_readonly_binds.contains(&"/custom".to_string()));
    }

    #[test]
    fn builder_adds_readwrite_bind() {
        let bw = BubblewrapSandbox::new().with_readwrite_bind("/tmp/work");
        assert!(bw.readwrite_binds.contains(&"/tmp/work".to_string()));
    }

    #[test]
    fn build_args_includes_separator_and_command() {
        let bw = BubblewrapSandbox::default();
        let args = bw.build_args("sh", &["-c".into(), "echo hi".into()]);

        // Find the "--" separator
        let sep_pos = args.iter().position(|a| a == "--");
        assert!(sep_pos.is_some(), "args should contain '--' separator");
        let sep_pos = sep_pos.unwrap();

        // After separator: the command executable and its args
        assert_eq!(args[sep_pos + 1], "sh");
        assert_eq!(args[sep_pos + 2], "-c");
        assert_eq!(args[sep_pos + 3], "echo hi");
    }

    #[test]
    fn build_args_restricts_network() {
        let bw = BubblewrapSandbox::default();
        let args = bw.build_args("sh", &["-c".into(), "echo hi".into()]);
        assert!(args.contains(&"--unshare-net".to_string()));
    }

    #[test]
    fn build_args_clears_environment() {
        let bw = BubblewrapSandbox::default();
        let args = bw.build_args("sh", &["-c".into(), "echo hi".into()]);
        assert!(args.contains(&"--clearenv".to_string()));
    }

    #[test]
    fn build_args_sets_path() {
        let bw = BubblewrapSandbox::default();
        let args = bw.build_args("sh", &["-c".into(), "echo hi".into()]);
        let path_pos = args.iter().position(|a| a == "PATH");
        assert!(path_pos.is_some());
        // The value follows immediately after PATH
        assert_eq!(args[path_pos.unwrap() + 1], "/usr/bin:/bin:/usr/sbin:/sbin");
    }
}

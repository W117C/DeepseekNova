//! # Sandbox — OS-level execution sandboxing
//!
//! Restricts subprocess execution via platform-specific sandboxes:
//! macOS Seatbelt (`sandbox-exec`) and Linux bubblewrap (`bwrap`).

#[cfg(target_os = "linux")]
pub mod bubblewrap;
#[cfg(target_os = "macos")]
pub mod seatbelt;

/// 沙箱档位：把 agent 运行权限抽象为少量分级档位（而非自由裁量）。
///
/// 设计对照 Codex 的三档模型（read-only / workspace-write / full-access）：
/// 每档对应一套可渲染、可校验的沙箱策略；档位越靠前，网络与写入
/// 限制越严。平台实现（seatbelt/bubblewrap）按档位渲染策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxTier {
    /// 只读：不可写文件系统（除系统临时区）、不可网络（强制禁网）。
    ReadOnly,
    /// 工作区写：可写根（writable paths）+ 网络按配置（默认禁网）。
    WorkspaceWrite,
    /// 全权限：可写任意路径 + 网络全开（仅在高风险授权场景使用）。
    FullAccess,
}

impl SandboxTier {
    /// 是否为只读档（无任何用户可写根）。
    pub fn is_read_only(&self) -> bool {
        matches!(self, SandboxTier::ReadOnly)
    }

    /// 该档位是否允许网络（受配置约束时）。
    /// `FullAccess` 全开；其余档位默认禁网，网络是否放行由上层配置决定。
    pub fn allows_network_by_default(&self) -> bool {
        matches!(self, SandboxTier::FullAccess)
    }
}

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

    /// 该沙箱是否属于"必须隔离"型（平台沙箱为 true，NoOp 为 false）。
    /// 上层工具据此 fail-closed：必须隔离但后端不可用时拒绝执行。
    fn requires_isolation(&self) -> bool {
        true
    }

    /// 后端可执行文件是否可用（macOS sandbox-exec / Linux bwrap）。
    fn backend_available(&self) -> bool {
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

    fn requires_isolation(&self) -> bool {
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

/// 按档位构造平台沙箱（Codex 式三档模型）。
///
/// - [`SandboxTier::ReadOnly`]：强制禁网、无用户可写根（只读档）
/// - [`SandboxTier::WorkspaceWrite`]：可写根 + 网络按 `allow_network`（默认禁网）
/// - [`SandboxTier::FullAccess`]：网络全开（`allow_network` 无效），
///   `writable_paths` 为空时仍由平台默认写策略约束
///
/// 空档位配置退化为 [`platform_sandbox`]。
pub fn platform_sandbox_tiered(
    tier: SandboxTier,
    writable_paths: &[String],
    allow_network: bool,
) -> Box<dyn Sandbox> {
    match tier {
        SandboxTier::ReadOnly => {
            #[cfg(target_os = "macos")]
            {
                Box::new(seatbelt::SeatbeltSandbox::with_tier(tier, &[], false))
            }
            #[cfg(target_os = "linux")]
            {
                Box::new(bubblewrap::BubblewrapSandbox::with_tier(tier, &[], false))
            }
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            {
                let _ = (tier, writable_paths, allow_network);
                Box::new(NoOpSandbox)
            }
        }
        SandboxTier::WorkspaceWrite | SandboxTier::FullAccess => {
            let net = tier.allows_network_by_default() || allow_network;
            #[cfg(target_os = "macos")]
            {
                Box::new(seatbelt::SeatbeltSandbox::with_tier(
                    tier,
                    writable_paths,
                    net,
                ))
            }
            #[cfg(target_os = "linux")]
            {
                Box::new(bubblewrap::BubblewrapSandbox::with_tier(
                    tier,
                    writable_paths,
                    net,
                ))
            }
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            {
                let _ = (tier, writable_paths, allow_network);
                Box::new(NoOpSandbox)
            }
        }
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

    #[test]
    fn tier_semantics() {
        assert!(SandboxTier::ReadOnly.is_read_only());
        assert!(!SandboxTier::WorkspaceWrite.is_read_only());
        assert!(!SandboxTier::FullAccess.is_read_only());
        // 网络默认：FullAccess 全开，其余档位默认禁网
        assert!(SandboxTier::FullAccess.allows_network_by_default());
        assert!(!SandboxTier::ReadOnly.allows_network_by_default());
        assert!(!SandboxTier::WorkspaceWrite.allows_network_by_default());
    }

    #[test]
    fn tiered_sandbox_keeps_platform_impl() {
        for tier in [
            SandboxTier::ReadOnly,
            SandboxTier::WorkspaceWrite,
            SandboxTier::FullAccess,
        ] {
            let sb = platform_sandbox_tiered(tier, &["/tmp/work".to_string()], false);
            assert_eq!(sb.name(), platform_sandbox().name());
        }
    }
}

//! # Sandbox — OS-level execution sandboxing
//!
//! Restricts subprocess execution via platform-specific sandboxes:
//! macOS Seatbelt (`sandbox-exec`) and Linux bubblewrap (`bwrap`).
//!
//! ## 平台现状
//!
//! | 平台 | 后端 | 说明 |
//! |------|------|------|
//! | macOS | Seatbelt（`sandbox-exec`） | 已实现，见 `seatbelt` 模块 |
//! | Linux | bubblewrap（`bwrap`） | 已实现，见 `bubblewrap` 模块 |
//! | Windows | 无（[`NoOpSandbox`]） | 见下文「Windows 沙箱现状与方案」 |
//! | 其他 | [`NoOpSandbox`] | 无隔离 |
//!
//! ## Windows 沙箱现状与方案
//!
//! **现状**：Windows 上 [`platform_sandbox`]、[`platform_sandbox_with`]、
//! [`platform_sandbox_tiered`] 的全部分支都返回 [`NoOpSandbox`]（无隔离）。
//! 不要误以为这些分支提供了 Windows 隔离。
//!
//! **方案（由简到严）**：
//! - **Job Object**（最小可行，推荐起点）：创建受限 Job 对象，把子进程
//!   assign 进 Job（`AssignProcessToJobObject`），用 Job 限制
//!   （`JOB_OBJECT_LIMIT_*`：进程数上限、CPU/内存配额、作业结束即杀进程树）
//!   提供进程树隔离 + 资源限制。注意 Job Object **不直接限制网络**——整网
//!   开关需配合 WFP（Windows Filtering Platform）过滤器或 AppContainer。
//! - **AppContainer**（更严格）：基于派生 SID 的低特权令牌 + 能力
//!   （capability）声明，需要令牌/清单/SID 派生，复杂度显著更高，可做
//!   只读文件系统与网络白名单，但开发与调试成本大。
//!
//! **实现前提（诚实约束）**：以上方案必须在 **Windows 环境**实现并验证——
//! Job Object / 令牌 API 是 Windows 专有系统调用，交叉编译只能验证编译期
//! 形态，无法验证运行时隔离行为。沙箱是安全边界，本 crate 在非 Windows
//! 平台不引入 `windows-sys`，也不编写无法验证的 `cfg(windows)` 后端代码。
//! Windows 后端的实际实现留待 Windows 环境落地。
//!
//! ## 网络策略与域名白名单
//!
//! - [`NetworkPolicy`] 提供类型化网络策略：整网开关 + 可选域名白名单。
//!   平台消费方式：**seatbelt/bwrap 当前仅支持整网开关**（seatbelt 追加
//!   `(allow network*)`，bwrap `--share-net`/`--unshare-net`）。域名级
//!   过滤需 DNS 解析后按 IP 过滤——seatbelt 无域名匹配原语，`(remote tcp)`
//!   只匹配 IP/端口——属后续实现。本 crate 当前只提供配置接口 + 类型 +
//!   校验，不实现未经验证的域名级过滤逻辑。
//!
//! ## 默认档位
//!
//! 沙箱默认关闭（`SandboxConfig.enabled = false`；启动时由 CLI 打印横幅
//! 提示，`--secure-defaults` 一键加固）。显式启用时 [`crate::SandboxTier::WorkspaceWrite`]
//! 是推荐档位：工作区可写 + 网络按配置（默认禁网），兼顾隔离与日常使用。
//! 本 crate 不修改任何默认值。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

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

/// 类型化网络策略：整网开关 + 可选域名白名单。
///
/// 平台消费方式（重要，勿过度承诺）：
/// - **seatbelt / bubblewrap 当前仅支持整网开关**——seatbelt 追加
///   `(allow network*)` 规则放行全部网络，bubblewrap 通过
///   `--share-net`（共享宿主网络）放行、`--unshare-net`（隔离网络）禁网；
/// - **`allow_domains`（域名白名单）当前不改变任何平台 profile/参数**，
///   属后续实现：需在进程内把域名解析为 IP 集后再按 IP 过滤（seatbelt
///   无域名匹配原语，`(remote tcp)` 只匹配 IP/端口）；
/// - 空 `allow_domains` 表示维持 `allow_network` bool 的整网开关语义。
///
/// 默认语义：`allow_network = false`（禁网）、`allow_domains = []`。
/// 本任务只提供类型 + 校验，不实现域名级过滤逻辑。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NetworkPolicy {
    /// 整网开关：`true` = 沙箱内网络放行（在平台 profile/参数层生效）。
    pub allow_network: bool,
    /// 域名白名单（可选）：当前仅记录/校验，不参与平台 profile 渲染。
    pub allow_domains: Vec<String>,
}

impl NetworkPolicy {
    /// 构造网络策略。
    pub fn new(allow_network: bool, allow_domains: Vec<String>) -> Self {
        Self {
            allow_network,
            allow_domains,
        }
    }

    /// 是否请求了域名级过滤（白名单非空，但平台后端当前不支持）。
    ///
    /// 供上层识别"用户期望域名过滤但当前后端只能整网开关"的差距，
    /// 便于日志提示，不改变任何平台行为。
    pub fn requests_domain_filtering(&self) -> bool {
        !self.allow_domains.is_empty()
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
/// - Other: `NoOpSandbox` — **Windows 无隔离**，沙箱现状与方案见
///   [crate 根文档](crate#windows-沙箱现状与方案)。
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
        // Windows/其他平台：无隔离。Windows 沙箱现状与方案见 crate 根文档
        // 「Windows 沙箱现状与方案」。
        Box::new(NoOpSandbox)
    }
}

/// Like [`platform_sandbox`] but applies a config-driven policy (writable
/// paths / bind mounts and an optional network share). An empty policy is
/// equivalent to [`platform_sandbox`].
///
/// 注意：网络为整网开关（seatbelt `(allow network*)` / bwrap
/// `--share-net`），**不支持域名级白名单**（见 [`NetworkPolicy`]）。
/// Windows 分支返回 [`NoOpSandbox`]，方案见
/// [crate 根文档](crate#windows-沙箱现状与方案)。
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
        // Windows/其他平台：无隔离。方案见 crate 根文档「Windows 沙箱现状与方案」。
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
///
/// 网络为整网开关，**不支持域名级白名单**（见 [`NetworkPolicy`]）。Windows
/// 全部分支返回 [`NoOpSandbox`]，现状与方案见
/// [crate 根文档](crate#windows-沙箱现状与方案)。
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
                // Windows/其他平台：无隔离。方案见 crate 根文档「Windows 沙箱现状与方案」。
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
                // Windows/其他平台：无隔离。方案见 crate 根文档「Windows 沙箱现状与方案」。
                let _ = (tier, writable_paths, allow_network, net);
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
    fn network_policy_default_denies_network() {
        // 类型化默认语义：禁网 + 无域名白名单（fail-closed 方向）。
        let p = NetworkPolicy::default();
        assert!(!p.allow_network);
        assert!(p.allow_domains.is_empty());
        assert!(!p.requests_domain_filtering());
    }

    #[test]
    fn network_policy_new_sets_fields() {
        let p = NetworkPolicy::new(
            true,
            vec!["api.github.com".to_string(), "example.com".to_string()],
        );
        assert!(p.allow_network);
        assert_eq!(p.allow_domains.len(), 2);
        assert!(p.requests_domain_filtering());
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

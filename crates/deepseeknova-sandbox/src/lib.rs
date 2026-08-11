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
//! | Windows | Job Object（`windows::JobSandbox`） | 已实现：进程树隔离 + 资源限制 |
//! | 其他 | [`NoOpSandbox`] | 无隔离 |
//!
//! ## Windows 沙箱现状与方案
//!
//! **现状（2026-08-10 已实现）**：Windows 上三个 `platform_sandbox*` 入口都
//! 返回 `windows::JobSandbox`：以 `CREATE_SUSPENDED` 创建子进程 → assign
//! 到 Job Object → 恢复主线程。Job 设置 `KILL_ON_JOB_CLOSE`（句柄释放即杀
//! 进程树）与活动进程数上限；Job 句柄由独立线程在进程退出后释放，保证
//! kill-on-close 不误杀仍在运行的子进程树。运行时行为由 CI 的
//! `windows-latest` 测试矩阵验证（本 crate 测试含真实 spawn 用例）。
//!
//! **方案（由简到严）**：
//! - **Job Object**（已实现，最小可行）：进程树隔离 + 资源限制
//!   （活动进程上限、kill-on-close）。注意 Job Object **不直接限制网络与
//!   文件系统写路径**——整网开关需配合 WFP（Windows Filtering Platform）
//!   过滤器或 AppContainer（后续项）。
//! - **AppContainer**（更严格）：基于派生 SID 的低特权令牌 + 能力
//!   （capability）声明，需要令牌/清单/SID 派生，复杂度显著更高，可做
//!   只读文件系统与网络白名单，但开发与调试成本大。
//!
//! **诚实约束**：Job Object 后端在非 Windows 平台只做交叉编译检查，运行时
//! 行为由 CI 的 windows-latest 矩阵执行测试验证；AppContainer 仍需 Windows
//! 环境专项实现。
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

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented,
        clippy::dbg_macro
    )
)]

#[cfg(target_os = "linux")]
pub mod bubblewrap;
#[cfg(target_os = "macos")]
pub mod seatbelt;
#[cfg(windows)]
pub mod windows;

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

    /// 在沙箱约束下 spawn 一个已构造好的命令。
    ///
    /// 默认实现直接 `cmd.spawn()`；Windows Job Object 后端覆盖此方法，用
    /// `CREATE_SUSPENDED` 创建进程、assign 到 Job 后再恢复主线程，保证
    /// 进程树在 Job 约束（含 kill-on-close）内。
    fn spawn(&self, mut cmd: tokio::process::Command) -> std::io::Result<tokio::process::Child> {
        cmd.spawn()
    }

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

    /// 该后端是否具备强制网络限制的能力（能力位查询）。
    ///
    /// `true`：后端能在沙箱内落实"禁网/网络隔离"策略——seatbelt 以
    /// `(deny network*)` 默认禁网、`(allow network*)` 放行；bwrap 以
    /// `--unshare-net` 隔离、`--share-net` 放行。是否实际禁网由构造参数
    /// `allow_network` 决定，与本站点值无关。
    ///
    /// `false`：后端对网络零限制（如 Windows JobSandbox、NoOpSandbox），
    /// 用户请求 `allow_network=false` 时会被静默忽略。上层（如 runtime
    /// 装配点）据此在"用户显式禁网但后端无法强制"时发出可检测的降级告警，
    /// 作为 fail-closed 决策输入。
    fn enforced_network(&self) -> bool {
        false
    }

    /// 该后端是否具备强制文件系统写限制的能力（能力位查询）。
    ///
    /// `true`：后端能在沙箱内限制文件写入——seatbelt/bwrap 按
    /// [`SandboxTier`] 渲染只读或路径白名单写策略。是否实际只读由构造档位
    /// 决定，与本站点值无关。
    ///
    /// `false`：后端对文件写入零限制（如 Windows JobSandbox、NoOpSandbox），
    /// 用户请求 [`SandboxTier::ReadOnly`] 时会被静默忽略。上层据此在
    /// "用户请求只读档但后端无法强制"时发出可检测的降级告警。
    fn enforced_fs(&self) -> bool {
        false
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
/// - Windows: `JobSandbox`（Job Object：进程树隔离 + 资源限制），见
///   `windows::JobSandbox`
/// - Other: `NoOpSandbox` — 无隔离
pub fn platform_sandbox() -> Box<dyn Sandbox> {
    #[cfg(target_os = "macos")]
    {
        Box::new(seatbelt::SeatbeltSandbox::default())
    }
    #[cfg(target_os = "linux")]
    {
        Box::new(bubblewrap::BubblewrapSandbox::default())
    }
    #[cfg(windows)]
    {
        Box::new(windows::JobSandbox::default())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        // 非 Windows 的其它平台：无隔离。
        Box::new(NoOpSandbox)
    }
}

/// Like [`platform_sandbox`] but applies a config-driven policy (writable
/// paths / bind mounts and an optional network share). An empty policy is
/// equivalent to [`platform_sandbox`].
///
/// 注意：网络为整网开关（seatbelt `(allow network*)` / bwrap
/// `--share-net`），**不支持域名级白名单**（见 [`NetworkPolicy`]）。
/// Windows 分支返回 `windows::JobSandbox`（策略参数当前仅记录：Job Object
/// 不直接限制网络/写路径，整网与文件系统白名单仍需 WFP/AppContainer）。
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
    #[cfg(windows)]
    {
        if !allow_network {
            tracing::warn!(
                "Windows JobSandbox cannot enforce allow_network=false; \
                 sandboxed commands still have network access"
            );
        }
        let _ = (writable_paths, allow_network);
        Box::new(windows::JobSandbox::default())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
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
///
/// 网络为整网开关，**不支持域名级白名单**（见 [`NetworkPolicy`]）。Windows
/// 全部分支返回 `windows::JobSandbox`（网络/写路径限制见
/// [`platform_sandbox_with`] 说明）。
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
            #[cfg(windows)]
            {
                tracing::warn!(
                    "Windows JobSandbox cannot enforce SandboxTier::ReadOnly; \
                     network and filesystem writes are not restricted"
                );
                let _ = (tier, writable_paths, allow_network);
                Box::new(windows::JobSandbox::default())
            }
            #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
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
            #[cfg(windows)]
            {
                if !net {
                    tracing::warn!(
                        "Windows JobSandbox cannot enforce the network-off policy; \
                         sandboxed commands still have network access"
                    );
                }
                let _ = (tier, writable_paths, allow_network, net);
                Box::new(windows::JobSandbox::default())
            }
            #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
            {
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
    fn noop_sandbox_cannot_enforce_network_or_fs() {
        // NoOp 无隔离：网络与文件系统写路径零限制（能力位为 false）。
        assert!(!NoOpSandbox.enforced_network());
        assert!(!NoOpSandbox.enforced_fs());
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

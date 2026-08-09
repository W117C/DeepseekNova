//! # Deepseeknova 错误模型
//!
//! 本模块定义跨 crate 的统一错误类型 [`DeepseeknovaError`]，作为 deepseeknova
//! 框架所有公开 API 的标准错误返回类型。设计目标：
//!
//! - **小而稳定的错误表面**：用有限个变体覆盖主要错误类别，避免变体爆炸
//! - **机器可读**：每个变体是一个明确的类别，调用方可按类别 `match` 处理
//! - **可重试性标注**：[`DeepseeknovaError::is_retryable`] 给出保守的重试建议；
//!   `Provider` 变体的可重试语义由错误产生方显式标注（`retryable` 字段），
//!   不依赖消息文本匹配
//! - **可脱敏**：[`DeepseeknovaError::redacted`] 输出可安全日志化的字符串
//! - **可演进**：枚举标记 `#[non_exhaustive]`，未来可加变体不破坏下游 `match`
//!
//! ## 兼容性策略（迁移路径）
//!
//! 本类型采用**加法式引入**，分阶段迁移 anyhow → DeepseeknovaError：
//!
//! 1. **Phase 1（已完成）**：core 定义并导出 [`DeepseeknovaError`]，提供
//!    `From<std::io::Error>`、`From<serde_json::Error>` 等核心转换。
//! 2. **Phase 2（已完成）**：下游 crate（provider、permission、graph、
//!    context、agent）在各自 crate 实现
//!    `From<TheirTypedError> for DeepseeknovaError`（利用 orphan rule）。
//! 3. **Phase 3（已完成）**：迁移核心 trait 方法签名（`Tool::execute`、
//!    `Runner::run`、`Executor::*`）。原 `bail!` 宏的所有调用已替换为具体
//!    类别变体（`Tool`/`Runner`/`Cancelled`），宏本身已移除。
//! 4. **Phase 4（已完成）**：全 workspace 移除 `anyhow` 依赖与 `Other` 变体。
//!    `runtime` / `serve` / `tui` / `cli` / `agent` / `provider` / `security`
//!    等下游 crate 已完成迁移并移除 `anyhow` 依赖；core 自身亦移除 `Other`
//!    变体与 `anyhow` 依赖，未注册外部错误需在调用点显式 `.map_err(...)?`
//!    归类到具体变体。
//!
//! ## 关键不变量
//!
//! - **`?` 单步解析**：Rust 的 `?` 运算符仅查找一个 `From` 实现，不进行多步
//!   转换。对 `Result<_, reqwest::Error>` 之类未在 [`DeepseeknovaError`] 中
//!   直接注册 `From` 的错误类型，`?` 会编译失败；调用点须显式
//!   `.map_err(DeepseeknovaError::from)?` 或归类到具体变体。
//!
//! ## 调用方使用示例
//!
//! ```rust
//! use deepseeknova_core::DeepseeknovaError;
//!
//! fn read_config() -> Result<String, DeepseeknovaError> {
//!     // std::io::Error 自动通过 #[from] 转 DeepseeknovaError::Io
//!     let s = std::fs::read_to_string("config.toml")?;
//!     Ok(s)
//! }
//!
//! fn handle(err: DeepseeknovaError) {
//!     if err.is_retryable() {
//!         eprintln!("transient: {} — retrying", err.redacted());
//!     } else {
//!         eprintln!("fatal: {}", err.redacted());
//!     }
//! }
//! ```

use std::sync::OnceLock;

/// 全局框架错误类型。
///
/// 详见[模块级文档](crate::error)。
///
/// # 演进
///
/// 本枚举标记 `#[non_exhaustive]`，未来可加变体而不破坏下游 `match`。下游
/// `match` 时必须使用 `_ =>` 兜底分支，不可穷举变体。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DeepseeknovaError {
    /// 配置错误：解析失败、字段缺失、值非法。
    #[error("configuration error: {0}")]
    Config(String),

    /// IO 错误：文件系统、网络读写。
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// 序列化错误：JSON / YAML / TOML 解析或编码失败。
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    /// 权限错误：工具调用被拒、需用户审批、策略非法。
    ///
    /// 已由 `deepseeknova-permission` crate 的
    /// `From<PermissionError> for DeepseeknovaError` 映射到本变体（Phase 2
    /// 落地），`Denied` / `RequiresApproval` / `InvalidPolicy` 均为确定性
    /// 错误，[`is_retryable`](Self::is_retryable) 返回 `false`。
    #[error("permission error: {0}")]
    Permission(String),

    /// LLM 提供商错误：HTTP 状态、限流、认证、流式中断。
    ///
    /// 已由 `deepseeknova-provider` crate 的 `From<ProviderError>` 与发送路径
    /// 映射到本变体（Phase 2 落地）。`retryable` 由产生方显式标注：限流/超时/
    /// 5xx/瞬时网络故障为 `true`，认证、参数非法、解析失败为 `false`；调用方
    /// 直接读 [`is_retryable`](Self::is_retryable)，不依赖消息文本匹配。
    #[error("provider error: {message}")]
    Provider {
        /// 错误描述。
        message: String,
        /// 该错误是否属于可重试类别（由产生方标注）。
        retryable: bool,
    },

    /// 代码图错误：解析失败、存储故障、索引繁忙、实体未找到。
    ///
    /// 已由 `deepseeknova-graph` crate 的
    /// `From<GraphError> for DeepseeknovaError` 映射到本变体（Phase 2 落地）。
    #[error("graph error: {0}")]
    Graph(String),

    /// 工具执行错误：工具内部执行失败。
    #[error("tool error: {0}")]
    Tool(String),

    /// 运行器错误：Runner/Executor/Planner 执行流失败。
    #[error("runner error: {0}")]
    Runner(String),

    /// 上下文错误：历史不变量违反、压缩失败。
    ///
    /// 已由 `deepseeknova-context` crate 的 `From<CompactionError>` /
    /// `From<InvariantViolation>` / `From<BuilderOrderError>` 映射到本变体
    /// （Phase 2 落地）。
    #[error("context error: {0}")]
    Context(String),

    /// 存储错误：SQLite/FTS5 读写、schema 校验、事务失败等。
    ///
    /// Phase 4 引入：通过 `From<rusqlite::Error>` 把 SQLite 错误桥接到本变体，
    /// 供 `core/memory` 与 `deepseeknova-store` 的存储层错误统一使用；
    /// `deepseeknova-graph` 的存储错误经 `From<GraphError>` 归入 `Graph` 变体。
    /// `is_retryable` 对本变体的 `SQLITE_BUSY` / `SQLITE_LOCKED` 返回 `true`
    /// （瞬时锁竞争可重试）。
    #[error("storage error: {0}")]
    Storage(String),

    /// Agent 错误：任务规格、@提及、清单解析失败。
    ///
    /// 已由 `deepseeknova-agent` crate 的 `From<TaskSpecError>` /
    /// `From<MentionError>` / `From<ManifestError>` 映射到本变体
    /// （Phase 2 落地）。
    #[error("agent error: {0}")]
    Agent(String),

    /// 操作被取消（用户中断、cancellation token 触发）。
    ///
    /// 取消是不可恢复的确定性状态，[`is_retryable`](Self::is_retryable)
    /// 返回 `false`。
    #[error("operation cancelled")]
    Cancelled,
}

/// Phase 4：把 `rusqlite::Error` 桥接到 [`DeepseeknovaError::Storage`]，
/// 供 `core/memory` 子系统（MemoryStore）与 `deepseeknova-store` 的存储层
/// 错误统一使用。SQLite `SQLITE_BUSY` / `SQLITE_LOCKED` 在 [`DeepseeknovaError::is_retryable`]
/// 中被识别为可重试。
impl From<rusqlite::Error> for DeepseeknovaError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Storage(e.to_string())
    }
}

impl DeepseeknovaError {
    /// 构造一个不可重试的 provider 错误（认证、参数非法、解析失败等）。
    pub fn provider(message: impl Into<String>) -> Self {
        Self::Provider {
            message: message.into(),
            retryable: false,
        }
    }

    /// 构造一个可重试的 provider 错误（限流、超时、5xx、瞬时网络故障）。
    ///
    /// 调用方不应自行按消息猜测重试语义；本构造函数仅供错误产生方
    /// （`deepseeknova-provider` 的发送路径与 `From<ProviderError>`）使用。
    pub fn provider_retryable(message: impl Into<String>) -> Self {
        Self::Provider {
            message: message.into(),
            retryable: true,
        }
    }

    /// 判断该错误是否值得重试。
    ///
    /// 返回 `true` 表示错误通常是瞬时的（IO 暂时性故障、provider 限流/超时），
    /// 调用方可使用退避重试策略；返回 `false` 表示错误是确定性的（权限拒绝、
    /// 配置非法、取消），重试无意义。
    pub fn is_retryable(&self) -> bool {
        match self {
            // 仅瞬时 IO 故障可重试；NotFound / PermissionDenied / AlreadyExists
            // 等确定性错误重试无意义。
            Self::Io(e) => matches!(
                e.kind(),
                std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::WouldBlock
                    | std::io::ErrorKind::Interrupted
                    | std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::NotConnected
                    | std::io::ErrorKind::NetworkUnreachable
                    | std::io::ErrorKind::HostUnreachable
            ),
            // 由产生方显式标注，不再依赖消息文本匹配（修复：原 `contains("5xx")`
            // 无法命中真实消息 `HTTP 500: ...`，5xx 重试语义丢失）。
            Self::Provider { retryable, .. } => *retryable,
            // SQLite SQLITE_BUSY / SQLITE_LOCKED 等瞬时锁竞争可重试；
            // 其余存储错误（schema、约束冲突、磁盘满）确定性，不重试。
            Self::Storage(msg) => {
                msg.contains("database is locked")
                    || msg.contains("database table is locked")
                    || msg.to_lowercase().contains("sqlite_busy")
            }
            _ => false,
        }
    }

    /// 返回脱敏后的错误描述，可安全写入日志/遥测。
    ///
    /// 当前实现移除常见敏感模式：
    /// - `sk-` 前缀的 API key（如 OpenAI 风格）
    /// - `Bearer <token>` 形式的认证头
    /// - `api_key=...` / `api-key: ...` 形式的查询/头参数
    ///
    /// 后续可扩展到更复杂的模式匹配。错误类别与变体名保留在描述中，便于
    /// 日志聚合分析。
    ///
    /// 正则编译失败时（理论上不会发生，模式为字面量）回退到原文返回，不
    /// 阻塞调用方。
    pub fn redacted(&self) -> String {
        let raw = self.to_string();
        let re = REDACTION_RE.get_or_init(|| {
            regex::Regex::new(
                r"(?i)(sk-[A-Za-z0-9_\-]{20,}|Bearer\s+[A-Za-z0-9_\-\.]{20,}|api[_-]?key[=:]\s*\S+)",
            )
            .ok()
        });
        match re {
            Some(re) => re.replace_all(&raw, "[REDACTED]").to_string(),
            None => raw,
        }
    }
}

/// 全局编译一次的脱敏正则（线程安全，惰性初始化）。
static REDACTION_RE: OnceLock<Option<regex::Regex>> = OnceLock::new();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_retryable_only_for_transient_kinds() {
        let err =
            DeepseeknovaError::from(std::io::Error::new(std::io::ErrorKind::NotFound, "missing"));
        assert!(matches!(err, DeepseeknovaError::Io(_)));
        assert!(!err.is_retryable(), "NotFound 是确定性错误，不应建议重试");

        let transient = DeepseeknovaError::from(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "timed out",
        ));
        assert!(transient.is_retryable(), "TimedOut 是瞬时错误，应可重试");
    }

    #[test]
    fn from_serde_error_uses_serde_variant() {
        let err: DeepseeknovaError = serde_json::from_str::<serde_json::Value>("{bad}")
            .unwrap_err()
            .into();
        assert!(matches!(err, DeepseeknovaError::Serde(_)));
        assert!(!err.is_retryable());
    }

    #[test]
    fn cancelled_is_not_retryable() {
        let err = DeepseeknovaError::Cancelled;
        assert!(!err.is_retryable());
        assert_eq!(err.to_string(), "operation cancelled");
    }

    #[test]
    fn provider_rate_limited_is_retryable() {
        let err = DeepseeknovaError::provider_retryable("rate limited — retry after 30s");
        assert!(err.is_retryable());
    }

    #[test]
    fn provider_timeout_is_retryable() {
        let err = DeepseeknovaError::provider_retryable("timeout after 30s");
        assert!(err.is_retryable());
    }

    #[test]
    fn provider_auth_is_not_retryable() {
        let err = DeepseeknovaError::provider("authentication failed: bad key");
        assert!(!err.is_retryable());
    }

    #[test]
    fn redacted_strips_sk_keys() {
        let err = DeepseeknovaError::provider(
            "auth failed with key sk-abcdef1234567890abcdef1234567890".to_string(),
        );
        let r = err.redacted();
        assert!(!r.contains("sk-abcdef"), "sk- key must be redacted: {r}");
        assert!(r.contains("[REDACTED]"));
    }

    #[test]
    fn redacted_strips_bearer_tokens() {
        let err = DeepseeknovaError::provider(
            "401 with Bearer abcdefghijklmnopqrstuvwxyz123456".to_string(),
        );
        let r = err.redacted();
        assert!(!r.contains("Bearer abcdef"), "Bearer must be redacted: {r}");
        assert!(r.contains("[REDACTED]"));
    }

    #[test]
    fn redacted_strips_api_key_query_param() {
        let err =
            DeepseeknovaError::Config("url api_key=ABCDEF1234567890ABCDEF invalid".to_string());
        let r = err.redacted();
        assert!(
            !r.contains("ABCDEF1234567890"),
            "api_key value must be redacted: {r}"
        );
        assert!(r.contains("[REDACTED]"));
    }

    #[test]
    fn io_to_deepseeknova_via_question_mark() {
        fn inner() -> std::io::Result<()> {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "nope",
            ))
        }
        fn outer() -> Result<(), DeepseeknovaError> {
            inner()?;
            Ok(())
        }
        let err = outer().unwrap_err();
        assert!(matches!(err, DeepseeknovaError::Io(_)));
    }

    #[test]
    fn variants_render_human_readable_messages() {
        assert_eq!(
            DeepseeknovaError::Config("missing field".into()).to_string(),
            "configuration error: missing field"
        );
        assert_eq!(
            DeepseeknovaError::Permission("denied".into()).to_string(),
            "permission error: denied"
        );
        assert_eq!(
            DeepseeknovaError::Tool("exec failed".into()).to_string(),
            "tool error: exec failed"
        );
    }
}

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

use std::error::Error;
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
    ///
    /// 当前为自由格式消息（`source` 为 `None`）；未来若引入类型化
    /// `ConfigError`，可通过 `From<ConfigError>` 路径保留 source 链。
    #[error("configuration error: {message}")]
    Config {
        /// 人可读错误描述。
        message: String,
        /// 原始类型化错误；自由格式消息时为 `None`。
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    /// IO 错误：文件系统、网络读写。
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// 序列化错误：JSON / YAML / TOML 解析或编码失败。
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    /// 权限错误：工具调用被拒、需用户审批、策略非法、路径越界、能力缺失。
    ///
    /// 由 `deepseeknova-permission` crate 的
    /// `From<PermissionError> for DeepseeknovaError` 映射到本变体（`source`
    /// 持有原始 `PermissionError`，调用方可通过 `source().downcast_ref::<
    /// PermissionError>()` 恢复 `Denied` / `RequiresApproval` /
    /// `InvalidPolicy` / `Io` 等具体变体）。`security` crate 的路径校验与
    /// 能力门禁失败也使用本变体，`source` 为 `None`（自由格式消息）。
    ///
    /// `Denied` / `RequiresApproval` / `InvalidPolicy` 均为确定性错误，
    /// [`is_retryable`](Self::is_retryable) 返回 `false`。
    #[error("permission error: {message}")]
    Permission {
        /// 人可读错误描述（保持与旧 `Permission(String)` 的 Display 一致）。
        message: String,
        /// 原始类型化错误（`PermissionError`）；自由格式消息时为 `None`。
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

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
    /// 由 `deepseeknova-graph` crate 的
    /// `From<GraphError> for DeepseeknovaError` 映射到本变体。变体持有
    /// `Box<dyn Error>` 以保留原始 `GraphError` 的变体信息与 source 链，
    /// 调用方可通过 `source().downcast_ref::<GraphError>()` 恢复具体变体
    /// （`Parse` / `Storage` / `IndexBusy` / `EntityNotFound`）。
    ///
    /// Display 输出与旧 `Graph(String)` 一致（`{0}` 走 boxed error 的 Display）。
    #[error("graph error: {0}")]
    Graph(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// 工具执行错误：工具内部执行失败。
    ///
    /// 当前为自由格式消息（`source` 为 `None`）；未来若引入类型化
    /// `ToolError`，可通过 `From<ToolError>` 路径保留 source 链。
    #[error("tool error: {message}")]
    Tool {
        /// 人可读错误描述。
        message: String,
        /// 原始类型化错误；自由格式消息时为 `None`。
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    /// 运行器错误：Runner/Executor/Planner 执行流失败。
    ///
    /// 当前为自由格式消息（`source` 为 `None`）；未来若引入类型化
    /// `RunnerError`，可通过 `From<RunnerError>` 路径保留 source 链。
    #[error("runner error: {message}")]
    Runner {
        /// 人可读错误描述。
        message: String,
        /// 原始类型化错误；自由格式消息时为 `None`。
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    /// 上下文错误：历史不变量违反、压缩失败、构造器顺序非法。
    ///
    /// 由 `deepseeknova-context` crate 的 `From<CompactionError>` /
    /// `From<InvariantViolation>` / `From<BuilderOrderError>` 映射到本变体。
    /// 变体持有 `Box<dyn Error>` 以保留原始错误类型与 source 链，调用方可
    /// 通过 `source().downcast_ref::<T>()` 恢复具体变体。
    ///
    /// Display 输出与旧 `Context(String)` 一致。
    #[error("context error: {0}")]
    Context(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// 存储错误：SQLite/FTS5 读写、schema 校验、事务失败等。
    ///
    /// Phase 4 引入：通过 `From<rusqlite::Error>` 把 SQLite 错误桥接到本变体，
    /// 供 `core/memory` 与 `deepseeknova-store` 的存储层错误统一使用；
    /// `deepseeknova-graph` 的存储错误经 `From<GraphError>` 归入 `Graph` 变体。
    ///
    /// `From<rusqlite::Error>` 路径的 `source` 持有原始 `rusqlite::Error`，
    /// [`is_retryable`](Self::is_retryable) 基于其 `SQLITE_BUSY` /
    /// `SQLITE_LOCKED` 基础码（含 extended code）类型化判断，不依赖消息文本。
    #[error("storage error: {message}")]
    Storage {
        /// 人可读错误描述（保持与旧 `Storage(String)` 的 Display 一致）。
        message: String,
        /// 原始类型化错误（`rusqlite::Error`）；自由格式消息时为 `None`。
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    /// Agent 错误：任务规格、@提及、清单解析失败、子代理未注册等。
    ///
    /// 由 `deepseeknova-agent` crate 的 `From<TaskSpecError>` /
    /// `From<MentionError>` / `From<ManifestError>` 映射到本变体（`source`
    /// 持有原始错误，调用方可通过 `source().downcast_ref::<T>()` 恢复具体
    /// 变体）。子代理注册/派发的自由格式错误也使用本变体，`source` 为 `None`。
    #[error("agent error: {message}")]
    Agent {
        /// 人可读错误描述（保持与旧 `Agent(String)` 的 Display 一致）。
        message: String,
        /// 原始类型化错误（`TaskSpecError` / `MentionError` / `ManifestError`）；
        /// 自由格式消息时为 `None`。
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    /// 操作被取消（用户中断、cancellation token 触发）。
    ///
    /// 取消是不可恢复的确定性状态，[`is_retryable`](Self::is_retryable)
    /// 返回 `false`。
    #[error("operation cancelled")]
    Cancelled,
}

/// Phase 4：把 `rusqlite::Error` 桥接到 [`DeepseeknovaError::Storage`]，
/// 供 `core/memory` 子系统（MemoryStore）与 `deepseeknova-store` 的存储层
/// 错误统一使用。`source` 持有原始 `rusqlite::Error`，使
/// [`DeepseeknovaError::is_retryable`] 能基于 `SQLITE_BUSY` / `SQLITE_LOCKED`
/// 基础码类型化判断（含 extended code 如 `SQLITE_BUSY_SNAPSHOT`），不依赖
/// 消息文本匹配。
impl From<rusqlite::Error> for DeepseeknovaError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Storage {
            message: e.to_string(),
            source: Some(Box::new(e)),
        }
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

    /// 构造一个自由格式权限错误（路径越界、能力缺失等无对应类型化错误的场景）。
    ///
    /// `source` 为 `None`；类型化权限错误应通过 `From<PermissionError>` 走
    /// 带源链的路径，而非本构造函数。
    pub fn permission(message: impl Into<String>) -> Self {
        Self::Permission {
            message: message.into(),
            source: None,
        }
    }

    /// 构造一个自由格式 agent 错误（子代理未注册、派发失败等无对应类型化
    /// 错误的场景）。
    ///
    /// `source` 为 `None`；类型化 agent 错误应通过 `From<TaskSpecError>` /
    /// `From<MentionError>` / `From<ManifestError>` 走带源链的路径。
    pub fn agent(message: impl Into<String>) -> Self {
        Self::Agent {
            message: message.into(),
            source: None,
        }
    }

    /// 构造一个自由格式存储错误（schema 非法、约束冲突、磁盘满等无对应
    /// 类型化错误的场景）。
    ///
    /// `source` 为 `None`，[`is_retryable`](Self::is_retryable) 对此类错误
    /// 返回 `false`；类型化存储错误应通过 `From<rusqlite::Error>` 走带源链
    /// 的路径，以保留 `SQLITE_BUSY` / `SQLITE_LOCKED` 的可重试判断。
    pub fn storage(message: impl Into<String>) -> Self {
        Self::Storage {
            message: message.into(),
            source: None,
        }
    }

    /// 构造一个自由格式配置错误（解析失败、字段缺失、值非法等无对应
    /// 类型化错误的场景）。
    ///
    /// `source` 为 `None`；未来引入类型化 `ConfigError` 后，`From<ConfigError>`
    /// 路径会保留 source 链。
    pub fn config(message: impl Into<String>) -> Self {
        Self::Config {
            message: message.into(),
            source: None,
        }
    }

    /// 构造一个自由格式工具执行错误。
    ///
    /// `source` 为 `None`；未来引入类型化 `ToolError` 后，`From<ToolError>`
    /// 路径会保留 source 链。
    pub fn tool(message: impl Into<String>) -> Self {
        Self::Tool {
            message: message.into(),
            source: None,
        }
    }

    /// 构造一个自由格式运行器错误。
    ///
    /// `source` 为 `None`；未来引入类型化 `RunnerError` 后，`From<RunnerError>`
    /// 路径会保留 source 链。
    pub fn runner(message: impl Into<String>) -> Self {
        Self::Runner {
            message: message.into(),
            source: None,
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
            // 基于 source 保留的 rusqlite::Error 类型化判断，不依赖消息文本
            // （修复：原 contains("database is locked") 无法覆盖非英文/变体
            // 消息，且 extended code 如 SQLITE_BUSY_SNAPSHOT 消息不含 "busy"）。
            Self::Storage { .. } => match self
                .source()
                .and_then(|s| s.downcast_ref::<rusqlite::Error>())
            {
                Some(rusqlite::Error::SqliteFailure(ffi_err, _)) => {
                    let base = ffi_err.extended_code & 0xFF;
                    base == rusqlite::ffi::SQLITE_BUSY || base == rusqlite::ffi::SQLITE_LOCKED
                }
                _ => false,
            },
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
        let err = DeepseeknovaError::config("url api_key=ABCDEF1234567890ABCDEF invalid");
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
            DeepseeknovaError::config("missing field").to_string(),
            "configuration error: missing field"
        );
        assert_eq!(
            DeepseeknovaError::permission("denied").to_string(),
            "permission error: denied"
        );
        assert_eq!(
            DeepseeknovaError::tool("exec failed").to_string(),
            "tool error: exec failed"
        );
        assert_eq!(
            DeepseeknovaError::storage("disk full").to_string(),
            "storage error: disk full"
        );
    }

    #[test]
    fn storage_from_rusqlite_preserves_source_for_downcast() {
        // 构造一个 SQLITE_BUSY 错误（基础码 5），验证 From<rusqlite::Error>
        // 保留了原始错误类型，使 is_retryable 能基于类型化判断返回 true。
        let rusqlite_err = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            Some("database is locked".into()),
        );
        let err: DeepseeknovaError = rusqlite_err.into();
        assert!(
            err.is_retryable(),
            "SQLITE_BUSY 应被识别为可重试（基于 source 类型化判断）"
        );
        // source 可 downcast 回 rusqlite::Error
        let src = err
            .source()
            .unwrap()
            .downcast_ref::<rusqlite::Error>()
            .unwrap();
        assert!(matches!(
            src,
            rusqlite::Error::SqliteFailure(_, Some(msg)) if msg == "database is locked"
        ));
    }

    #[test]
    fn storage_free_form_is_not_retryable() {
        // 自由格式 storage 错误（source=None）不应可重试。
        let err = DeepseeknovaError::storage("schema mismatch");
        assert!(!err.is_retryable());
        assert!(err.source().is_none());
    }

    #[test]
    fn storage_non_busy_rusqlite_is_not_retryable() {
        // SQLITE_CANTOPEN（基础码 14）不是锁竞争，不应可重试。
        let rusqlite_err = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
            None,
        );
        let err: DeepseeknovaError = rusqlite_err.into();
        assert!(
            !err.is_retryable(),
            "SQLITE_CANTOPEN 是确定性错误，不应重试"
        );
    }
}

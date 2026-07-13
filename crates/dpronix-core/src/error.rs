use thiserror::Error;

/// CoreError is the typed error for dpronix-core.
/// Downstream crates may wrap it or use anyhow for application-level errors.
#[derive(Debug, Error)]
pub enum CoreError {
    #[error("registry: {0}")]
    Registry(String),

    #[error("graph: {0}")]
    Graph(String),

    #[error("execution: {0}")]
    Execution(String),

    #[error("tool validation: {0}")]
    ToolValidation(String),

    #[error("timeout after {0:?}")]
    Timeout(std::time::Duration),

    #[error("cancelled")]
    Cancelled,

    #[error("provider: {0}")]
    Provider(String),

    #[error("config: {0}")]
    Config(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

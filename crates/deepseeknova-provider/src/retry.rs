//! — Exponential backoff with jitter for provider HTTP calls.
//!
//! Retry decisions:
//! - Network/connection errors → always retry
//! - HTTP 429 (rate limited) → retry after `Retry-After` header or default backoff
//! - HTTP 5xx → retry
//! - HTTP 4xx (except 429) → no retry
//! - HTTP 2xx/3xx → no retry

use std::time::Duration;
use tokio::time::sleep;

const BASE_DELAY_MS: u64 = 500;
const MAX_DELAY_MS: u64 = 30_000;
const JITTER_FACTOR: f64 = 0.25;

/// The outcome of a single HTTP attempt.
pub enum HttpAttempt<T> {
    /// Success — stop retrying.
    Success(T),
    /// Retryable failure — network error, 5xx, 429.
    Retryable {
        /// Human-readable diagnostic message (HTTP response body or transport error).
        message: String,
        /// HTTP status code when the failure was a non-2xx HTTP response; `None` for transport errors.
        status: Option<u16>,
        /// Server-provided `Retry-After` hint — the backoff prefers it over the
        /// default exponential schedule when present.
        retry_after: Option<Duration>,
    },
    /// Non-retryable failure — 4xx (except 429), auth, invalid request.
    Fatal {
        /// Human-readable diagnostic message.
        message: String,
        /// HTTP status code when the failure was a non-2xx HTTP response; `None` for transport errors.
        status: Option<u16>,
    },
}

/// Execute `f` with exponential-backoff retry up to `max_retries` attempts.
/// `max_retries=0` means a single attempt with no retry.
/// The closure `f` must be `'static` — clone captured values before the closure.
pub async fn retry_with_backoff<T>(
    max_retries: u32,
    f: impl Fn() -> futures::future::BoxFuture<'static, HttpAttempt<T>>,
) -> HttpAttempt<T> {
    let max_attempts = max_retries.saturating_add(1).max(1);
    let mut attempt = 0u32;

    loop {
        let result = f().await;
        attempt += 1;

        match result {
            HttpAttempt::Success(val) => return HttpAttempt::Success(val),
            HttpAttempt::Fatal { .. } => return result,
            HttpAttempt::Retryable {
                message,
                status,
                retry_after,
            } => {
                if attempt >= max_attempts {
                    return HttpAttempt::Retryable {
                        message,
                        status,
                        retry_after,
                    };
                }
                // T-M1：优先采用服务端 `Retry-After` 提示；缺失/无法解析时
                // 回落默认指数退避。服务端提示是不可信输入（429 头可被网关
                // 恶意/误配为大值），必须钳制到与默认退避相同的上限
                // MAX_DELAY_MS，否则 `Retry-After: 86400` 会让 CLI 挂起数小时
                //（审查#2：无界 sleep = 可靠性挂起 + DoS 面）。
                let delay = retry_after
                    .map(|d| d.min(Duration::from_millis(MAX_DELAY_MS)))
                    .unwrap_or_else(|| backoff_duration(attempt));
                sleep(delay).await;
            }
        }
    }
}

/// Compute exponential backoff with jitter for the given attempt number
/// (1-based).  E.g. attempt=1 → ~500ms, attempt=2 → ~1s, attempt=3 → ~2s, …
pub(crate) fn backoff_duration(attempt: u32) -> Duration {
    let exp = BASE_DELAY_MS as f64 * (2.0f64).powi((attempt.saturating_sub(1)) as i32);
    let capped = exp.min(MAX_DELAY_MS as f64);
    let jitter = (rand::random::<f64>() - 0.5) * 2.0 * JITTER_FACTOR * capped;
    let ms = (capped + jitter).max(1.0) as u64;
    Duration::from_millis(ms)
}

/// Check if an HTTP status code is retryable.
pub fn is_retryable_status(status: u16) -> bool {
    status == 429 || (500..=599).contains(&status)
}

/// Check if an error string suggests a retryable network failure.
pub fn is_retryable_error(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    // Connection / timeout / DNS / TLS errors
    lower.contains("connection refused")
        || lower.contains("connection reset")
        || lower.contains("connection aborted")
        || lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("dns")
        || lower.contains("tls")
        || lower.contains("eof")
        || lower.contains("broken pipe")
        || lower.contains("no route to host")
}

/// 解析 `Retry-After` 响应头的值。
///
/// 支持 RFC 9110 的 delay-seconds 形式（非负整数的秒数）；HTTP-date 形式
/// （LLM 网关极少使用）不做解析。头缺失、值为空或无法解析时返回 `None`，
/// 调用方回落默认指数退避。
pub(crate) fn parse_retry_after(header_value: Option<&str>) -> Option<Duration> {
    let value = header_value?.trim();
    if value.is_empty() {
        return None;
    }
    let secs: u64 = value.parse().ok()?;
    Some(Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_increases_with_attempts() {
        let d1 = backoff_duration(1);
        let d2 = backoff_duration(2);
        let d3 = backoff_duration(3);
        assert!(d2 >= d1, "attempt 2 should be >= attempt 1");
        assert!(d3 >= d2, "attempt 3 should be >= attempt 2");
    }

    #[test]
    fn backoff_is_capped() {
        let d = backoff_duration(10);
        assert!(
            d <= Duration::from_millis(MAX_DELAY_MS + (MAX_DELAY_MS as f64 * JITTER_FACTOR) as u64)
        );
    }

    #[test]
    fn retryable_status_checks() {
        assert!(is_retryable_status(429));
        assert!(is_retryable_status(500));
        assert!(is_retryable_status(502));
        assert!(is_retryable_status(503));
        assert!(!is_retryable_status(200));
        assert!(!is_retryable_status(400));
        assert!(!is_retryable_status(401));
        assert!(!is_retryable_status(403));
        assert!(!is_retryable_status(404));
    }

    #[test]
    fn retryable_error_checks() {
        assert!(is_retryable_error("connection refused"));
        assert!(is_retryable_error("connection reset by peer"));
        assert!(is_retryable_error("timed out"));
        assert!(is_retryable_error("Request timed out after 30s"));
        assert!(is_retryable_error("dns resolution failed"));
        assert!(is_retryable_error("tls handshake failed"));
        assert!(!is_retryable_error("permission denied"));
        assert!(!is_retryable_error("invalid api key"));
    }

    #[tokio::test]
    async fn retry_success_first_try() {
        let result = retry_with_backoff(3, || Box::pin(async { HttpAttempt::Success(42) })).await;
        match result {
            HttpAttempt::Success(v) => assert_eq!(v, 42),
            _ => panic!("expected success"),
        }
    }

    #[tokio::test]
    async fn retry_fatal_stops_immediately() {
        let count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let c = count.clone();
        let result: HttpAttempt<String> = retry_with_backoff(3, move || {
            c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Box::pin(async move {
                HttpAttempt::Fatal {
                    message: "bad request".to_string(),
                    status: None,
                }
            })
        })
        .await;
        assert_eq!(
            count.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "fatal errors should not retry"
        );
        match result {
            HttpAttempt::Fatal { message, status } => {
                assert_eq!(message, "bad request");
                assert_eq!(status, None);
            }
            _ => panic!("expected fatal"),
        }
    }

    #[tokio::test]
    async fn retry_eventually_succeeds() {
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let a = attempts.clone();
        let result = retry_with_backoff(3, move || {
            let prev = a.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Box::pin(async move {
                if prev < 2 {
                    HttpAttempt::Retryable {
                        message: "transient".into(),
                        status: None,
                        retry_after: None,
                    }
                } else {
                    HttpAttempt::Success("ok")
                }
            })
        })
        .await;
        assert_eq!(attempts.load(std::sync::atomic::Ordering::Relaxed), 3);
        match result {
            HttpAttempt::Success(v) => assert_eq!(v, "ok"),
            _ => panic!("expected success"),
        }
    }

    #[test]
    fn parse_retry_after_seconds() {
        // T-M1：delay-seconds 形式的 `Retry-After` 应解析为对应 Duration。
        assert_eq!(
            parse_retry_after(Some("120")),
            Some(Duration::from_secs(120))
        );
        assert_eq!(parse_retry_after(Some("0")), Some(Duration::ZERO));
        assert_eq!(parse_retry_after(Some(" 5 ")), Some(Duration::from_secs(5)));
    }

    #[test]
    fn parse_retry_after_invalid_returns_none() {
        // T-M1：头缺失、空值、负数、小数与 HTTP-date 形式都回落默认退避。
        assert_eq!(parse_retry_after(None), None);
        assert_eq!(parse_retry_after(Some("")), None);
        assert_eq!(parse_retry_after(Some("  ")), None);
        assert_eq!(parse_retry_after(Some("-5")), None);
        assert_eq!(parse_retry_after(Some("1.5")), None);
        assert_eq!(
            parse_retry_after(Some("Wed, 21 Oct 2015 07:28:00 GMT")),
            None
        );
    }

    #[tokio::test]
    async fn retry_uses_server_retry_after_hint() {
        // T-M1：服务端 `Retry-After`（5ms）应优先于默认指数退避（~500ms），
        // 用远小于默认退避的提示值验证重试耗时显著更短。
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let a = attempts.clone();
        let start = std::time::Instant::now();
        let result = retry_with_backoff(3, move || {
            let prev = a.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Box::pin(async move {
                if prev == 0 {
                    HttpAttempt::Retryable {
                        message: "HTTP 429: rate limited".to_string(),
                        status: Some(429),
                        retry_after: Some(Duration::from_millis(5)),
                    }
                } else {
                    HttpAttempt::Success("ok")
                }
            })
        })
        .await;
        assert_eq!(attempts.load(std::sync::atomic::Ordering::Relaxed), 2);
        assert!(
            start.elapsed() < Duration::from_millis(250),
            "应优先采用服务端 Retry-After（5ms），而非默认 ~500ms 退避"
        );
        match result {
            HttpAttempt::Success(v) => assert_eq!(v, "ok"),
            _ => panic!("expected success"),
        }
    }

    #[test]
    fn oversized_retry_after_is_clamped() {
        // 审查#2：服务端 `Retry-After` 是不可信输入，超限值必须钳制到
        // MAX_DELAY_MS，否则 `Retry-After: 86400` 会让 CLI 挂起数小时。
        // 通过解析出的 Duration 与钳制上限做纯函数断言（不实际 sleep）。
        let huge = parse_retry_after(Some("86400")).expect("86400 秒可解析");
        let clamped = huge.min(Duration::from_millis(MAX_DELAY_MS));
        assert_eq!(
            clamped,
            Duration::from_millis(MAX_DELAY_MS),
            "超限 Retry-After 应被钳制到 MAX_DELAY_MS"
        );
        // 正常小值不受影响（仍为原值，非钳制）。
        let small = parse_retry_after(Some("2")).expect("2 秒可解析");
        assert_eq!(small, Duration::from_secs(2));
        assert_eq!(small.min(Duration::from_millis(MAX_DELAY_MS)), small);
    }
}

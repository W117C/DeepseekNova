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
    Retryable(String),
    /// Non-retryable failure — 4xx (except 429), auth, invalid request.
    Fatal(String),
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
            HttpAttempt::Fatal(msg) => return HttpAttempt::Fatal(msg),
            HttpAttempt::Retryable(msg) => {
                if attempt >= max_attempts {
                    return HttpAttempt::Retryable(msg);
                }
                let delay = backoff_duration(attempt);
                sleep(delay).await;
            }
        }
    }
}

/// Compute exponential backoff with jitter for the given attempt number
/// (1-based).  E.g. attempt=1 → ~500ms, attempt=2 → ~1s, attempt=3 → ~2s, …
fn backoff_duration(attempt: u32) -> Duration {
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
        assert!(d <= Duration::from_millis(MAX_DELAY_MS + (MAX_DELAY_MS as f64 * JITTER_FACTOR) as u64));
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
        let result = retry_with_backoff(3, || {
            Box::pin(async { HttpAttempt::Success(42) })
        }).await;
        match result {
            HttpAttempt::Success(v) => assert_eq!(v, 42),
            _ => panic!("expected success"),
        }
    }

    #[tokio::test]
    async fn retry_fatal_stops_immediately() {
        let count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let c = count.clone();
        let result = retry_with_backoff(3, move || {
            c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Box::pin(async move { HttpAttempt::Fatal("bad request".into()) })
        }).await;
        assert_eq!(count.load(std::sync::atomic::Ordering::Relaxed), 1, "fatal errors should not retry");
        match result {
            HttpAttempt::Fatal(msg) => assert_eq!(msg, "bad request"),
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
                    HttpAttempt::Retryable("transient".into())
                } else {
                    HttpAttempt::Success("ok")
                }
            })
        }).await;
        assert_eq!(attempts.load(std::sync::atomic::Ordering::Relaxed), 3);
        match result {
            HttpAttempt::Success(v) => assert_eq!(v, "ok"),
            _ => panic!("expected success"),
        }
    }
}
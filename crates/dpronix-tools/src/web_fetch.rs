use async_trait::async_trait;
use dpronix_core::{Tool, ToolContext, ToolSchema};
use serde::Deserialize;
use serde_json::json;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;
use std::time::Duration;
use tokio::net::lookup_host;

const DEFAULT_FETCH_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024; // 5 MB

pub struct WebFetchTool;

#[derive(Deserialize)]
struct WebFetchArgs {
    url: String,
}

#[async_trait]
impl Tool for WebFetchTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "web_fetch".to_string(),
            description: "Fetches content from a URL and returns it as text. \
                Only HTTP and HTTPS schemes are allowed. \
                Access to private, loopback, and link-local addresses is blocked."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL to fetch content from."
                    }
                },
                "required": ["url"]
            }),
        }
    }

    fn read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        dpronix_security::context::enforce_capability(
            ctx,
            dpronix_security::capability::Capability::NetworkAccess,
        )?;
        let parsed: WebFetchArgs = serde_json::from_str(args)?;

        if ctx.cancellation.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        // Step 1 — Parse and validate the URL scheme + host
        let url = validate_url(&parsed.url)?;

        // Step 2 — Resolve DNS and check resolved IPs are safe
        let host = url
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("URL has no host: {}", parsed.url))?;
        if let Some(sec) = ctx
            .extensions
            .get::<dpronix_security::context::SecurityContext>()
        {
            if !sec.policy.is_domain_allowed(host) {
                anyhow::bail!(
                    "Security violation: domain '{}' is blocked by security policy",
                    host
                );
            }
        }
        validate_host_ssrf(host).await?;

        // Step 3 — Build an HTTP client (no automatic redirects — we re-validate each hop)
        let client = build_ssrf_safe_client()?;

        // Step 4 — Fetch with timeout, manually following redirects with re-validation
        let body = fetch_with_redirects(&client, url.clone(), 0).await?;

        Ok(body)
    }
}

// ---------------------------------------------------------------------------
// URL validation
// ---------------------------------------------------------------------------

/// Accept only http/https, reject non-parseable and non-standard URLs.
fn validate_url(raw: &str) -> anyhow::Result<url::Url> {
    let url = url::Url::parse(raw).map_err(|e| anyhow::anyhow!("invalid URL '{raw}': {e}"))?;

    match url.scheme() {
        "http" | "https" => {}
        other => {
            anyhow::bail!("unsupported URL scheme '{other}'; only http and https are allowed")
        }
    }

    // Reject URLs where the host is not a domain or IP (e.g. "data:", "file:", "javascript:")
    if url.host_str().is_none() {
        anyhow::bail!("URL has no host: {raw}");
    }

    Ok(url)
}

// ---------------------------------------------------------------------------
// SSRF-safe DNS resolution
// ---------------------------------------------------------------------------

/// Resolve the hostname to IP addresses and reject any that fall into
/// private, loopback, link-local, or other unsafe ranges.
async fn validate_host_ssrf(host: &str) -> anyhow::Result<()> {
    // Handle raw IPv4 and IPv6 addresses directly so we don't rely on DNS.
    if let Ok(ip) = IpAddr::from_str(host) {
        ensure_safe_ip(&ip)?;
        return Ok(());
    }

    // For IPv6 addresses enclosed in brackets (e.g. "[::1]"), strip brackets.
    if let Some(inner) = host.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        if let Ok(ip) = IpAddr::from_str(inner) {
            ensure_safe_ip(&ip)?;
            return Ok(());
        }
    }

    // Resolve DNS (append :0 for port since lookup_host requires a SocketAddr-ish string).
    let sockaddr_str = if host.contains(':') {
        // IPv6 without brackets — bracket it for lookup_host
        format!("[{host}]:0")
    } else {
        format!("{host}:0")
    };

    let addrs: Vec<_> = lookup_host(&sockaddr_str).await?.collect();

    if addrs.is_empty() {
        anyhow::bail!("DNS resolution returned no addresses for '{host}'");
    }

    for addr in &addrs {
        ensure_safe_ip(&addr.ip())?;
    }

    Ok(())
}

/// Reject IP addresses in private, loopback, link-local, or other unsafe ranges.
fn ensure_safe_ip(ip: &IpAddr) -> anyhow::Result<()> {
    match ip {
        IpAddr::V4(v4) => {
            if is_unsafe_ipv4(v4) {
                anyhow::bail!("access to {ip} is blocked (private/internal network)");
            }
        }
        IpAddr::V6(v6) => {
            if is_unsafe_ipv6(v6) {
                anyhow::bail!("access to {ip} is blocked (private/internal network)");
            }
        }
    }
    Ok(())
}

fn is_unsafe_ipv4(ip: &Ipv4Addr) -> bool {
    let octets = ip.octets();
    // 0.0.0.0/8          — "This" network
    // 10.0.0.0/8         — Private
    // 127.0.0.0/8        — Loopback
    // 169.254.0.0/16     — Link-local
    // 172.16.0.0/12      — Private
    // 192.168.0.0/16     — Private
    // 224.0.0.0/4        — Multicast
    // 240.0.0.0/4        — Reserved (including 255.255.255.255)
    octets[0] == 0
        || octets[0] == 10
        || octets[0] == 127
        || (octets[0] == 169 && octets[1] == 254)
        || (octets[0] == 172 && (16..=31).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 168)
        || octets[0] >= 224
}

fn is_unsafe_ipv6(ip: &Ipv6Addr) -> bool {
    // ::1                     — Loopback
    // fe80::/10               — Link-local
    // fc00::/7                — Unique local (ULA)
    // ff00::/8                — Multicast
    // ::ffff:0:0/96           — IPv4-mapped (check embedded IPv4)
    ip.is_loopback()
        || is_ipv6_link_local(ip)
        || is_ipv6_unique_local(ip)
        || ip.is_multicast()
        || is_ipv6_ipv4_mapped_unsafe(ip)
}

/// fe80::/10
fn is_ipv6_link_local(ip: &Ipv6Addr) -> bool {
    let segments = ip.segments();
    segments[0] & 0xffc0 == 0xfe80
}

/// fc00::/7 (covers both fc00::/8 and fd00::/8)
fn is_ipv6_unique_local(ip: &Ipv6Addr) -> bool {
    let segments = ip.segments();
    segments[0] & 0xfe00 == 0xfc00
}

/// ::ffff:0:0/96 — map the embedded IPv4 and check it.
fn is_ipv6_ipv4_mapped_unsafe(ip: &Ipv6Addr) -> bool {
    let segments = ip.segments();
    // ::ffff:0:0/96 is [0, 0, 0, 0, 0, 0xffff, ..., ...]
    let is_v4_mapped = segments[0] == 0
        && segments[1] == 0
        && segments[2] == 0
        && segments[3] == 0
        && segments[4] == 0
        && segments[5] == 0xffff;

    if is_v4_mapped {
        // The embedded IPv4 address is in the lower 32 bits
        let v4 = Ipv4Addr::new(
            ((segments[6] >> 8) & 0xff) as u8,
            (segments[6] & 0xff) as u8,
            ((segments[7] >> 8) & 0xff) as u8,
            (segments[7] & 0xff) as u8,
        );
        return is_unsafe_ipv4(&v4);
    }

    false
}

// ---------------------------------------------------------------------------
// HTTP client with redirect SSRF protection
// ---------------------------------------------------------------------------

const MAX_REDIRECTS: u32 = 10;

fn build_ssrf_safe_client() -> anyhow::Result<reqwest::Client> {
    let client = reqwest::Client::builder()
        .timeout(DEFAULT_FETCH_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(format!("dpronix-tools/{}", env!("CARGO_PKG_VERSION")))
        .build()?;

    Ok(client)
}

/// Follow redirects manually, re-validating the SSRF safety of every hop.
fn fetch_with_redirects(
    client: &reqwest::Client,
    url: url::Url,
    depth: u32,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<String>> + Send + '_>> {
    Box::pin(async move {
        if depth > MAX_REDIRECTS {
            anyhow::bail!("too many redirects (max {MAX_REDIRECTS})");
        }

        let response = tokio::time::timeout(DEFAULT_FETCH_TIMEOUT, client.get(url.clone()).send())
            .await
            .map_err(|_| {
                anyhow::anyhow!("request timed out after {:?}", DEFAULT_FETCH_TIMEOUT)
            })??;

        let status = response.status();

        // Handle redirects
        if status.is_redirection() {
            if let Some(location) = response.headers().get(reqwest::header::LOCATION) {
                let location_str = location
                    .to_str()
                    .map_err(|_| anyhow::anyhow!("redirect Location header is not valid UTF-8"))?;

                let next_url = url.join(location_str).map_err(|e| {
                    anyhow::anyhow!("failed to resolve redirect Location '{location_str}': {e}")
                })?;

                // Validate scheme
                match next_url.scheme() {
                    "http" | "https" => {}
                    other => {
                        anyhow::bail!(
                            "redirect to unsupported scheme '{other}' is blocked (from {url})"
                        )
                    }
                }

                // Re-validate the redirect target's host against SSRF
                let next_host = next_url.host_str().ok_or_else(|| {
                    anyhow::anyhow!("redirect Location has no host: {location_str}")
                })?;
                validate_host_ssrf(next_host).await?;

                return fetch_with_redirects(client, next_url, depth + 1).await;
            }
        }

        // Check for error status
        if !status.is_success() {
            anyhow::bail!("HTTP {status} fetching {url}");
        }

        // Read body with size cap
        let body_bytes = tokio::time::timeout(DEFAULT_FETCH_TIMEOUT, response.bytes())
            .await
            .map_err(|_| anyhow::anyhow!("body read timed out"))??;

        if body_bytes.len() > MAX_RESPONSE_BYTES {
            anyhow::bail!(
                "response body exceeds maximum size of {MAX_RESPONSE_BYTES} bytes (got {} bytes)",
                body_bytes.len()
            );
        }

        let body = String::from_utf8(body_bytes.to_vec())
            .map_err(|e| anyhow::anyhow!("response is not valid UTF-8: {e}"))?;

        Ok(body)
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // ensure_safe_ip
    // -----------------------------------------------------------------------

    #[test]
    fn rejects_loopback_ipv4() {
        assert!(ensure_safe_ip(&IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))).is_err());
        assert!(ensure_safe_ip(&IpAddr::V4(Ipv4Addr::new(127, 255, 255, 255))).is_err());
    }

    #[test]
    fn rejects_private_10() {
        assert!(ensure_safe_ip(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))).is_err());
        assert!(ensure_safe_ip(&IpAddr::V4(Ipv4Addr::new(10, 255, 255, 255))).is_err());
    }

    #[test]
    fn rejects_private_172_16() {
        assert!(ensure_safe_ip(&IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1))).is_err());
        assert!(ensure_safe_ip(&IpAddr::V4(Ipv4Addr::new(172, 31, 255, 255))).is_err());
    }

    #[test]
    fn rejects_private_192_168() {
        assert!(ensure_safe_ip(&IpAddr::V4(Ipv4Addr::new(192, 168, 0, 1))).is_err());
        assert!(ensure_safe_ip(&IpAddr::V4(Ipv4Addr::new(192, 168, 255, 255))).is_err());
    }

    #[test]
    fn rejects_link_local() {
        assert!(ensure_safe_ip(&IpAddr::V4(Ipv4Addr::new(169, 254, 0, 1))).is_err());
    }

    #[test]
    fn rejects_multicast() {
        assert!(ensure_safe_ip(&IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1))).is_err());
    }

    #[test]
    fn rejects_loopback_ipv6() {
        assert!(ensure_safe_ip(&IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1))).is_err());
    }

    #[test]
    fn rejects_link_local_ipv6() {
        // fe80::1
        assert!(ensure_safe_ip(&IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1))).is_err());
    }

    #[test]
    fn rejects_ipv4_mapped_private() {
        // ::ffff:127.0.0.1
        assert!(ensure_safe_ip(&IpAddr::V6(Ipv6Addr::new(
            0, 0, 0, 0, 0, 0xffff, 0x7f00, 0x0001
        )))
        .is_err());
    }

    #[test]
    fn allows_public_ipv4() {
        assert!(ensure_safe_ip(&IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))).is_ok());
        assert!(ensure_safe_ip(&IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))).is_ok());
    }

    #[test]
    fn allows_public_ipv6() {
        // 2001:4860:4860::8888 (Google DNS)
        assert!(ensure_safe_ip(&IpAddr::V6(Ipv6Addr::new(
            0x2001, 0x4860, 0x4860, 0, 0, 0, 0, 0x8888
        )))
        .is_ok());
    }

    #[test]
    fn allows_172_15_public() {
        // 172.15.0.1 — outside the 172.16-31 private range, should be allowed
        assert!(ensure_safe_ip(&IpAddr::V4(Ipv4Addr::new(172, 15, 0, 1))).is_ok());
    }

    // -----------------------------------------------------------------------
    // validate_url
    // -----------------------------------------------------------------------

    #[test]
    fn rejects_file_scheme() {
        assert!(validate_url("file:///etc/passwd").is_err());
    }

    #[test]
    fn rejects_javascript_scheme() {
        assert!(validate_url("javascript:alert(1)").is_err());
    }

    #[test]
    fn rejects_data_scheme() {
        assert!(validate_url("data:text/html,<script>alert(1)</script>").is_err());
    }

    #[test]
    fn accepts_https() {
        assert!(validate_url("https://example.com").is_ok());
    }

    #[test]
    fn accepts_http() {
        assert!(validate_url("http://example.com").is_ok());
    }
}

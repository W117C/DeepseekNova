//! Web search tool — lets the agent "search the web" instead of only
//! fetching known URLs.
//!
//! Providers:
//! - `ddg` (default, no key): DuckDuckGo Instant Answer API.
//! - `tavily`: Tavily Search API (`TAVILY_API_KEY` by default).
//! - `bing`: Bing Web Search API (`BING_API_KEY` by default).
//! - `searxng`: self-hosted SearXNG instance (`base_url` required).

use async_trait::async_trait;
use deepseeknova_config::WebSearchConfig;
use deepseeknova_core::{DeepseeknovaError, Tool, ToolContext, ToolSchema};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

const SNIPPET_MAX_CHARS: usize = 320;
const MAX_RESULTS_CAP: usize = 10;
const MAX_SEARCH_REDIRECTS: u32 = 5;

/// Build the optional web-search tool set for agent registration.
/// HTTP 客户端构造失败时降级为空集合并告警——不 panic（L2）。
pub fn web_search_tools(cfg: &deepseeknova_config::ToolsConfig) -> Vec<Arc<dyn Tool>> {
    match WebSearchTool::new(cfg.web_search.clone()) {
        Ok(tool) => vec![Arc::new(tool)],
        Err(e) => {
            tracing::warn!("web_search tool disabled: failed to build HTTP client: {e}");
            Vec::new()
        }
    }
}

pub struct WebSearchTool {
    cfg: WebSearchConfig,
    /// 进程级/工具级一次构造的共享客户端：不自动跟随重定向，每个跳点
    /// 重新做域名/SSRF 校验（`search_get`）。
    client: reqwest::Client,
}

impl WebSearchTool {
    /// 构造工具。HTTP 客户端构建失败返回 `Err` 向上传播（L2：不再
    /// `expect` panic 宿主）。
    pub fn new(cfg: WebSearchConfig) -> Result<Self, DeepseeknovaError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_secs.max(1)))
            .user_agent(format!("deepseeknova-tools/{}", env!("CARGO_PKG_VERSION")))
            // 不自动跟随重定向：每个跳点都要重新做域名/SSRF 校验（同 web_fetch）。
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| DeepseeknovaError::tool(format!("failed to build HTTP client: {e}")))?;
        Ok(Self { cfg, client })
    }
}

#[derive(Deserialize)]
struct WebSearchArgs {
    query: String,
    #[serde(default)]
    max_results: Option<usize>,
}

#[derive(Debug, Clone)]
struct SearchHit {
    title: String,
    url: String,
    snippet: String,
}

#[async_trait]
impl Tool for WebSearchTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "web_search".to_string(),
            description: "Searches the web and returns ranked results (title, URL, snippet). "
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query."
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Maximum results (default: config value, capped at 10)."
                    }
                },
                "required": ["query"]
            }),
        }
    }

    fn read_only(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        ctx: &ToolContext,
        args: &str,
    ) -> Result<String, deepseeknova_core::DeepseeknovaError> {
        deepseeknova_security::context::enforce_capability(
            ctx,
            &self.schema().name,
            deepseeknova_security::capability::Capability::NetworkAccess,
        )?;
        let parsed: WebSearchArgs = serde_json::from_str(args)?;
        if parsed.query.trim().is_empty() {
            return Err(deepseeknova_core::DeepseeknovaError::tool(
                "web_search: query must not be empty".to_string(),
            ));
        }
        if ctx.cancellation.is_cancelled() {
            return Err(deepseeknova_core::DeepseeknovaError::Cancelled);
        }

        let max_results = parsed
            .max_results
            .unwrap_or(self.cfg.max_results)
            .min(MAX_RESULTS_CAP);
        let client = &self.client;

        let hits = match self.cfg.provider.as_str() {
            "tavily" => {
                let key = env_api_key(self.cfg.api_key_env.as_deref(), "TAVILY_API_KEY")?;
                let endpoint = self
                    .cfg
                    .base_url
                    .clone()
                    .unwrap_or_else(|| "https://api.tavily.com/search".to_string());
                check_domain_allowed(ctx, &endpoint)?;
                let body = json!({
                    "api_key": key,
                    "query": parsed.query,
                    "max_results": max_results,
                    "search_depth": "basic"
                });
                let resp = client
                    .post(&endpoint)
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| {
                        DeepseeknovaError::tool(format!("tavily search request failed: {e}"))
                    })?;
                let status = resp.status();
                let text = resp.text().await.map_err(|e| {
                    DeepseeknovaError::tool(format!("tavily search response read failed: {e}"))
                })?;
                if !status.is_success() {
                    return Err(deepseeknova_core::DeepseeknovaError::tool(format!(
                        "tavily search failed (HTTP {status}): {}",
                        truncate(&text, 300)
                    )));
                }
                parse_tavily(&text)?
            }
            "bing" => {
                let key = env_api_key(self.cfg.api_key_env.as_deref(), "BING_API_KEY")?;
                let base = self
                    .cfg
                    .base_url
                    .clone()
                    .unwrap_or_else(|| "https://api.bing.microsoft.com".to_string());
                let url = format!(
                    "{}/v7.0/search?q={}&count={}",
                    base.trim_end_matches('/'),
                    urlencode(&parsed.query),
                    max_results
                );
                let (status, text) = search_get(
                    ctx,
                    client,
                    &url,
                    &[("Ocp-Apim-Subscription-Key", key.as_str())],
                )
                .await?;
                if !status.is_success() {
                    return Err(deepseeknova_core::DeepseeknovaError::tool(format!(
                        "bing search failed (HTTP {status}): {}",
                        truncate(&text, 300)
                    )));
                }
                parse_bing(&text)?
            }
            "searxng" => {
                let base = self.cfg.base_url.as_deref().ok_or_else(|| {
                    DeepseeknovaError::tool("web_search: searxng requires base_url".to_string())
                })?;
                let url = format!(
                    "{}/search?q={}&format=json",
                    base.trim_end_matches('/'),
                    urlencode(&parsed.query)
                );
                let (status, text) = search_get(ctx, client, &url, &[]).await?;
                if !status.is_success() {
                    return Err(deepseeknova_core::DeepseeknovaError::tool(format!(
                        "searxng search failed (HTTP {status}): {}",
                        truncate(&text, 300)
                    )));
                }
                parse_searxng(&text)?
            }
            "ddg" => {
                let url = format!(
                    "https://api.duckduckgo.com/?q={}&format=json&no_html=1&skip_disambig=1",
                    urlencode(&parsed.query)
                );
                let (status, text) = search_get(ctx, client, &url, &[]).await?;
                if !status.is_success() {
                    return Err(deepseeknova_core::DeepseeknovaError::tool(format!(
                        "duckduckgo search failed (HTTP {status}): {}",
                        truncate(&text, 300)
                    )));
                }
                parse_ddg(&text)?
            }
            other => {
                return Err(deepseeknova_core::DeepseeknovaError::tool(format!(
                    "unknown web_search provider '{other}' \
                     (supported: ddg, tavily, bing, searxng)"
                )))
            }
        };

        if hits.is_empty() {
            return Ok("No web search results found.".to_string());
        }

        let mut out = String::new();
        for (i, hit) in hits.into_iter().take(max_results).enumerate() {
            out.push_str(&format!(
                "{}. {}\n   URL: {}\n   {}\n",
                i + 1,
                truncate(&hit.title, 200),
                hit.url,
                truncate(&hit.snippet, SNIPPET_MAX_CHARS)
            ));
        }
        Ok(out.trim_end().to_string())
    }
}

fn env_api_key(env_override: Option<&str>, default_env: &str) -> Result<String, DeepseeknovaError> {
    let name = env_override.unwrap_or(default_env);
    std::env::var(name).map_err(|_| {
        DeepseeknovaError::tool(format!(
            "web_search: provider requires API key; set environment variable {name} \
             or configure [tools.web_search] api_key_env"
        ))
    })
}

fn check_domain_allowed(ctx: &ToolContext, url: &str) -> Result<(), DeepseeknovaError> {
    if let Some(sec) = ctx
        .extensions
        .get::<deepseeknova_security::context::SecurityContext>()
    {
        let parsed = url::Url::parse(url)
            .map_err(|e| DeepseeknovaError::tool(format!("invalid search URL: {e}")))?;
        if let Some(host) = parsed.host_str() {
            if !sec.policy.is_domain_allowed(host) {
                return Err(DeepseeknovaError::tool(format!(
                    "Security violation: domain '{host}' is blocked by security policy"
                )));
            }
        }
    }
    Ok(())
}

/// GET with manual redirect handling: every hop is re-validated against the
/// domain policy and the same SSRF checks web_fetch applies. Returns the
/// final non-redirect status + body.
async fn search_get(
    ctx: &ToolContext,
    client: &reqwest::Client,
    url: &str,
    headers: &[(&str, &str)],
) -> Result<(reqwest::StatusCode, String), DeepseeknovaError> {
    let mut current = url.to_string();
    for _ in 0..=MAX_SEARCH_REDIRECTS {
        check_domain_allowed(ctx, &current)?;
        let parsed = url::Url::parse(&current)
            .map_err(|e| DeepseeknovaError::tool(format!("invalid search URL '{current}': {e}")))?;
        let host = parsed
            .host_str()
            .ok_or_else(|| DeepseeknovaError::tool(format!("search URL has no host: {current}")))?;
        crate::web_fetch::validate_host_ssrf(host).await?;

        let mut req = client.get(&current);
        for (name, value) in headers {
            req = req.header(*name, *value);
        }
        let resp = req.send().await.map_err(|e| {
            DeepseeknovaError::tool(format!("search request to '{current}' failed: {e}"))
        })?;
        let status = resp.status();
        if status.is_redirection() {
            let location = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    DeepseeknovaError::tool(format!(
                        "search redirect (HTTP {status}) missing Location header"
                    ))
                })?;
            current = parsed
                .join(location)
                .map_err(|e| {
                    DeepseeknovaError::tool(format!("invalid redirect location '{location}': {e}"))
                })?
                .to_string();
            continue;
        }
        let text = resp.text().await.map_err(|e| {
            DeepseeknovaError::tool(format!("failed to read search response body: {e}"))
        })?;
        return Ok((status, text));
    }
    Err(DeepseeknovaError::tool(format!(
        "search endpoint exceeded max redirects ({MAX_SEARCH_REDIRECTS})"
    )))
}

fn urlencode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}

fn parse_ddg(text: &str) -> Result<Vec<SearchHit>, DeepseeknovaError> {
    let v: serde_json::Value = serde_json::from_str(text)?;
    let mut hits = Vec::new();
    if let Some(abstract_text) = v["AbstractText"].as_str().filter(|s| !s.is_empty()) {
        hits.push(SearchHit {
            title: "Summary".to_string(),
            url: v["AbstractURL"].as_str().unwrap_or_default().to_string(),
            snippet: abstract_text.to_string(),
        });
    }
    for item in v["RelatedTopics"].as_array().into_iter().flatten() {
        collect_ddg_topic(item, &mut hits);
    }
    for item in v["Results"].as_array().into_iter().flatten() {
        if let Some(text) = item["Text"].as_str() {
            hits.push(SearchHit {
                title: item["FirstURL"]
                    .as_str()
                    .and_then(|u| url::Url::parse(u).ok())
                    .and_then(|u| u.host_str().map(|h| h.to_string()))
                    .unwrap_or_else(|| "Result".to_string()),
                url: item["FirstURL"].as_str().unwrap_or_default().to_string(),
                snippet: text.to_string(),
            });
        }
    }
    Ok(hits)
}

fn collect_ddg_topic(item: &serde_json::Value, hits: &mut Vec<SearchHit>) {
    if let Some(topics) = item["Topics"].as_array() {
        for sub in topics {
            collect_ddg_topic(sub, hits);
        }
    }
    if let Some(text) = item["Text"].as_str() {
        hits.push(SearchHit {
            title: item["FirstURL"]
                .as_str()
                .and_then(|u| url::Url::parse(u).ok())
                .and_then(|u| u.host_str().map(|h| h.to_string()))
                .unwrap_or_else(|| "Result".to_string()),
            url: item["FirstURL"].as_str().unwrap_or_default().to_string(),
            snippet: text.to_string(),
        });
    }
}

fn parse_tavily(text: &str) -> Result<Vec<SearchHit>, DeepseeknovaError> {
    let v: serde_json::Value = serde_json::from_str(text)?;
    let mut hits = Vec::new();
    for item in v["results"].as_array().into_iter().flatten() {
        hits.push(SearchHit {
            title: item["title"].as_str().unwrap_or_default().to_string(),
            url: item["url"].as_str().unwrap_or_default().to_string(),
            snippet: item["content"].as_str().unwrap_or_default().to_string(),
        });
    }
    Ok(hits)
}

fn parse_bing(text: &str) -> Result<Vec<SearchHit>, DeepseeknovaError> {
    let v: serde_json::Value = serde_json::from_str(text)?;
    let mut hits = Vec::new();
    for item in v["webPages"]["value"].as_array().into_iter().flatten() {
        hits.push(SearchHit {
            title: item["name"].as_str().unwrap_or_default().to_string(),
            url: item["url"].as_str().unwrap_or_default().to_string(),
            snippet: item["snippet"].as_str().unwrap_or_default().to_string(),
        });
    }
    Ok(hits)
}

fn parse_searxng(text: &str) -> Result<Vec<SearchHit>, DeepseeknovaError> {
    let v: serde_json::Value = serde_json::from_str(text)?;
    let mut hits = Vec::new();
    for item in v["results"].as_array().into_iter().flatten() {
        hits.push(SearchHit {
            title: item["title"].as_str().unwrap_or_default().to_string(),
            url: item["url"].as_str().unwrap_or_default().to_string(),
            snippet: item["content"].as_str().unwrap_or_default().to_string(),
        });
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ddg_parses_abstract_and_related_topics() {
        let text = r#"{
            "AbstractText": "Rust is a systems programming language.",
            "AbstractURL": "https://example.com/rust",
            "RelatedTopics": [
                {"Text": "Rust wiki", "FirstURL": "https://example.com/wiki"},
                {"Topics": [{"Text": "Nested", "FirstURL": "https://example.com/nested"}]}
            ],
            "Results": [{"Text": "A result", "FirstURL": "https://example.com/result"}]
        }"#;
        let hits = parse_ddg(text).unwrap();
        assert_eq!(hits.len(), 4);
        assert_eq!(hits[0].title, "Summary");
        assert!(hits.iter().any(|h| h.snippet.contains("Rust")));
    }

    #[test]
    fn tavily_parses_results() {
        let text = r#"{"results":[
            {"title":"T","url":"https://e.com/1","content":"snippet one"}
        ]}"#;
        let hits = parse_tavily(text).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "T");
        assert_eq!(hits[0].url, "https://e.com/1");
    }

    #[test]
    fn bing_parses_web_pages() {
        let text = r#"{"webPages":{"value":[
            {"name":"N","url":"https://e.com/2","snippet":"snippet two"}
        ]}}"#;
        let hits = parse_bing(text).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "N");
        assert_eq!(hits[0].snippet, "snippet two");
    }

    #[test]
    fn searxng_parses_results() {
        let text = r#"{"results":[
            {"title":"S","url":"https://e.com/3","content":"snippet three"}
        ]}"#;
        let hits = parse_searxng(text).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, "https://e.com/3");
    }

    #[test]
    fn truncate_keeps_short_and_marks_long() {
        assert_eq!(truncate("abc", 5), "abc");
        assert_eq!(truncate("abcdef", 4), "abcd…");
    }

    #[test]
    fn max_results_is_capped() {
        let cfg = WebSearchConfig::default();
        let max = 99usize.min(MAX_RESULTS_CAP);
        assert_eq!(max, MAX_RESULTS_CAP);
        assert_eq!(cfg.max_results, 5);
    }

    #[tokio::test]
    async fn unknown_provider_is_rejected_not_silently_fallback() {
        let cfg = WebSearchConfig {
            provider: "bogus".to_string(),
            ..Default::default()
        };
        let tool = WebSearchTool::new(cfg).unwrap();
        let ctx = ToolContext::new("t")
            .with_extension(deepseeknova_security::context::SecurityContext::with_safe_defaults());
        let err = tool
            .execute(&ctx, r#"{"query":"rust"}"#)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("unknown web_search provider 'bogus'"),
            "expected provider error, got: {err}"
        );
    }
}

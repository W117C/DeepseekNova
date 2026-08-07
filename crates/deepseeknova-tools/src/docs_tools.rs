//! Context7 文档检索工具：`context7_docs`。
//!
//! Context7 的公开 API 免费、无需 key：按「库名 + 主题」返回第三方库的最新文档片段，
//! 让 agent 不再依赖可能过时的训练数据。安全边界：端点固定 `context7.com`
//! （测试经 `#[cfg(test)]` 构造器注入本地地址），执行前强制 `NetworkAccess` 能力，
//! 所有错误转成对模型友好的提示文本，不向运行器抛 Err。

use async_trait::async_trait;
use deepseeknova_core::{Tool, ToolContext, ToolSchema};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

/// 生产固定端点；测试经 `with_base_url` 注入本地地址。
const DEFAULT_BASE_URL: &str = "https://context7.com";
const SEARCH_PATH: &str = "/api/v2/libs/search";
const CONTEXT_PATH: &str = "/api/v2/context";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_MAX_CHARS: usize = 6000;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

pub struct Context7DocsTool {
    base_url: String,
    /// 工具级一次构造的共享客户端（不自动跟随重定向）。
    client: reqwest::Client,
}

/// 进程级共享 HTTP 客户端构造（客户端不随请求变化，一次构建复用）。
fn build_shared_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(DEFAULT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(format!("deepseeknova-tools/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("failed to build shared HTTP client")
}

impl Context7DocsTool {
    pub fn new() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            client: build_shared_client(),
        }
    }

    /// 仅测试可见：注入本地测试服务器地址（127.0.0.1 / localhost）。
    #[cfg(test)]
    fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            client: build_shared_client(),
        }
    }
}

impl Default for Context7DocsTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct Context7DocsArgs {
    library: String,
    query: String,
    #[serde(default)]
    library_id: Option<String>,
    #[serde(default)]
    max_chars: Option<usize>,
}

/// 构造搜索库的 URL：`/api/v2/libs/search?libraryName=<名>&query=<主题>`。
fn search_url(base: &str, library: &str, query: &str) -> anyhow::Result<url::Url> {
    let mut u = url::Url::parse(base)?.join(SEARCH_PATH)?;
    u.query_pairs_mut()
        .append_pair("libraryName", library)
        .append_pair("query", query);
    Ok(u)
}

/// 构造拉文档片段的 URL：`/api/v2/context?libraryId=<id>&query=<主题>&type=txt`。
fn context_url(base: &str, library_id: &str, query: &str) -> anyhow::Result<url::Url> {
    let mut u = url::Url::parse(base)?.join(CONTEXT_PATH)?;
    u.query_pairs_mut()
        .append_pair("libraryId", library_id)
        .append_pair("query", query)
        .append_pair("type", "txt");
    Ok(u)
}

/// 解析 search 响应，取第一个结果的 (id, title)；无结果或解析失败返回 None。
fn first_result(body: &str) -> Option<(String, String)> {
    #[derive(Deserialize)]
    struct SearchResponse {
        results: Vec<SearchResult>,
    }
    #[derive(Deserialize)]
    struct SearchResult {
        id: String,
        #[serde(default)]
        title: String,
    }
    let parsed: SearchResponse = serde_json::from_str(body).ok()?;
    parsed.results.into_iter().next().map(|r| (r.id, r.title))
}

/// 按字符数截断，保证 UTF-8 边界安全，并附截断标记。
fn truncate_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out = text[..text.floor_char_boundary(max_chars)].to_string();
    out.push_str("\n…(truncated)");
    out
}

/// 域名固定：生产只允许 `context7.com`；测试构建额外允许本地地址。
fn validate_base_url(base: &str) -> anyhow::Result<()> {
    let u = url::Url::parse(base).map_err(|e| anyhow::anyhow!("invalid base URL '{base}': {e}"))?;
    let host = u
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("base URL has no host"))?;
    if host == "context7.com" {
        return Ok(());
    }
    #[cfg(test)]
    if host == "127.0.0.1" || host == "localhost" {
        return Ok(());
    }
    anyhow::bail!("blocked base URL host '{host}'; only context7.com is allowed")
}

fn render_docs(
    library: &str,
    title: &str,
    library_id: &str,
    query: &str,
    body: &str,
    max_chars: usize,
) -> String {
    format!(
        "context7_docs: {title} ({library_id})\nlibrary: {library}\nquery: {query}\n---\n{}",
        truncate_text(body.trim(), max_chars)
    )
}

fn render_not_found(library: &str) -> String {
    format!("no Context7 library found for '{library}'; check the library name and try again")
}

/// 拉取文本并做大小/超时/状态码校验；失败返回可直接展示的提示。
async fn fetch_text(client: &reqwest::Client, url: url::Url, what: &str) -> Result<String, String> {
    let response = tokio::time::timeout(DEFAULT_TIMEOUT, client.get(url).send())
        .await
        .map_err(|_| format!("Context7 {what} request timed out"))?
        .map_err(|e| format!("Context7 {what} request failed: {e}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("Context7 {what} failed with HTTP {status}"));
    }
    let bytes = tokio::time::timeout(DEFAULT_TIMEOUT, response.bytes())
        .await
        .map_err(|_| format!("Context7 {what} body read timed out"))?
        .map_err(|e| format!("Context7 {what} body read failed: {e}"))?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(format!(
            "Context7 {what} response exceeds {MAX_RESPONSE_BYTES} bytes"
        ));
    }
    String::from_utf8(bytes.to_vec())
        .map_err(|_| format!("Context7 {what} response is not valid UTF-8"))
}

#[async_trait]
impl Tool for Context7DocsTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "context7_docs".to_string(),
            description: "Fetches latest third-party library docs snippets from Context7."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "library": {
                        "type": "string",
                        "description": "Library name (e.g. serde)."
                    },
                    "query": {
                        "type": "string",
                        "description": "Topic or question (e.g. derive Serialize)."
                    },
                    "library_id": {
                        "type": "string",
                        "description": "Optional Context7 library id like /owner/repo; skips search."
                    },
                    "max_chars": {
                        "type": "integer",
                        "description": "Output cap (6000 default)."
                    }
                },
                "required": ["library", "query"]
            }),
        }
    }

    fn read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        deepseeknova_security::context::enforce_capability(
            ctx,
            &self.schema().name,
            deepseeknova_security::capability::Capability::NetworkAccess,
        )?;
        let parsed: Context7DocsArgs = serde_json::from_str(args)?;
        if ctx.cancellation.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        if let Err(e) = validate_base_url(&self.base_url) {
            return Ok(e.to_string());
        }

        let client = &self.client;

        // 1) 定库：显式 library_id 或先搜索取首个结果
        let (library_id, title) = match &parsed.library_id {
            Some(id) => (id.clone(), parsed.library.clone()),
            None => {
                let url = match search_url(&self.base_url, &parsed.library, &parsed.query) {
                    Ok(u) => u,
                    Err(e) => return Ok(format!("Context7 search URL error: {e}")),
                };
                let body = match fetch_text(client, url, "search").await {
                    Ok(b) => b,
                    Err(msg) => return Ok(msg),
                };
                match first_result(&body) {
                    Some((id, title)) => (id, title),
                    None => return Ok(render_not_found(&parsed.library)),
                }
            }
        };

        // 2) 拉文档片段并截断渲染
        let url = match context_url(&self.base_url, &library_id, &parsed.query) {
            Ok(u) => u,
            Err(e) => return Ok(format!("Context7 docs URL error: {e}")),
        };
        let body = match fetch_text(client, url, "docs").await {
            Ok(b) => b,
            Err(msg) => return Ok(msg),
        };
        let max_chars = parsed
            .max_chars
            .unwrap_or(DEFAULT_MAX_CHARS)
            .min(DEFAULT_MAX_CHARS);
        Ok(render_docs(
            &parsed.library,
            &title,
            &library_id,
            &parsed.query,
            &body,
            max_chars,
        ))
    }
}

/// runtime 常驻注册的文档检索工具（不进 all_builtin，避免触碰 schema 预算测试）。
pub fn docs_tools() -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(Context7DocsTool::new())]
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_core::tool::{Tool, ToolContext};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn ctx_with_network() -> ToolContext {
        ToolContext::new("c7")
            .with_extension(deepseeknova_security::context::SecurityContext::with_safe_defaults())
    }

    #[test]
    fn builds_search_url_with_encoded_params() {
        let u = search_url("https://context7.com", "serde", "derive Serialize").unwrap();
        assert_eq!(u.path(), "/api/v2/libs/search");
        let pairs: Vec<(String, String)> = u
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert!(pairs.contains(&("libraryName".to_string(), "serde".to_string())));
        assert!(pairs.contains(&("query".to_string(), "derive Serialize".to_string())));
    }

    #[test]
    fn builds_context_url_with_encoded_library_id() {
        let u = context_url("https://context7.com", "/serde-rs/serde", "derive").unwrap();
        assert_eq!(u.path(), "/api/v2/context");
        let raw = u.query().unwrap();
        assert!(
            raw.contains("libraryId=%2Fserde-rs%2Fserde"),
            "library id 必须百分号编码：{raw}"
        );
        assert!(raw.contains("type=txt"));
        let pairs: Vec<(String, String)> = u
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert!(pairs.contains(&("libraryId".to_string(), "/serde-rs/serde".to_string())));
    }

    #[test]
    fn parses_search_json_and_takes_first_id() {
        let body = r#"{"results":[{"id":"/serde-rs/serde","title":"Serde"},{"id":"/other/x","title":"X"}]}"#;
        let (id, title) = first_result(body).unwrap();
        assert_eq!(id, "/serde-rs/serde");
        assert_eq!(title, "Serde");
        assert!(
            first_result(r#"{"results":[]}"#).is_none(),
            "空结果应判未找到"
        );
        assert!(first_result("not json").is_none(), "坏 JSON 应判未找到");
    }

    #[test]
    fn truncates_text_at_max_chars_utf8_safe() {
        // ASCII：恰好截到 50 字符
        let out = truncate_text(&"a".repeat(100), 50);
        assert!(out.ends_with("…(truncated)"));
        assert_eq!(out.chars().count(), 50 + "…(truncated)".chars().count() + 1);
        // 多字节：截断点落在字符中间时按 UTF-8 边界下取整，不许 panic、不许切坏字符
        let wide = "é".repeat(100);
        let out2 = truncate_text(&wide, 50);
        assert!(out2.ends_with("…(truncated)"));
        assert!(
            out2.chars().count() < 100,
            "应截短：{}",
            out2.chars().count()
        );
        // 未超限时不截断
        assert_eq!(truncate_text("short", 6000), "short");
        // 空/零上限不 panic
        assert!(truncate_text(&wide, 0).ends_with("…(truncated)"));
    }

    #[test]
    fn rejects_non_context7_base_url() {
        assert!(validate_base_url("https://context7.com").is_ok());
        assert!(
            validate_base_url("http://127.0.0.1:8123").is_ok(),
            "测试构建允许本地地址"
        );
        let err = validate_base_url("https://evil.com").unwrap_err();
        assert!(err.to_string().contains("only context7.com"), "{err}");
        assert!(
            validate_base_url("https://context7.com.evil.com").is_err(),
            "子域混淆必须拒绝"
        );
    }

    #[test]
    fn not_found_renders_friendly_hint() {
        let msg = render_not_found("serde");
        assert!(
            msg.contains("no Context7 library found for 'serde'"),
            "{msg}"
        );
    }

    /// 本地 TcpListener 测试服务器：依次对每个连接返回固定响应体。
    async fn serve(listener: tokio::net::TcpListener, responses: Vec<&'static str>) {
        for body in responses {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            // 读到请求头结束即可，无需消费 body
            loop {
                let n = sock.read(&mut buf).await.unwrap();
                if n == 0 || String::from_utf8_lossy(&buf[..n]).contains("\r\n\r\n") {
                    break;
                }
            }
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
            let _ = sock.shutdown().await;
        }
    }

    #[tokio::test]
    async fn execute_fetches_docs_from_local_server() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(serve(
            listener,
            vec![
                r#"{"results":[{"id":"/serde-rs/serde","title":"Serde"}]}"#,
                "### Rust Struct\n\nuse serde::Serialize;\n#[derive(Serialize)]\nstruct Point { x: i32 }\n",
            ],
        ));

        let tool = Context7DocsTool::with_base_url(format!("http://{addr}"));
        let ctx = ctx_with_network();
        let out = tool
            .execute(&ctx, r#"{"library":"serde","query":"derive"}"#)
            .await
            .unwrap();
        server.await.unwrap();

        assert!(
            out.contains("/serde-rs/serde"),
            "输出应标注所选库 id：{out}"
        );
        assert!(out.contains("Serde"), "输出应标注库 title：{out}");
        assert!(
            out.contains("use serde::Serialize;"),
            "输出应含文档片段：{out}"
        );
    }

    #[tokio::test]
    async fn execute_reports_missing_library_via_http() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(serve(listener, vec![r#"{"results":[]}"#]));

        let tool = Context7DocsTool::with_base_url(format!("http://{addr}"));
        let ctx = ctx_with_network();
        let out = tool
            .execute(&ctx, r#"{"library":"no_such_lib_xyz","query":"anything"}"#)
            .await
            .unwrap();
        server.await.unwrap();

        assert!(
            out.contains("no Context7 library found"),
            "空结果应转友好提示：{out}"
        );
    }

    #[tokio::test]
    async fn execute_maps_http_error_to_friendly_hint() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await.unwrap();
            let resp = "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            sock.write_all(resp.as_bytes()).await.unwrap();
            let _ = sock.shutdown().await;
        });

        let tool = Context7DocsTool::with_base_url(format!("http://{addr}"));
        let ctx = ctx_with_network();
        let out = tool
            .execute(&ctx, r#"{"library":"serde","query":"derive"}"#)
            .await
            .unwrap();
        server.await.unwrap();

        assert!(out.contains("HTTP 500"), "非 200 应转友好提示：{out}");
    }
}

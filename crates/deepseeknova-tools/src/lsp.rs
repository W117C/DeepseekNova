//! LSP 编辑后诊断工具 — a minimal Language Server Protocol client.
//!
//! Spawns a configured language server over stdio, opens the target file,
//! waits for `textDocument/publishDiagnostics`, then formats the findings for
//! the model. Kept intentionally small: no incremental sync, no inlay hints,
//! no code actions — just "what is wrong with this file right now".
//!
//! Built-in server mapping:
//! - rust → `rust-analyzer`
//! - python → `pyright-langserver --stdio`
//! - go → `gopls serve`
//! - typescript → `typescript-language-server --stdio`
//! - c / cpp → `clangd`

use async_trait::async_trait;
use deepseeknova_config::LspConfig;
use deepseeknova_core::{Tool, ToolContext, ToolSchema};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::time::timeout;

const EMPTY_GRACE: Duration = Duration::from_millis(1500);

/// 收到空诊断后是否已过宽限期（等迟到的非空更新）。
fn empty_grace_elapsed(empty_since: Option<Instant>, now: Instant) -> bool {
    empty_since.is_some_and(|s| now.duration_since(s) >= EMPTY_GRACE)
}

/// Build the optional LSP diagnostics tool set for agent registration.
pub fn lsp_diagnostics_tools(cfg: &deepseeknova_config::ToolsConfig) -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(LspDiagnosticsTool::new(cfg.lsp.clone()))]
}

pub struct LspDiagnosticsTool {
    cfg: LspConfig,
}

impl LspDiagnosticsTool {
    pub fn new(cfg: LspConfig) -> Self {
        Self { cfg }
    }
}

#[derive(Deserialize)]
struct LspArgs {
    path: String,
    #[serde(default)]
    language: Option<String>,
}

#[async_trait]
impl Tool for LspDiagnosticsTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "lsp_diagnostics".to_string(),
            description: "Runs the language server on a file and returns current \
                         diagnostics (errors/warnings with line numbers). Use after editing \
                         code to catch compile/type errors."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path, relative to the workspace root."
                    },
                    "language": {
                        "type": "string",
                        "description": "Optional language override: rust|python|go|typescript|c|cpp."
                    }
                },
                "required": ["path"]
            }),
        }
    }

    fn read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        let parsed: LspArgs = serde_json::from_str(args)?;
        if ctx.cancellation.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        deepseeknova_security::context::enforce_capability(
            ctx,
            deepseeknova_security::capability::Capability::FileRead,
        )?;
        if !self.cfg.enabled {
            return Ok("lsp_diagnostics is disabled by [tools.lsp] enabled=false".to_string());
        }

        let path = resolve_path(&ctx.workspace_root, &parsed.path)?;
        if !path.is_file() {
            anyhow::bail!("lsp_diagnostics: path is not a file: {}", path.display());
        }
        let language = match parsed.language.as_deref() {
            Some(l) => l.to_string(),
            None => language_for(&path)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "lsp_diagnostics: unsupported file extension for {}; \
                         pass language=rust|python|go|typescript|c|cpp",
                        path.display()
                    )
                })?
                .to_string(),
        };
        let (server_bin, server_args) = server_for(&language, &self.cfg.servers);

        let text = std::fs::read_to_string(&path)?;
        if text.len() > self.cfg.max_file_bytes {
            return Ok(format!(
                "lsp_diagnostics: file {} exceeds {} bytes; skipped",
                path.display(),
                self.cfg.max_file_bytes
            ));
        }

        let file_uri = uri_from_path(&path)?;
        let root_uri = uri_from_path(&ctx.workspace_root)?;

        let mut session = match LspSession::spawn(&server_bin, &server_args) {
            Ok(s) => s,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return Ok(format!(
                    "lsp_diagnostics: language server '{server_bin}' not found. \
                     Install it (or set [tools.lsp.servers] {language} = \"<binary>\") \
                     to enable diagnostics."
                ));
            }
            Err(e) => anyhow::bail!("lsp_diagnostics: failed to spawn '{server_bin}': {e}"),
        };

        let result = session
            .collect_diagnostics(
                &file_uri,
                &root_uri,
                &language,
                &text,
                Duration::from_secs(self.cfg.timeout_secs.max(1)),
            )
            .await;
        session.shutdown().await;
        let diagnostics = result?;

        Ok(format_diagnostics(&path, &diagnostics))
    }
}

fn resolve_path(workspace_root: &Path, raw: &str) -> anyhow::Result<PathBuf> {
    let p = PathBuf::from(raw);
    let p = if p.is_absolute() {
        p
    } else {
        workspace_root.join(p)
    };
    // Lexical normalization (no filesystem canonicalization so the file does
    // not need to exist for URI construction in tests).
    Ok(p)
}

fn uri_from_path(path: &Path) -> anyhow::Result<String> {
    let url = url::Url::from_file_path(path)
        .map_err(|_| anyhow::anyhow!("cannot build file URI for {}", path.display()))?;
    Ok(url.to_string())
}

fn language_for(path: &Path) -> Option<&'static str> {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "rs" => Some("rust"),
        "py" => Some("python"),
        "go" => Some("go"),
        "ts" | "tsx" | "js" | "jsx" | "mts" | "cts" => Some("typescript"),
        "c" | "h" => Some("c"),
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => Some("cpp"),
        _ => None,
    }
}

fn server_for(language: &str, overrides: &HashMap<String, String>) -> (String, Vec<String>) {
    let bin = overrides
        .get(language)
        .cloned()
        .unwrap_or_else(|| match language {
            "rust" => "rust-analyzer".to_string(),
            "python" => "pyright-langserver".to_string(),
            "go" => "gopls".to_string(),
            "typescript" => "typescript-language-server".to_string(),
            "c" | "cpp" => "clangd".to_string(),
            other => format!("lsp-server-{other}"),
        });
    let args = match language {
        "python" => vec!["--stdio".to_string()],
        "go" => vec!["serve".to_string()],
        "typescript" => vec!["--stdio".to_string()],
        _ => Vec::new(),
    };
    (bin, args)
}

// ---------------------------------------------------------------------------
// Minimal LSP client
// ---------------------------------------------------------------------------

struct LspSession {
    child: Option<Child>,
    stdin: Box<dyn tokio::io::AsyncWrite + Unpin + Send>,
    stdout: BufReader<Box<dyn tokio::io::AsyncRead + Unpin + Send>>,
}

impl LspSession {
    fn spawn(bin: &str, args: &[String]) -> std::io::Result<Self> {
        let mut cmd = tokio::process::Command::new(bin);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = cmd.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::new(ErrorKind::BrokenPipe, "missing stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::new(ErrorKind::BrokenPipe, "missing stdout"))?;
        Ok(Self {
            child: Some(child),
            stdin: Box::new(stdin),
            stdout: BufReader::new(Box::new(stdout)),
        })
    }

    async fn send(&mut self, msg: &Value) -> anyhow::Result<()> {
        let body = serde_json::to_string(msg)?;
        let frame = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        self.stdin.write_all(frame.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn next_message(&mut self) -> anyhow::Result<Option<Value>> {
        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            let n = self.stdout.read_line(&mut line).await?;
            if n == 0 {
                return Ok(None);
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
                content_length = rest.trim().parse::<usize>().ok();
            }
        }
        let len = content_length
            .ok_or_else(|| anyhow::anyhow!("LSP message missing Content-Length header"))?;
        let mut body = vec![0u8; len];
        self.stdout.read_exact(&mut body).await?;
        let value = serde_json::from_slice(&body)?;
        Ok(Some(value))
    }

    /// Read messages until the target file's diagnostics arrive (or timeout).
    /// Server-initiated requests are answered with `null` so the server does
    /// not stall waiting for configuration etc.
    async fn collect_diagnostics(
        &mut self,
        file_uri: &str,
        root_uri: &str,
        language: &str,
        text: &str,
        total: Duration,
    ) -> anyhow::Result<Vec<LspDiagnostic>> {
        let init = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "processId": null,
                "clientInfo": {"name": "deepseeknova", "version": env!("CARGO_PKG_VERSION")},
                "rootUri": root_uri,
                "capabilities": {},
                "workspaceFolders": [{"uri": root_uri, "name": "workspace"}]
            }
        });
        self.send(&init).await?;
        let init_result = self
            .respond_until_id(1, total)
            .await?
            .ok_or_else(|| anyhow::anyhow!("LSP server exited before initialize response"))?;
        if init_result.get("error").is_some() {
            anyhow::bail!("LSP initialize failed: {}", init_result["error"]);
        }

        self.send(&json!({"jsonrpc":"2.0","method":"initialized","params":{}}))
            .await?;
        self.send(&json!({
            "jsonrpc":"2.0",
            "method":"textDocument/didOpen",
            "params":{
                "textDocument":{
                    "uri": file_uri,
                    "languageId": language,
                    "version": 1,
                    "text": text
                }
            }
        }))
        .await?;

        let deadline = Instant::now() + total;
        let mut empty_since: Option<Instant> = None;
        loop {
            // 收到空诊断后最多再等 EMPTY_GRACE，等待迟到的非空更新；
            // 检查放在等待下一条消息之前，避免“空诊断后服务端安静”时
            // 一直等到整体 deadline。
            if empty_grace_elapsed(empty_since, Instant::now()) {
                break;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            // 收到空诊断后，把下一次读取的等待上限压到 EMPTY_GRACE 剩余量，
            // 否则“空诊断后服务端安静”会一直阻塞到整体 deadline，而不是
            // 快速返回空结果。
            let wait = if let Some(since) = empty_since {
                let grace_left = EMPTY_GRACE.saturating_sub(since.elapsed());
                remaining.min(grace_left)
            } else {
                remaining
            };
            let msg = match timeout(wait, self.next_message()).await {
                Ok(Ok(Some(m))) => m,
                Ok(Ok(None)) => break,
                Ok(Err(e)) => return Err(e),
                Err(_) => break, // overall deadline reached
            };

            if let Some(id) = msg.get("id").and_then(|v| v.as_i64()) {
                // Server request we must answer to keep it moving.
                self.send(&json!({"jsonrpc":"2.0","id":id,"result":null}))
                    .await?;
                continue;
            }
            if msg["method"] == "textDocument/publishDiagnostics" {
                let uri = msg["params"]["uri"].as_str().unwrap_or_default();
                if uri == file_uri {
                    let diags = parse_diagnostics(&msg["params"]);
                    if diags.is_empty() {
                        empty_since = Some(Instant::now());
                    } else {
                        return Ok(diags);
                    }
                }
                continue;
            }
            // Ignore log messages, progress, etc.
        }

        Ok(Vec::new())
    }

    /// Send a request, answer server requests along the way, and return the
    /// response with the given id (or `None` if the server exits).
    async fn respond_until_id(
        &mut self,
        target_id: i64,
        total: Duration,
    ) -> anyhow::Result<Option<Value>> {
        let deadline = Instant::now() + total;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                anyhow::bail!("LSP initialize timed out after {total:?}");
            }
            let msg = match timeout(remaining, self.next_message()).await {
                Ok(Ok(Some(m))) => m,
                Ok(Ok(None)) => return Ok(None),
                Ok(Err(e)) => return Err(e),
                Err(_) => anyhow::bail!("LSP initialize timed out after {total:?}"),
            };
            if msg.get("id").and_then(|v| v.as_i64()) == Some(target_id) {
                return Ok(Some(msg));
            }
            if let Some(id) = msg.get("id").and_then(|v| v.as_i64()) {
                self.send(&json!({"jsonrpc":"2.0","id":id,"result":null}))
                    .await?;
            }
        }
    }

    async fn shutdown(&mut self) {
        let _ = self
            .send(&json!({"jsonrpc":"2.0","id":2,"method":"shutdown","params":null}))
            .await;
        // Give the server a moment to answer, then exit regardless.
        let _ = timeout(Duration::from_millis(500), self.next_message()).await;
        let _ = self
            .send(&json!({"jsonrpc":"2.0","method":"exit","params":null}))
            .await;
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LspDiagnostic {
    severity: u8,
    message: String,
    line: u32,
}

fn parse_diagnostics(params: &Value) -> Vec<LspDiagnostic> {
    params["diagnostics"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|d| LspDiagnostic {
            severity: d["severity"].as_u64().unwrap_or(3) as u8,
            message: d["message"].as_str().unwrap_or_default().to_string(),
            line: d["range"]["start"]["line"].as_u64().unwrap_or(0) as u32 + 1,
        })
        .filter(|d| !d.message.is_empty())
        .collect()
}

fn severity_label(severity: u8) -> &'static str {
    match severity {
        1 => "error",
        2 => "warning",
        3 => "information",
        _ => "hint",
    }
}

fn format_diagnostics(path: &Path, diagnostics: &[LspDiagnostic]) -> String {
    if diagnostics.is_empty() {
        return format!("No LSP diagnostics for {}.", path.display());
    }
    let mut out = format!("LSP diagnostics for {}:\n", path.display());
    for d in diagnostics {
        out.push_str(&format!(
            "- {}: {} (line {})\n",
            severity_label(d.severity),
            d.message,
            d.line
        ));
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn read_lsp_frame<R: tokio::io::AsyncBufRead + Unpin>(
        reader: &mut R,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).await? == 0 {
                return Ok(None);
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
                content_length = rest.trim().parse::<usize>().ok();
            }
        }
        let len = content_length.ok_or_else(|| anyhow::anyhow!("missing Content-Length"))?;
        let mut body = vec![0u8; len];
        reader.read_exact(&mut body).await?;
        Ok(Some(serde_json::from_slice(&body)?))
    }

    async fn write_lsp_frame<W: tokio::io::AsyncWrite + Unpin>(
        writer: &mut W,
        value: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let body = serde_json::to_string(value)?;
        writer
            .write_all(format!("Content-Length: {}\r\n\r\n{}", body.len(), body).as_bytes())
            .await?;
        writer.flush().await?;
        Ok(())
    }

    #[test]
    fn detects_languages_by_extension() {
        assert_eq!(language_for(Path::new("src/main.rs")), Some("rust"));
        assert_eq!(language_for(Path::new("app.py")), Some("python"));
        assert_eq!(language_for(Path::new("main.go")), Some("go"));
        assert_eq!(language_for(Path::new("x.tsx")), Some("typescript"));
        assert_eq!(language_for(Path::new("x.cpp")), Some("cpp"));
        assert_eq!(language_for(Path::new("README.md")), None);
    }

    #[test]
    fn server_override_wins() {
        let mut map = HashMap::new();
        map.insert("rust".to_string(), "/opt/ra".to_string());
        let (bin, _) = server_for("rust", &map);
        assert_eq!(bin, "/opt/ra");
        let (bin, args) = server_for("python", &HashMap::new());
        assert_eq!(bin, "pyright-langserver");
        assert_eq!(args, vec!["--stdio"]);
    }

    #[test]
    fn parses_diagnostics_with_one_based_lines() {
        let params = json!({
            "uri": "file:///tmp/a.rs",
            "diagnostics": [
                {"severity": 1, "message": "expected `;`", "range": {"start": {"line": 4}}},
                {"severity": 2, "message": "unused variable", "range": {"start": {"line": 0}}},
                {"severity": 4, "message": "", "range": {"start": {"line": 9}}}
            ]
        });
        let diags = parse_diagnostics(&params);
        assert_eq!(diags.len(), 2);
        assert_eq!(diags[0].severity, 1);
        assert_eq!(diags[0].line, 5);
        assert_eq!(diags[1].line, 1);
    }

    #[test]
    fn formats_diagnostics_human_readably() {
        let diags = vec![
            LspDiagnostic {
                severity: 1,
                message: "expected `;`".to_string(),
                line: 5,
            },
            LspDiagnostic {
                severity: 2,
                message: "unused variable".to_string(),
                line: 1,
            },
        ];
        let out = format_diagnostics(Path::new("src/main.rs"), &diags);
        assert!(out.contains("error: expected `;` (line 5)"));
        assert!(out.contains("warning: unused variable (line 1)"));
        let empty = format_diagnostics(Path::new("src/main.rs"), &[]);
        assert!(empty.contains("No LSP diagnostics"));
    }

    #[test]
    fn frame_encoding_uses_byte_length() {
        let body = "{\"a\":\"你好\"}";
        let frame = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        assert!(frame.starts_with("Content-Length: "));
        assert!(frame.ends_with(body));
    }

    #[test]
    fn empty_grace_elapsed_after_window() {
        let now = Instant::now();
        assert!(!empty_grace_elapsed(None, now));
        assert!(!empty_grace_elapsed(Some(now), now));
        assert!(empty_grace_elapsed(Some(now - EMPTY_GRACE), now));
    }

    #[tokio::test]
    async fn lsp_empty_diagnostics_return_quickly_not_full_timeout() {
        let (client_write, server_read) = tokio::io::duplex(64 * 1024);
        let (server_write, client_read) = tokio::io::duplex(64 * 1024);
        let mut session = LspSession {
            child: None,
            stdin: Box::new(client_write),
            stdout: BufReader::new(Box::new(client_read)),
        };
        let server = tokio::spawn(async move {
            let mut reader = BufReader::new(server_read);
            let mut writer = server_write;
            loop {
                let Some(msg) = read_lsp_frame(&mut reader).await.unwrap() else {
                    break;
                };
                if let Some(id) = msg.get("id").and_then(|v| v.as_i64()) {
                    if msg["method"] == "initialize" {
                        write_lsp_frame(
                            &mut writer,
                            &json!({"jsonrpc":"2.0","id":id,"result":{"capabilities":{}}}),
                        )
                        .await
                        .unwrap();
                    } else if msg["method"] == "shutdown" {
                        write_lsp_frame(
                            &mut writer,
                            &json!({"jsonrpc":"2.0","id":id,"result":null}),
                        )
                        .await
                        .unwrap();
                        break;
                    }
                    continue;
                }
                if msg["method"] == "textDocument/didOpen" {
                    let file_uri = msg["params"]["textDocument"]["uri"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    write_lsp_frame(
                        &mut writer,
                        &json!({
                            "jsonrpc": "2.0",
                            "method": "textDocument/publishDiagnostics",
                            "params": {"uri": file_uri, "diagnostics": []}
                        }),
                    )
                    .await
                    .unwrap();
                }
            }
        });

        let started = Instant::now();
        let diags = session
            .collect_diagnostics(
                "file:///tmp/a.rs",
                "file:///tmp",
                "rust",
                "fn main() {}",
                Duration::from_secs(8),
            )
            .await
            .unwrap();
        let elapsed = started.elapsed();
        session.shutdown().await;
        server.await.unwrap();

        assert!(diags.is_empty(), "fake server sent no diagnostics");
        assert!(
            elapsed < Duration::from_secs(5),
            "empty diagnostics must return quickly, took {elapsed:?}"
        );
    }

    #[test]
    fn uri_from_workspace_path_is_file_scheme() {
        let uri = uri_from_path(Path::new("/tmp/ws/src/main.rs")).unwrap();
        assert!(uri.starts_with("file:///"));
        assert!(uri.ends_with("src/main.rs"));
    }
}

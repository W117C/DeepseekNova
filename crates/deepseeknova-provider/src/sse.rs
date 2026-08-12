//! — 共享 SSE（Server-Sent Events）行切分。
//!
//! OpenAI 兼容端点与 Anthropic 端点都以 `text/event-stream` 形式流式返回
//! token；本模块统一负责字节级切行（`\n` 分隔、`\r` 跳过、超长行报错、
//! 尾行冲刷），把"整行语义"（`event:`/`data:` 前缀、空行分发、JSON 解析）
//! 留给各 provider 的闭包自行处理。

use deepseeknova_core::DeepseeknovaError;
use futures::{Stream, StreamExt};
use std::future::Future;

/// 单条 SSE 行的最大字节数（含 `data:`/`event:` 前缀，tool arguments 累积
/// 同样受此上限约束）。恶意/损坏网关可在超时窗口内塞入无换行大块数据；超限
/// 即视为协议异常并返回明确错误，防止内存无限累积。
pub(crate) const MAX_SSE_LINE_BYTES: usize = 1024 * 1024;

/// 逐字节切分 SSE 字节流并逐行回调，返回闭包维护的 `state`。
///
/// 每个完整行（以 `\n` 结尾）先做 UTF-8 解码与首尾空白 trim，再以 `String`
/// 传给 `on_line`（**空行同样回调**——Anthropic 以空行作为事件分发边界，
/// OpenAI 侧由闭包自行跳过空行）。`\r` 字节直接跳过（兼容 `\r\n` 行尾）。
/// 单行累积超过 [`MAX_SSE_LINE_BYTES`] 视为协议异常报错；流结束后未换行的
/// 尾行（非空）同样冲刷给闭包。
///
/// 闭包契约：`on_line(line, state)` 按值接收当行与累计状态，返回一个
/// 拥有（而非借用）这些数据的 future，处理完再把 `state` 原样归还。
/// 累计状态（tool 调用、事件缓冲、usage 等）因此必须放进 `St` 由调用方
/// 传入，不能作为闭包环境捕获——否则返回的 future 会借走闭包捕获的
/// `&mut` 状态，违反 `FnMut(String, St) -> Fut` 的逃逸规则。
///
/// `context` 用于错误消息中的 provider 名（"OpenAI"/"Anthropic"），便于
/// 区分协议异常来自哪个端点。
///
/// # Errors
///
/// 读流失败、非法 UTF-8、超长行或闭包返回的错误都会以
/// [`DeepseeknovaError`] 形式返回。
pub(crate) async fn for_each_sse_line<S, B, St, F, Fut>(
    byte_stream: S,
    context: &str,
    mut state: St,
    mut on_line: F,
) -> Result<St, DeepseeknovaError>
where
    S: Stream<Item = Result<B, reqwest::Error>> + Unpin,
    B: AsRef<[u8]>,
    F: FnMut(String, St) -> Fut,
    Fut: Future<Output = Result<St, DeepseeknovaError>>,
{
    let mut byte_stream = byte_stream;
    // 逐字节累积当前行，避免 UTF-8 字符跨 TCP 分片被截断。
    let mut line_bytes: Vec<u8> = Vec::new();

    while let Some(chunk_result) = byte_stream.next().await {
        let bytes = chunk_result.map_err(|e| {
            DeepseeknovaError::provider(format!("failed to read chunk from {context} stream: {e}"))
        })?;
        for &b in bytes.as_ref().iter() {
            match b {
                b'\n' => {
                    let line_str =
                        String::from_utf8(std::mem::take(&mut line_bytes)).map_err(|e| {
                            DeepseeknovaError::provider(format!(
                                "invalid UTF-8 in {context} SSE stream: {e}"
                            ))
                        })?;
                    state = on_line(line_str.trim().to_string(), state).await?;
                }
                b'\r' => { /* 跳过——`\n` 已处理 */ }
                _ => {
                    // 单行无长度上限会被恶意/损坏网关的无换行大块数据撑爆内存；
                    // 超限视为协议异常直接报错（未换行的尾行同样受此约束）。
                    if line_bytes.len() >= MAX_SSE_LINE_BYTES {
                        return Err(DeepseeknovaError::provider(format!(
                            "{context} SSE line exceeds maximum allowed length (protocol anomaly)"
                        )));
                    }
                    line_bytes.push(b);
                }
            }
        }
    }

    // 冲刷未换行的尾行（若确有余量）。
    if !line_bytes.is_empty() {
        let tail_str = String::from_utf8(std::mem::take(&mut line_bytes)).map_err(|e| {
            DeepseeknovaError::provider(format!("invalid UTF-8 in {context} SSE stream tail: {e}"))
        })?;
        let trimmed = tail_str.trim().to_string();
        if !trimmed.is_empty() {
            state = on_line(trimmed, state).await?;
        }
    }

    Ok(state)
}

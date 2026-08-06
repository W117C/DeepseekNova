//! 权限审批通道：TUI 侧 `ApprovalResponder` 实现 + 待审批请求结构。
//!
//! 接线（Claude Code Confirmation context 的轻量版）：
//! - CLI 构建 agent 时创建通道，`TuiApprovalResponder` 注入 agent（Ask
//!   决策经 `ApprovalResponder::request` 回调）；
//! - 接收端注入 `TuiRunner` → `TuiCaps` → `AppState.pending_approval`，
//!   事件循环轮询，确认浮层渲染在对话区上方；
//! - 用户 `y` 允许 / `n` 拒绝，经 oneshot 回给阻塞中的 agent。
//!
//! 与 serve 的 `ServerApprovalResponder` 同构，但面向 TUI 交互。

use tokio::sync::{mpsc, oneshot};

/// 一条待审批请求（发送侧 → UI 侧）。
#[derive(Debug)]
pub struct ApprovalRequest {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub reply: oneshot::Sender<bool>,
}

/// 注入 agent 的 responder：把请求转发到 TUI 事件循环并等待用户裁决。
#[derive(Clone)]
pub struct TuiApprovalResponder {
    tx: mpsc::Sender<ApprovalRequest>,
}

impl TuiApprovalResponder {
    pub fn new(tx: mpsc::Sender<ApprovalRequest>) -> Self {
        Self { tx }
    }
}

#[async_trait::async_trait]
impl deepseeknova_core::runner::ApprovalResponder for TuiApprovalResponder {
    async fn request(&self, id: &str, title: &str, description: Option<&str>) -> bool {
        let (reply, rx) = oneshot::channel();
        let req = ApprovalRequest {
            id: id.to_string(),
            title: title.to_string(),
            description: description.map(|d| d.to_string()),
            reply,
        };
        // 通道关闭（TUI 已退出）→ 拒绝（fail-closed）。
        if self.tx.send(req).await.is_err() {
            return false;
        }
        // 等待 UI 裁决；UI 异常退出也视为拒绝。
        rx.await.unwrap_or(false)
    }
}

/// 创建审批通道：返回（注入 agent 的 responder, TUI 消费的接收端）。
pub fn approval_channel() -> (TuiApprovalResponder, mpsc::Receiver<ApprovalRequest>) {
    let (tx, rx) = mpsc::channel(8);
    (TuiApprovalResponder::new(tx), rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_core::runner::ApprovalResponder;

    #[test]
    fn responder_forwards_and_gets_verdict() {
        let (responder, mut rx) = approval_channel();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let handle = tokio::spawn(async move {
                let req = rx.recv().await.unwrap();
                assert_eq!(req.title, "run shell");
                assert_eq!(req.description.as_deref(), Some("rm -rf /"));
                req.reply.send(true).unwrap();
            });
            let allowed = responder.request("a1", "run shell", Some("rm -rf /")).await;
            assert!(allowed, "UI 裁决 true → 允许");
            handle.await.unwrap();
        });
    }

    #[test]
    fn responder_denies_when_channel_closed() {
        // fail-closed：TUI 已退出时拒绝。
        let (tx, rx) = mpsc::channel(8);
        // 显式 drop receiver：`_rx` 绑定会存活到函数结束，send 不会报错。
        drop(rx);
        let responder = TuiApprovalResponder::new(tx);
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let allowed = responder.request("a1", "x", None).await;
            assert!(!allowed);
        });
    }
}

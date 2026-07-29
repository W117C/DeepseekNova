//! 集成：并发委派在信号量满时排队（不失败、不死锁）。

use deepseeknova_agent::test_utils::MockProvider;
use deepseeknova_agent::{Agent, DelegateEngine};
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_delegates_queue_and_complete() {
    let mut agents: HashMap<String, Arc<Agent>> = HashMap::new();
    agents.insert(
        "explorer".into(),
        Arc::new(Agent::new(Arc::new(MockProvider::text("done-a")), 3).with_system_prompt("x")),
    );
    agents.insert(
        "coder".into(),
        Arc::new(Agent::new(Arc::new(MockProvider::text("done-b")), 3).with_system_prompt("x")),
    );
    // 并发上限 1 → 第二个委派必须排队等待，二者都应成功完成。
    let engine = Arc::new(DelegateEngine::new(agents, 1, 2000));
    let (e1, e2) = (engine.clone(), engine.clone());
    let (r1, r2) = tokio::join!(
        tokio::spawn(async move { e1.run("explorer", "g1").await }),
        tokio::spawn(async move { e2.run("coder", "g2").await }),
    );
    assert!(r1.unwrap().is_ok(), "first delegate should complete");
    assert!(r2.unwrap().is_ok(), "queued delegate should complete");
}

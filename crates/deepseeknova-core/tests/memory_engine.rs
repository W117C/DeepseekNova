//! 集成：记忆跨重启持久 + 进程内并发写安全。

use deepseeknova_core::memory::engine::MemoryEngine;
use deepseeknova_core::memory::store::MemoryCategory;
use std::sync::Arc;

fn temp_db() -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("dnv-mem-it-{}-{}.db", std::process::id(), nanos))
}

#[test]
fn memory_persists_across_reopen() {
    let path = temp_db();
    {
        let eng = MemoryEngine::open(&path, true).unwrap();
        eng.remember(
            "fact-1",
            "the build uses cargo make check",
            vec!["build".into()],
        )
        .unwrap();
    } // drop → 连接关闭
    {
        let eng = MemoryEngine::open(&path, true).unwrap();
        let hits = eng.recall("cargo make check", 5).unwrap();
        assert!(
            hits.iter().any(|h| h.entry.id == "fact-1"),
            "memory must survive reopen (this is the core bug fix)"
        );
    }
    let _ = std::fs::remove_file(&path);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_writes_do_not_deadlock_or_lose() {
    let path = temp_db();
    let eng = Arc::new(MemoryEngine::open(&path, true).unwrap());
    let mut handles = Vec::new();
    for i in 0..20 {
        let e = eng.clone();
        handles.push(tokio::spawn(async move {
            e.remember(&format!("k{i}"), &format!("value number {i}"), vec![])
                .unwrap();
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    let count = eng.list(MemoryCategory::Task).unwrap().len();
    assert_eq!(count, 20, "all concurrent writes must persist without loss");
    let _ = std::fs::remove_file(&path);
}

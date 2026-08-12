//! Criterion bench：memory 检索热点基线。
//!
//! 覆盖记忆库核心路径：FTS5 全文检索（英文/中文 trigram/未命中）、
//! 单条 upsert 写入、分类检索。hybrid 语义路径（BM25 × 余弦融合）见
//! `deepseeknova-graph/benches/retrieval.rs`（同一融合算法家族）。
//!
//! 运行：`cargo bench -p deepseeknova-core --bench memory_search`

// 基准辅助代码：fixture 构建失败即视为致命（预期行为），豁免生产 lint。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use criterion::{criterion_group, criterion_main, Criterion};
use deepseeknova_core::memory::store::{MemoryCategory, MemoryEntry, MemoryStore};
use std::hint::black_box;

/// 构造一条确定性记忆（中英混合内容，贴近真实任务记忆）。
fn entry(i: usize) -> MemoryEntry {
    MemoryEntry {
        id: format!("mem-{i}"),
        content: format!(
            "任务 {i}：用户偏好使用 Rust 与 sqlite，拒绝引入非必要依赖；\
             recall note: use tokio::sync::Mutex instead of std::sync::Mutex for shared env locks"
        ),
        tags: vec!["rust".to_string(), "sqlite".to_string(), format!("task{i}")],
        category: MemoryCategory::Task,
        source: "auto-distill".to_string(),
        created_at: 1_700_000_000 + i as i64,
        importance: 0.3 + (i % 5) as f32 * 0.1,
    }
}

/// 填充 `n` 条记忆（含 1/3 中文条目，覆盖 CJK trigram 检索路径）。
fn seeded_store(n: usize) -> MemoryStore {
    let store = MemoryStore::open_in_memory().expect("open memory store");
    for i in 0..n {
        let mut e = entry(i);
        if i % 3 == 0 {
            e.content = format!("中文记忆 {i}：审查发现路径逃逸漏洞，修复后补回归测试");
            e.tags.push("中文".to_string());
        }
        store.store(&e).expect("store entry");
    }
    store
}

fn bench_upsert(c: &mut Criterion) {
    let store = seeded_store(50);
    let e = entry(999);
    c.bench_function("memory/store_upsert", |b| {
        b.iter(|| {
            store.store(&e).expect("store entry");
        })
    });
}

fn bench_search(c: &mut Criterion) {
    let store = seeded_store(200);
    let mut group = c.benchmark_group("memory/search");
    group.bench_function("hit_english", |b| {
        b.iter(|| {
            let hits = store.search("tokio sync mutex", 10);
            let _ = black_box(hits);
        })
    });
    group.bench_function("hit_chinese_trigram", |b| {
        b.iter(|| {
            let hits = store.search("路径逃逸", 10);
            let _ = black_box(hits);
        })
    });
    group.bench_function("miss", |b| {
        b.iter(|| {
            let hits = store.search("nonexistent_zzz_token", 10);
            let _ = black_box(hits);
        })
    });
    group.finish();
}

fn bench_search_category(c: &mut Criterion) {
    let store = seeded_store(200);
    c.bench_function("memory/search_category", |b| {
        b.iter(|| {
            let hits = store.search_category("rust", MemoryCategory::Task, 10);
            let _ = black_box(hits);
        })
    });
}

fn bench_count(c: &mut Criterion) {
    let store = seeded_store(200);
    c.bench_function("memory/count", |b| {
        b.iter(|| {
            let n = store.count();
            let _ = black_box(n);
        })
    });
}

criterion_group!(
    memory_search_benches,
    bench_upsert,
    bench_search,
    bench_search_category,
    bench_count,
);
criterion_main!(memory_search_benches);

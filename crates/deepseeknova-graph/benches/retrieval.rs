//! Criterion bench：graph 检索热点基线。
//!
//! 覆盖四条热点路径：tree-sitter 解析（parse_source）、个性化 PageRank、
//! 全量/增量索引刷新（refresh）、FTS5 BM25 检索与 hybrid（BM25 × 余弦）融合。
//! 纯词法路径（无 embedder）与 hybrid 路径分别建标。
//!
//! 运行：`cargo bench -p deepseeknova-graph`

// 基准辅助代码：fixture 构建失败即视为致命（预期行为），豁免生产 lint。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use criterion::{criterion_group, criterion_main, Criterion};
use deepseeknova_core::DeepseeknovaError;
use deepseeknova_graph::parser::{parse_source, Lang};
use deepseeknova_graph::{rank, EmbeddingProvider, GraphIndex, NodeKind};
use std::hint::black_box;
use std::sync::Arc;
use tempfile::TempDir;

/// 合成一个中等体量的 Rust 模块（struct + impl + fn + trait + doc 注释），
/// 用于 parse_source 与索引构建基准。
fn rust_module(i: usize) -> String {
    format!(
        r#"//! Module {i} — pipeline stage docs.
use std::collections::HashMap;

/// Handler for stage {i} processing.
pub struct Handler{i} {{
    pub id: u32,
    cache: HashMap<String, u32>,
}}

impl Handler{i} {{
    /// Process a batch item under `key`.
    pub fn process(&mut self, key: &str) -> u32 {{
        let v = self.cache.entry(key.to_string()).or_insert(0);
        *v += 1;
        *v
    }}

    /// Reset all counters.
    pub fn reset(&mut self) {{
        self.cache.clear();
    }}
}}

/// Compute the stage-{i} aggregate result.
pub fn compute_{i}(a: u32, b: u32) -> u32 {{
    let mut h = Handler{i} {{ id: a, cache: HashMap::new() }};
    h.process("key");
    h.id.wrapping_add(b)
}}

/// Pipeline contract for stage {i}.
pub trait Pipeline{i} {{
    /// Run the stage on `input`.
    fn run(&mut self, input: u32) -> u32;
}}
"#
    )
}

/// 确定性伪嵌入：长度 16 的字符散列向量（归一化），仅用于 hybrid 检索路径的
/// 延迟基线，不追求语义质量。
struct FakeEmbed;

impl EmbeddingProvider for FakeEmbed {
    fn embed(&self, text: &str) -> Result<Vec<f32>, DeepseeknovaError> {
        let mut v = vec![0.0f32; 16];
        for (k, b) in text.bytes().enumerate() {
            v[k % 16] += f32::from(b);
        }
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
        Ok(v.into_iter().map(|x| x / norm).collect())
    }
}

/// 在临时目录中写入 `n` 个合成 Rust 模块并构建索引（返回持有者与索引）。
fn indexed_fixture(n: usize) -> (TempDir, GraphIndex) {
    let dir = TempDir::new().expect("tempdir");
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).expect("create src");
    for i in 0..n {
        std::fs::write(src.join(format!("mod{i}.rs")), rust_module(i)).expect("write module");
    }
    let mut index = GraphIndex::open(dir.path(), 1_048_576).expect("open graph index");
    index.refresh().expect("refresh graph index");
    (dir, index)
}

fn bench_parse_source(c: &mut Criterion) {
    let src = rust_module(7);
    c.bench_function("graph/parse_rust_module", |b| {
        b.iter(|| {
            let parsed = parse_source(Lang::Rust, "src/mod7.rs", &src);
            let _ = black_box(parsed);
        })
    });
}

fn bench_pagerank(c: &mut Criterion) {
    let n = 1000usize;
    let nodes: Vec<String> = (0..n).map(|i| format!("node{i}")).collect();
    let edges: Vec<(String, String)> = (0..n)
        .map(|i| (format!("node{i}"), format!("node{}", (i + 1) % n)))
        .collect();
    c.bench_function("graph/pagerank_1000_nodes", |b| {
        b.iter(|| {
            let ranks = rank::pagerank(&nodes, &edges, &[], 0.85, 50);
            black_box(ranks);
        })
    });
}

fn bench_refresh(c: &mut Criterion) {
    c.bench_function("graph/refresh_30_files", |b| {
        let dir = TempDir::new().expect("tempdir");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).expect("create src");
        for i in 0..30 {
            std::fs::write(src.join(format!("mod{i}.rs")), rust_module(i)).expect("write module");
        }
        let mut index = GraphIndex::open(dir.path(), 1_048_576).expect("open graph index");
        b.iter(|| {
            let report = index.refresh();
            let _ = black_box(report);
        })
    });
}

fn bench_lexical_search(c: &mut Criterion) {
    let (_dir, index) = indexed_fixture(30);
    let mut group = c.benchmark_group("graph/search_lexical");
    group.bench_function("hit_method", |b| {
        b.iter(|| {
            let nodes = index.search("process", Some(NodeKind::Method), 10);
            let _ = black_box(nodes);
        })
    });
    group.bench_function("hit_any", |b| {
        b.iter(|| {
            let nodes = index.search("Handler", None, 10);
            let _ = black_box(nodes);
        })
    });
    group.bench_function("miss", |b| {
        b.iter(|| {
            let nodes = index.search("nonexistent_symbol_xyz", None, 10);
            let _ = black_box(nodes);
        })
    });
    group.finish();
}

fn bench_hybrid_search(c: &mut Criterion) {
    let dir = TempDir::new().expect("tempdir");
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).expect("create src");
    for i in 0..30 {
        std::fs::write(src.join(format!("mod{i}.rs")), rust_module(i)).expect("write module");
    }
    let mut index =
        GraphIndex::open_with_embedder(dir.path(), 1_048_576, Some(Arc::new(FakeEmbed)), "fake")
            .expect("open graph index");
    index.refresh().expect("refresh graph index");

    c.bench_function("graph/search_hybrid", |b| {
        b.iter(|| {
            let nodes = index.search_best("process", Some(NodeKind::Method), 10);
            let _ = black_box(nodes);
        })
    });
}

criterion_group!(
    graph_retrieval_benches,
    bench_parse_source,
    bench_pagerank,
    bench_refresh,
    bench_lexical_search,
    bench_hybrid_search,
);
criterion_main!(graph_retrieval_benches);

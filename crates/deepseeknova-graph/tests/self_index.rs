//! 自举冒烟测试：对 DeepseekNova 本仓库建索引，验证真实规模下的检索行为。
//!
//! 保持 `#[ignore]` 的原因（M8 复查后确认，2026-08-08）：
//! 1. 依赖运行环境仓库布局——在 CI 检出深度/子模块缺失时会行为漂移；
//! 2. 对真实仓库全量解析耗时秒级且会写入 root/.deepseeknova/graph.db
//!    （派生数据），不适合进常规测试门禁；
//! 3. 公开 API 端到端链路已由 `tests/e2e.rs` 在临时目录上覆盖
//!    （refresh → search/trace/repo_map/deps），可稳定进 CI。
//!
//! 手动运行：`cargo test -p deepseeknova-graph --test self_index -- --ignored --nocapture`
#![allow(clippy::unwrap_used, clippy::expect_used)]

use deepseeknova_graph::{Direction, EdgeKind, GraphIndex};

/// 定位 workspace 根：从本 crate 目录（CARGO_MANIFEST_DIR）上溯两级到仓库根。
fn workspace_root() -> std::path::PathBuf {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // crates/deepseeknova-graph → crates → <root>
    manifest
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

#[test]
#[ignore = "bootstraps a full index over the real repo; run explicitly"]
fn self_index_finds_known_symbols() {
    let root = workspace_root();
    // 对真实仓库根建索引，写到 root/.deepseeknova/graph.db（已被 .gitignore，派生数据保留无害）。
    let mut idx = GraphIndex::open(&root, 512 * 1024).expect("open index");
    let report = idx.refresh().expect("refresh index");
    eprintln!(
        "indexed files={} reparsed={} nodes={} edges={}",
        report.files_indexed, report.files_reparsed, report.nodes, report.edges
    );
    assert!(report.nodes > 100, "expected a non-trivial node count");

    // 成功标准 2：search_code("PermissionGate") 首位命中定义实体
    let hits = idx.search("PermissionGate", None, 10).expect("search");
    assert!(!hits.is_empty(), "PermissionGate should be found");
    assert_eq!(
        hits[0].name, "PermissionGate",
        "top hit should be the PermissionGate definition, got {}",
        hits[0].name
    );

    // traverse：PermissionGate 应有邻居关系（callers/callees 任一非空即可）
    let neighbors = idx
        .neighbors("PermissionGate", &[EdgeKind::Calls], Direction::Both, 2)
        .expect("neighbors");
    eprintln!("PermissionGate neighbors via calls: {}", neighbors.len());

    // skeleton 可取
    let sk = idx.skeleton("PermissionGate").expect("skeleton");
    assert!(sk.contains("PermissionGate"));

    // repo_map 在 1024 token 预算内非空（成功标准 4 的下界验证）
    let map = idx.repo_map(1024, &[]).expect("repo map");
    assert!(
        !map.is_empty(),
        "repo map should be non-empty for a real repo"
    );
    eprintln!("repo_map chars={}", map.chars().count());
}

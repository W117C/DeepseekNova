//! 端到端集成测试：临时多语言项目 → refresh → 检索/追踪/repo_map/依赖图。
//!
//! 与 `self_index.rs`（对真实仓库建索引、`#[ignore]`）互补：本文件在
//! 临时目录上建小型多文件项目，覆盖公开 API 的完整链路且可稳定进 CI。

use deepseeknova_graph::{Direction, EdgeKind, GraphIndex};
use tempfile::tempdir;

/// 建一个小型多语言项目（Rust + Python + Go + Cargo.toml），返回持有
/// TempDir 的句柄（保持目录存活）与根路径。
fn setup_project() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("src")).unwrap();
    // Rust
    std::fs::write(
        root.join("src/lib.rs"),
        "/// permission gate\npub struct PermissionGate {}\n\
         pub fn build_agent() { permission_gate_for(); }\n\
         pub fn permission_gate_for() {}\n",
    )
    .unwrap();
    // Python
    std::fs::write(
        root.join("src/service.py"),
        "class Service:\n    def start(self):\n        return run()\n\ndef run():\n    return 1\n",
    )
    .unwrap();
    // Go：main.go 相对导入 ./util（依赖 go 扩展探测，见 store::resolve_file_node）
    std::fs::create_dir_all(root.join("util")).unwrap();
    std::fs::write(
        root.join("main.go"),
        "package main\n\nimport (\n\t\"fmt\"\n\t\"./util\"\n)\n\nfunc main() { fmt.Println(\"hi\") }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("util/util.go"),
        "package util\n\nfunc Helper() int { return 1 }\n",
    )
    .unwrap();
    // Manifest
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[dependencies]\nserde = \"1\"\n",
    )
    .unwrap();
    (dir, root)
}

#[test]
fn e2e_refresh_search_trace_repomap_across_languages() {
    let (_dir, root) = setup_project();
    let mut idx = GraphIndex::open(&root, 1_048_576).unwrap();
    let report = idx.refresh().unwrap();
    assert!(
        report.files_indexed >= 4,
        "files_indexed={}",
        report.files_indexed
    );
    assert!(report.nodes > 5, "nodes={}", report.nodes);

    // 跨语言检索
    let hits = idx.search("build_agent", None, 10).unwrap();
    assert!(hits.iter().any(|n| n.name == "build_agent"));
    let svc = idx.search("Service", None, 10).unwrap();
    assert!(
        svc.iter().any(|n| n.name == "Service"),
        "Python class 应可检索：{:?}",
        svc.iter().map(|n| &n.name).collect::<Vec<_>>()
    );

    // 跨文件调用链追踪（callers 归一为 源→…→目标）
    let tr = idx
        .trace(
            "permission_gate_for",
            &[EdgeKind::Calls],
            Direction::Callers,
            4,
        )
        .unwrap();
    assert!(
        tr.paths
            .iter()
            .any(|p| p.iter().any(|n| n.name == "build_agent")),
        "追踪应命中调用方 build_agent"
    );

    // repo_map 预算内非空
    let map = idx.repo_map(1024, &[]).unwrap();
    assert!(
        map.contains("build_agent") || map.contains("permission_gate_for"),
        "repo_map 应含符号：{map}"
    );

    // 大小写不敏感（经公开 API）
    let up = idx.search("PERMISSIONGATE", None, 10).unwrap();
    assert!(!up.is_empty() && up[0].name == "PermissionGate", "{up:?}");
}

#[test]
fn e2e_external_deps_and_go_local_import() {
    let (_dir, root) = setup_project();
    let mut idx = GraphIndex::open(&root, 1_048_576).unwrap();
    idx.refresh().unwrap();

    // Cargo.toml 外部依赖
    let deps = idx.external_deps().unwrap();
    assert!(
        deps.iter().any(|(p, d)| d == "serde" && p == "Cargo.toml"),
        "{deps:?}"
    );
    let file_deps = idx.external_deps_for_file("src/lib.rs").unwrap();
    assert!(file_deps.contains(&"serde".to_string()), "{file_deps:?}");

    // Go 相对导入 → file→file Imports 边
    let imports = idx.edges(EdgeKind::Imports).unwrap();
    assert!(
        imports
            .iter()
            .any(|(s, d)| s.contains("main.go") && d.contains("util.go")),
        "main.go → util.go：{imports:?}"
    );
}

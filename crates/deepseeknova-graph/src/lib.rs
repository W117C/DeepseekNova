//! # deepseeknova-graph
//!
//! 代码图引擎：tree-sitter 解析 → SQLite 异构图（FTS5 BM25）→
//! 个性化 PageRank 排序 → 图检索 API 与 token 预算 repo map。

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented,
        clippy::dbg_macro
    )
)]

pub mod model;
pub mod parser;
pub mod rank;
pub mod repomap;
pub mod store;

pub use deepseeknova_core::memory::embedding::EmbeddingProvider;
pub use model::{EdgeKind, GraphError, Node, NodeKind};
pub use store::{Direction, TraceResult};

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 代码图索引门面。内部 Store 连接串行；多处共享时外层包 `Arc<Mutex<GraphIndex>>`。
pub struct GraphIndex {
    store: store::Store,
    root: PathBuf,
    max_file_size: u64,
}

impl GraphIndex {
    /// 打开（或创建）workspace 的图索引。不触发解析——refresh 才解析。
    pub fn open(root: impl AsRef<Path>, max_file_size: u64) -> Result<Self, GraphError> {
        Self::open_with_embedder(root, max_file_size, None, "")
    }

    /// 打开图索引并装配语义嵌入后端（写入即嵌入 + hybrid 检索）。
    /// 嵌入由调用方装配（如 provider 的 RemoteEmbedder 包成
    /// `Arc<dyn EmbeddingProvider>`）；不可用时检索回落纯词法，不阻断既有功能。
    pub fn open_with_embedder(
        root: impl AsRef<Path>,
        max_file_size: u64,
        embedder: Option<Arc<dyn EmbeddingProvider>>,
        model: &str,
    ) -> Result<Self, GraphError> {
        let root = root.as_ref().to_path_buf();
        let db = root.join(".deepseeknova").join("graph.db");
        let store = store::Store::open_with_embedder(&db, embedder, model)?;
        Ok(Self {
            store,
            root,
            max_file_size,
        })
    }

    /// 增量刷新并重算 PageRank（分数写回 nodes.score）。
    pub fn refresh(&mut self) -> Result<store::RefreshReport, GraphError> {
        let report = self.store.refresh(&self.root, self.max_file_size)?;
        let nodes: Vec<String> = self.store.all_nodes()?.into_iter().map(|n| n.id).collect();
        let edges: Vec<(String, String)> = self
            .store
            .all_edges()?
            .into_iter()
            .map(|e| (e.src, e.dst))
            .collect();
        let scores = rank::pagerank(&nodes, &edges, &[], 0.85, 50);
        self.store
            .set_scores(&scores.into_iter().collect::<Vec<_>>())?;
        Ok(report)
    }

    pub fn search(
        &self,
        query: &str,
        kind: Option<NodeKind>,
        limit: usize,
    ) -> Result<Vec<Node>, GraphError> {
        self.store.search(query, kind, limit)
    }

    /// 是否装配了语义嵌入后端（`open_with_embedder` 传入 Some）。
    /// 未装配时 [`Self::search_best`] 逐字节走纯词法检索，结果与 [`Self::search`]
    /// 严格一致；装配后 `search_code` 等工具自动启用 hybrid（语义 + 词法融合）。
    pub fn has_embedder(&self) -> bool {
        self.store.has_embedder()
    }

    /// 检索入口：装配了嵌入后端 → [`Self::search_hybrid`]（语义增强）；否则
    /// 逐字节委托 [`Self::search`]（零行为变化）。工具调用方无需感知嵌入装配。
    pub fn search_best(
        &self,
        query: &str,
        kind: Option<NodeKind>,
        limit: usize,
    ) -> Result<Vec<Node>, GraphError> {
        if self.has_embedder() {
            self.search_hybrid(query, kind, limit)
        } else {
            self.search(query, kind, limit)
        }
    }

    /// 混合检索（词法 ∪ 语义），默认 `0.5*bm25 + 0.5*余弦`；嵌入不可用回落纯词法。
    pub fn search_hybrid(
        &self,
        query: &str,
        kind: Option<NodeKind>,
        limit: usize,
    ) -> Result<Vec<Node>, GraphError> {
        self.store.search_hybrid(query, kind, limit)
    }

    /// 混合检索，可调词法权重 `weight`（余弦权重 `1 - weight`）。
    pub fn search_hybrid_with_weight(
        &self,
        query: &str,
        kind: Option<NodeKind>,
        limit: usize,
        weight: f64,
    ) -> Result<Vec<Node>, GraphError> {
        self.store
            .search_hybrid_with_weight(query, kind, limit, weight)
    }

    pub fn neighbors(
        &self,
        entity: &str,
        kinds: &[EdgeKind],
        dir: Direction,
        hops: usize,
    ) -> Result<Vec<Node>, GraphError> {
        let id = self.resolve(entity)?;
        self.store.neighbors(&id, kinds, dir, hops)
    }

    /// 多跳路径追踪：沿 `kinds` 按方向搜索（callers 归一为「源 → … → 目标」）。
    pub fn trace(
        &self,
        entity: &str,
        kinds: &[EdgeKind],
        dir: Direction,
        max_hops: usize,
    ) -> Result<TraceResult, GraphError> {
        let id = self.resolve(entity)?;
        self.store.trace_paths(&id, kinds, dir, max_hops)
    }

    /// 指定类型的全部边（测试与依赖查询用）。
    pub fn edges(&self, kind: EdgeKind) -> Result<Vec<(String, String)>, GraphError> {
        Ok(self
            .store
            .all_edges()?
            .into_iter()
            .filter(|e| e.kind == kind)
            .map(|e| (e.src, e.dst))
            .collect())
    }

    /// 骨架视图：doc + 签名 + 直接子实体签名。
    pub fn skeleton(&self, entity: &str) -> Result<String, GraphError> {
        let id = self.resolve(entity)?;
        let node = self
            .store
            .get(&id)?
            .ok_or_else(|| GraphError::EntityNotFound(entity.into()))?;
        let mut out = String::new();
        if !node.doc.is_empty() {
            out.push_str(&format!("// {}\n", node.doc));
        }
        out.push_str(&node.signature);
        out.push('\n');
        for child in self.store.children(&id)? {
            out.push_str(&format!("  {}\n", child.signature));
        }
        Ok(out)
    }

    /// 该实体的 (path, start_line, end_line)，供 retrieve_entity(full) 精确取码。
    pub fn location(&self, entity: &str) -> Result<(String, u32, u32), GraphError> {
        let id = self.resolve(entity)?;
        let n = self
            .store
            .get(&id)?
            .ok_or_else(|| GraphError::EntityNotFound(entity.into()))?;
        Ok((n.path, n.start_line, n.end_line))
    }

    /// 按相对路径取文件节点 id（依赖图文件→文件边查询用）。
    pub fn file_node(&self, path: &str) -> Result<Option<String>, GraphError> {
        self.store.file_node(path)
    }

    /// 全库外部依赖事实（path=清单文件路径, dep_name）。
    pub fn external_deps(&self) -> Result<Vec<(String, String)>, GraphError> {
        self.store.external_deps()
    }

    /// 某源码文件所属最近清单的外部依赖名。
    pub fn external_deps_for_file(&self, file_path: &str) -> Result<Vec<String>, GraphError> {
        self.store.external_deps_for_file(file_path)
    }

    /// token 预算内的 repo map；personalization 为种子符号名/路径。
    pub fn repo_map(
        &self,
        token_budget: usize,
        personalization: &[String],
    ) -> Result<String, GraphError> {
        if token_budget == 0 {
            return Ok(String::new());
        }
        let mut nodes = self.store.all_nodes()?;
        if !personalization.is_empty() {
            let ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
            let edges: Vec<(String, String)> = self
                .store
                .all_edges()?
                .into_iter()
                .map(|e| (e.src, e.dst))
                .collect();
            let seeds: Vec<String> = nodes
                .iter()
                .filter(|n| {
                    personalization
                        .iter()
                        .any(|p| &n.name == p || n.path.contains(p.as_str()))
                })
                .map(|n| n.id.clone())
                .collect();
            let scores = rank::pagerank(&ids, &edges, &seeds, 0.85, 50);
            for n in nodes.iter_mut() {
                n.score = *scores.get(&n.id).unwrap_or(&0.0);
            }
        }
        nodes.retain(|n| !n.signature.is_empty());
        Ok(repomap::render_repo_map(&nodes, token_budget))
    }

    /// entity 支持 `id`（含 `#`）、`path:name`、裸 `name`，以及
    /// `Type::method` 限定名（按 Contains 祖先链过滤同名候选）。
    fn resolve(&self, entity: &str) -> Result<String, GraphError> {
        if entity.contains('#') {
            return Ok(entity.to_string());
        }
        let qualified = entity.contains("::");
        let (name, path_hint) = if qualified {
            (entity.rsplit("::").next().unwrap_or(entity), None)
        } else {
            match entity.split_once(':') {
                Some((p, n)) => (n, Some(p)),
                None => (entity, None),
            }
        };
        let hits = self.store.find_by_name(name)?;
        let hits = if qualified {
            let qualifiers: Vec<String> = entity.split("::").map(|s| s.to_string()).collect();
            let mut filtered = Vec::new();
            for n in hits {
                // (a) 祖先链匹配（Python 类 / Go / JS 类等有 Contains 边）。
                let mut cur = n.id.clone();
                let mut hops = 0;
                let mut found = false;
                while let Some(parent) = self.store.parents(&cur)?.into_iter().next() {
                    hops += 1;
                    if hops > 32 {
                        break;
                    }
                    if qualifiers.contains(&parent.name) {
                        found = true;
                        break;
                    }
                    cur = parent.id;
                }
                // (b) 引用/调用/实现目标匹配：Rust 固有 impl 方法没有
                // Contains 归属边，但方法体引用/调用/实现类型名（如
                // `GraphIndex {}` → References GraphIndex）。
                if !found {
                    for kind in [EdgeKind::References, EdgeKind::Calls, EdgeKind::Implements] {
                        let neighbors =
                            self.store
                                .neighbors(&n.id, &[kind], Direction::Callees, 1)?;
                        if neighbors.iter().any(|nb| qualifiers.contains(&nb.name)) {
                            found = true;
                            break;
                        }
                    }
                }
                if found {
                    filtered.push(n);
                }
            }
            filtered
        } else {
            hits
        };
        let pick = match path_hint {
            Some(p) => hits.into_iter().find(|n| n.path.contains(p)),
            None => hits.into_iter().max_by(|a, b| {
                a.score
                    .partial_cmp(&b.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
        };
        pick.map(|n| n.id)
            .ok_or_else(|| GraphError::EntityNotFound(entity.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_core::DeepseeknovaError;
    use tempfile::tempdir;

    #[test]
    fn search_best_without_embedder_is_byte_identical_to_search() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn build_agent() {}\npub fn permission_gate_for() {}\n",
        )
        .unwrap();
        let mut idx = GraphIndex::open(root, 1_048_576).unwrap();
        idx.refresh().unwrap();
        assert!(!idx.has_embedder(), "open 不装配嵌入后端");

        for query in ["build_agent", "permission", "no_such_symbol", ""] {
            let plain = idx.search(query, None, 10).unwrap();
            let best = idx.search_best(query, None, 10).unwrap();
            // 零回归保证：未装配嵌入时 search_code 的结果严格不变。
            assert_eq!(best.len(), plain.len(), "query={query:?} 长度不一致");
            for (b, p) in best.iter().zip(plain.iter()) {
                assert_eq!(b.name, p.name, "query={query:?} 名称不一致");
                assert_eq!(b.path, p.path, "query={query:?} 路径不一致");
            }
        }
    }

    #[test]
    fn index_search_neighbors_skeleton_location_repomap() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"),
            "/// build it\npub fn build_agent() { permission_gate_for(); }\npub fn permission_gate_for() {}\n").unwrap();
        let mut idx = GraphIndex::open(root, 1_048_576).unwrap();
        idx.refresh().unwrap();

        // search 命中定义
        let hits = idx.search("build_agent", None, 10).unwrap();
        assert!(hits.iter().any(|n| n.name == "build_agent"));

        // neighbors：permission_gate_for 的 callers 含 build_agent
        let callers = idx
            .neighbors(
                "permission_gate_for",
                &[EdgeKind::Calls],
                Direction::Callers,
                2,
            )
            .unwrap();
        assert!(callers.iter().any(|n| n.name == "build_agent"));

        // skeleton 含签名与 doc
        let sk = idx.skeleton("build_agent").unwrap();
        assert!(sk.contains("build_agent"));
        assert!(sk.contains("build it"));

        // location 行区间
        let (path, s, e) = idx.location("build_agent").unwrap();
        assert!(path.contains("lib.rs"));
        assert!(s >= 1 && e >= s);

        // repo_map 非空且含签名
        let map = idx.repo_map(1024, &[]).unwrap();
        assert!(map.contains("build_agent") || map.contains("permission_gate_for"));

        // 不存在实体报错
        assert!(idx.skeleton("no_such_symbol").is_err());
    }

    #[test]
    fn resolve_qualified_type_method() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "struct GraphIndex {}\nimpl GraphIndex {\n    fn open() -> GraphIndex { GraphIndex {} }\n}\n",
        )
        .unwrap();
        let mut idx = GraphIndex::open(root, 1_048_576).unwrap();
        idx.refresh().unwrap();

        // 裸方法名可解析。
        assert!(!idx.store.find_by_name("open").unwrap().is_empty());

        // 限定名 GraphIndex::open 必须解析到该方法（此前直接 EntityNotFound）。
        let sk = idx.skeleton("GraphIndex::open").unwrap();
        assert!(
            sk.contains("fn open") || sk.contains("open"),
            "限定名应解析到 open 方法骨架: {sk}"
        );
        assert!(
            idx.trace(
                "GraphIndex::open",
                &[EdgeKind::Calls],
                Direction::Callers,
                6
            )
            .is_ok(),
            "限定名 trace 不得 EntityNotFound"
        );
    }

    #[test]
    fn trace_follows_dynamic_dispatch_to_all_impl_candidates() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/animals.rs"),
            "trait Animal {\n    fn speak(&self);\n}\n\n\
             struct Dog;\nimpl Animal for Dog {\n    fn speak(&self) {}\n}\n\n\
             struct Cat;\nimpl Animal for Cat {\n    fn speak(&self) {}\n}\n\n\
             fn make_noise(a: Box<dyn Animal>) {\n    a.speak();\n}\n",
        )
        .unwrap();
        let mut idx = GraphIndex::open(root, 1_048_576).unwrap();
        idx.refresh().unwrap();

        let tr = idx
            .trace(
                "make_noise",
                &[EdgeKind::Calls, EdgeKind::Dispatch],
                Direction::Callees,
                6,
            )
            .unwrap();
        let impl_paths: Vec<_> = tr
            .paths
            .iter()
            .filter(|p| p.len() >= 3 && p[2].kind == NodeKind::Method)
            .collect();
        assert_eq!(
            impl_paths.len(),
            2,
            "dyn 调用应桥接到两个 impl 候选：{:?}",
            tr.paths
                .iter()
                .map(|p| p.iter().map(|n| n.id.as_str()).collect::<Vec<_>>())
                .collect::<Vec<_>>()
        );
        assert!(
            impl_paths[0][2].id != impl_paths[1][2].id,
            "两个候选必须是不同 impl 方法节点"
        );
        assert!(!tr.truncated);
    }

    #[test]
    fn trace_callers_returns_reversed_call_chain() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/chain.rs"),
            "pub fn a() { b(); }\npub fn b() { c(); }\npub fn c() {}\n",
        )
        .unwrap();
        let mut idx = GraphIndex::open(root, 1_048_576).unwrap();
        idx.refresh().unwrap();

        let tr = idx
            .trace("c", &[EdgeKind::Calls], Direction::Callers, 6)
            .unwrap();
        assert_eq!(tr.paths.len(), 1, "应只有 a→b→c 一条链");
        let names: Vec<&str> = tr.paths[0].iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
        assert!(!tr.truncated);
    }

    #[test]
    fn trace_truncates_at_max_hops() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/deep.rs"),
            "pub fn a() { b(); }\npub fn b() { c(); }\npub fn c() { d(); }\n\
             pub fn d() { e(); }\npub fn e() { f(); }\npub fn f() { g(); }\npub fn g() {}\n",
        )
        .unwrap();
        let mut idx = GraphIndex::open(root, 1_048_576).unwrap();
        idx.refresh().unwrap();

        let tr = idx
            .trace("g", &[EdgeKind::Calls], Direction::Callers, 3)
            .unwrap();
        assert!(tr.truncated, "深度超限必须显式标注");
        assert_eq!(tr.paths.len(), 1);
        let names: Vec<&str> = tr.paths[0].iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["d", "e", "f", "g"], "截断在 3 跳处");
    }

    #[test]
    fn references_edge_links_symbol_usage_in_same_file() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "pub struct Foo {}\npub fn use_foo() -> Foo { Foo {} }\n",
        )
        .unwrap();
        let mut idx = GraphIndex::open(root, 1_048_576).unwrap();
        idx.refresh().unwrap();

        let refs = idx.edges(EdgeKind::References).unwrap();
        assert_eq!(refs.len(), 1, "应恰好一条引用边：{refs:?}");
        let (src, dst) = &refs[0];
        assert!(src.contains("use_foo"), "{src}");
        assert!(dst.contains("Foo"), "{dst}");
        // 引用边可通过 neighbors 查到
        let callers = idx
            .neighbors("Foo", &[EdgeKind::References], Direction::Callers, 1)
            .unwrap();
        assert!(callers.iter().any(|n| n.name == "use_foo"));
    }

    #[test]
    fn references_edge_links_cross_file_symbol() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/a.rs"),
            "pub fn use_bar() -> Bar { Bar {} }\n",
        )
        .unwrap();
        std::fs::write(root.join("src/b.rs"), "pub struct Bar {}\n").unwrap();
        let mut idx = GraphIndex::open(root, 1_048_576).unwrap();
        idx.refresh().unwrap();

        let refs = idx.edges(EdgeKind::References).unwrap();
        assert_eq!(refs.len(), 1, "跨文件引用边：{refs:?}");
        let (src, dst) = &refs[0];
        assert!(src.contains("a.rs#use_bar"), "{src}");
        assert!(dst.contains("b.rs#Bar"), "{dst}");
    }

    #[test]
    fn recursive_function_has_no_self_reference_edge() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/r.rs"),
            "pub fn recur(n: u32) -> u32 {\n    if n == 0 { 0 } else { recur(n - 1) }\n}\n",
        )
        .unwrap();
        let mut idx = GraphIndex::open(root, 1_048_576).unwrap();
        idx.refresh().unwrap();

        assert!(
            idx.edges(EdgeKind::References).unwrap().is_empty(),
            "递归调用走 Calls，不应产生自引用 References 边"
        );
        // 调用边仍在（递归本身）
        assert!(
            idx.edges(EdgeKind::Calls)
                .unwrap()
                .iter()
                .any(|(s, d)| s.contains("recur") && d.contains("recur")),
            "递归 Calls 边应保留"
        );
    }

    #[test]
    fn manifest_and_use_declaration_produce_external_deps() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "use serde::Serialize;\npub fn main_fn() {}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\n\n[dependencies]\nserde = \"1\"\n",
        )
        .unwrap();
        let mut idx = GraphIndex::open(root, 1_048_576).unwrap();
        idx.refresh().unwrap();

        let deps = idx.external_deps().unwrap();
        assert!(
            deps.iter().any(|(p, d)| p == "Cargo.toml" && d == "serde"),
            "外部依赖表应有 serde：{deps:?}"
        );
        let file_deps = idx.external_deps_for_file("src/lib.rs").unwrap();
        assert!(file_deps.contains(&"serde".to_string()), "{file_deps:?}");
    }

    #[test]
    fn js_relative_import_creates_file_to_file_edge() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/main.js"),
            "import x from './util.js';\nexport function main_fn() { x(); }\n",
        )
        .unwrap();
        std::fs::write(root.join("src/util.js"), "export const x = 1;\n").unwrap();
        let mut idx = GraphIndex::open(root, 1_048_576).unwrap();
        idx.refresh().unwrap();

        let imports = idx.edges(EdgeKind::Imports).unwrap();
        assert!(
            imports
                .iter()
                .any(|(s, d)| s.contains("main.js") && d.contains("util.js")),
            "main.js → util.js 文件间 Imports 边：{imports:?}"
        );
        // 反向：util.js 的依赖方
        let dependents = idx
            .neighbors("util.js", &[EdgeKind::Imports], Direction::Callers, 1)
            .unwrap();
        assert!(dependents.iter().any(|n| n.name == "main.js"));
    }

    #[test]
    fn python_from_import_matches_symbol_by_name() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/m.py"), "from pkg import Thing\n").unwrap();
        std::fs::write(root.join("src/pkg.py"), "class Thing:\n    pass\n").unwrap();
        let mut idx = GraphIndex::open(root, 1_048_576).unwrap();
        idx.refresh().unwrap();

        let importers = idx
            .neighbors("Thing", &[EdgeKind::Imports], Direction::Callers, 1)
            .unwrap();
        assert!(
            importers.iter().any(|n| n.path == "src/m.py"),
            "m.py 应 import Thing：{:?}",
            importers
                .iter()
                .map(|n| (&n.path, &n.name))
                .collect::<Vec<_>>()
        );
    }

    /// 确定性嵌入替身：查询 token "needle" 与目标 doc "ferris" 语义对应但无共词。
    struct FakeEmbed;

    impl EmbeddingProvider for FakeEmbed {
        fn embed(&self, text: &str) -> Result<Vec<f32>, DeepseeknovaError> {
            if text.contains("ferris") {
                Ok(vec![0.9, 0.1])
            } else if text.contains("needle") {
                Ok(vec![1.0, 0.0])
            } else {
                Ok(vec![0.0, 1.0])
            }
        }
    }

    #[test]
    fn open_with_embedder_enables_hybrid_search() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "/// needle alpha formatter\npub fn alpha_needle() {}\n\
             /// ferris crab language\npub fn ferris_crab() {}\n",
        )
        .unwrap();
        let mut idx = GraphIndex::open_with_embedder(
            root,
            1_048_576,
            Some(Arc::new(FakeEmbed)),
            "test-model",
        )
        .unwrap();
        idx.refresh().unwrap();

        let plain = idx.search("needle", None, 10).unwrap();
        assert!(
            plain.iter().all(|n| n.name != "ferris_crab"),
            "纯词法不得召回语义命中"
        );
        let hy = idx.search_hybrid("needle", None, 10).unwrap();
        assert!(
            hy.iter().any(|n| n.name == "ferris_crab"),
            "GraphIndex hybrid 必须召回语义独有命中"
        );
        assert_eq!(hy[0].name, "alpha_needle", "双命中应居首");
    }

    #[test]
    fn search_best_with_embedder_routes_to_hybrid() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "/// needle alpha formatter\npub fn alpha_needle() {}\n\
             /// ferris crab language\npub fn ferris_crab() {}\n",
        )
        .unwrap();
        let mut idx = GraphIndex::open_with_embedder(
            root,
            1_048_576,
            Some(Arc::new(FakeEmbed)),
            "test-model",
        )
        .unwrap();
        idx.refresh().unwrap();

        assert!(idx.has_embedder());
        // 纯词法不得召回语义命中。
        assert!(idx
            .search("needle", None, 10)
            .unwrap()
            .iter()
            .all(|n| n.name != "ferris_crab"));
        // search_best 装配嵌入后必须路由到 hybrid：召回语义独有命中。
        let best = idx.search_best("needle", None, 10).unwrap();
        assert!(
            best.iter().any(|n| n.name == "ferris_crab"),
            "{:?}",
            best.iter().map(|n| &n.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn short_query_like_fallback_orders_by_pagerank() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "pub fn alpha_go() { beta_go(); }\n").unwrap();
        std::fs::write(root.join("src/b.rs"), "pub fn beta_go() {}\n").unwrap();
        let mut idx = GraphIndex::open(root, 1_048_576).unwrap();
        idx.refresh().unwrap();

        // "go" 仅 2 字符 → LIKE 回退按 PageRank score 降序；beta_go 有入边 → 分更高。
        let hits = idx.search("go", None, 10).unwrap();
        let names: Vec<&str> = hits.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"alpha_go"), "{names:?}");
        assert!(names.contains(&"beta_go"), "{names:?}");
        assert_eq!(
            names[0], "beta_go",
            "更高 PageRank 的 beta_go 应居首：{names:?}"
        );
    }

    #[test]
    fn repo_map_personalization_ranks_seed_first() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/a.rs"),
            "pub fn seed_fn() {}\npub fn other_fn() {}\n",
        )
        .unwrap();
        std::fs::write(root.join("src/b.rs"), "pub fn zeta_fn() {}\n").unwrap();
        let mut idx = GraphIndex::open(root, 1_048_576).unwrap();
        idx.refresh().unwrap();

        let map = idx.repo_map(512, &["seed_fn".to_string()]).unwrap();
        assert!(map.contains("seed_fn"), "{map}");
        assert!(map.contains("zeta_fn"), "{map}");
        let seed_pos = map.find("pub fn seed_fn").expect("seed_fn 签名在 map 中");
        let other_pos = map.find("pub fn other_fn").expect("other_fn 签名在 map 中");
        assert!(
            seed_pos < other_pos,
            "个性化种子符号应排在普通符号前：{map}"
        );
    }

    #[test]
    fn repo_map_empty_repo_and_zero_budget() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let mut idx = GraphIndex::open(root, 1_048_576).unwrap();
        idx.refresh().unwrap();
        assert_eq!(idx.repo_map(0, &[]).unwrap(), "");
        assert_eq!(idx.repo_map(1024, &[]).unwrap(), "");

        // 有节点但零预算 → 空。
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "pub fn f() {}\n").unwrap();
        let mut idx2 = GraphIndex::open(root, 1_048_576).unwrap();
        idx2.refresh().unwrap();
        assert_eq!(idx2.repo_map(0, &[]).unwrap(), "");
    }

    #[test]
    fn edges_filters_by_kind() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/a.rs"),
            "use crate::helper;\npub fn alpha() { helper(); }\npub fn helper() {}\n",
        )
        .unwrap();
        let mut idx = GraphIndex::open(root, 1_048_576).unwrap();
        idx.refresh().unwrap();

        // 混合边库中按 kind 隔离：Contains/Calls/Imports 互不泄漏。
        let contains = idx.edges(EdgeKind::Contains).unwrap();
        assert_eq!(contains.len(), 2, "{contains:?}");
        let calls = idx.edges(EdgeKind::Calls).unwrap();
        assert!(
            calls
                .iter()
                .any(|(s, d)| s.contains("alpha") && d.contains("helper")),
            "{calls:?}"
        );
        let imports = idx.edges(EdgeKind::Imports).unwrap();
        assert!(
            imports
                .iter()
                .any(|(s, d)| s.contains("a.rs") && d.contains("helper")),
            "use crate::helper → file→helper Imports 边：{imports:?}"
        );
    }
}

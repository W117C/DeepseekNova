//! # deepseeknova-graph
//!
//! 代码图引擎：tree-sitter 解析 → SQLite 异构图（FTS5 BM25）→
//! 个性化 PageRank 排序 → 图检索 API 与 token 预算 repo map。

pub mod model;
pub mod parser;
pub mod rank;
pub mod repomap;
pub mod store;

pub use model::{EdgeKind, GraphError, Node, NodeKind};
pub use store::{Direction, TraceResult};

use std::path::{Path, PathBuf};

/// 代码图索引门面。内部 Store 连接串行；多处共享时外层包 `Arc<Mutex<GraphIndex>>`。
pub struct GraphIndex {
    store: store::Store,
    root: PathBuf,
    max_file_size: u64,
}

impl GraphIndex {
    /// 打开（或创建）workspace 的图索引。不触发解析——refresh 才解析。
    pub fn open(root: impl AsRef<Path>, max_file_size: u64) -> Result<Self, GraphError> {
        let root = root.as_ref().to_path_buf();
        let db = root.join(".deepseeknova").join("graph.db");
        let store = store::Store::open(&db)?;
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

    /// entity 支持 `id`（含 `#`）或 `path:name` 或裸 `name`。
    fn resolve(&self, entity: &str) -> Result<String, GraphError> {
        if entity.contains('#') {
            return Ok(entity.to_string());
        }
        let (name, path_hint) = match entity.split_once(':') {
            Some((p, n)) => (n, Some(p)),
            None => (entity, None),
        };
        let hits = self.store.find_by_name(name)?;
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
    use tempfile::tempdir;

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
}

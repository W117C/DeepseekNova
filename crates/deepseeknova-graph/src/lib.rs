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
pub use store::Direction;

use std::path::{Path, PathBuf};

/// 代码图索引门面。内部 Store 连接串行；多处共享时外层包 Arc<Mutex<GraphIndex>>。
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
        Ok(Self { store, root, max_file_size })
    }

    /// 增量刷新并重算 PageRank（分数写回 nodes.score）。
    pub fn refresh(&mut self) -> Result<store::RefreshReport, GraphError> {
        let report = self.store.refresh(&self.root, self.max_file_size)?;
        let nodes: Vec<String> = self.store.all_nodes()?.into_iter().map(|n| n.id).collect();
        let edges: Vec<(String, String)> =
            self.store.all_edges()?.into_iter().map(|e| (e.src, e.dst)).collect();
        let scores = rank::pagerank(&nodes, &edges, &[], 0.85, 50);
        self.store.set_scores(&scores.into_iter().collect::<Vec<_>>())?;
        Ok(report)
    }

    pub fn search(&self, query: &str, kind: Option<NodeKind>, limit: usize)
        -> Result<Vec<Node>, GraphError> { self.store.search(query, kind, limit) }

    pub fn neighbors(&self, entity: &str, kinds: &[EdgeKind], dir: Direction, hops: usize)
        -> Result<Vec<Node>, GraphError> {
        let id = self.resolve(entity)?;
        self.store.neighbors(&id, kinds, dir, hops)
    }

    /// 骨架视图：doc + 签名 + 直接子实体签名。
    pub fn skeleton(&self, entity: &str) -> Result<String, GraphError> {
        let id = self.resolve(entity)?;
        let node = self.store.get(&id)?.ok_or_else(|| GraphError::EntityNotFound(entity.into()))?;
        let mut out = String::new();
        if !node.doc.is_empty() { out.push_str(&format!("// {}\n", node.doc)); }
        out.push_str(&node.signature); out.push('\n');
        for child in self.store.children(&id)? {
            out.push_str(&format!("  {}\n", child.signature));
        }
        Ok(out)
    }

    /// 该实体的 (path, start_line, end_line)，供 retrieve_entity(full) 精确取码。
    pub fn location(&self, entity: &str) -> Result<(String, u32, u32), GraphError> {
        let id = self.resolve(entity)?;
        let n = self.store.get(&id)?.ok_or_else(|| GraphError::EntityNotFound(entity.into()))?;
        Ok((n.path, n.start_line, n.end_line))
    }

    /// token 预算内的 repo map；personalization 为种子符号名/路径。
    pub fn repo_map(&self, token_budget: usize, personalization: &[String])
        -> Result<String, GraphError> {
        if token_budget == 0 { return Ok(String::new()); }
        let mut nodes = self.store.all_nodes()?;
        if !personalization.is_empty() {
            let ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
            let edges: Vec<(String, String)> =
                self.store.all_edges()?.into_iter().map(|e| (e.src, e.dst)).collect();
            let seeds: Vec<String> = nodes.iter()
                .filter(|n| personalization.iter().any(|p| &n.name == p || n.path.contains(p.as_str())))
                .map(|n| n.id.clone()).collect();
            let scores = rank::pagerank(&ids, &edges, &seeds, 0.85, 50);
            for n in nodes.iter_mut() { n.score = *scores.get(&n.id).unwrap_or(&0.0); }
        }
        nodes.retain(|n| !n.signature.is_empty());
        Ok(repomap::render_repo_map(&nodes, token_budget))
    }

    /// entity 支持 `id`（含 `#`）或 `path:name` 或裸 `name`。
    fn resolve(&self, entity: &str) -> Result<String, GraphError> {
        if entity.contains('#') { return Ok(entity.to_string()); }
        let (name, path_hint) = match entity.split_once(':') {
            Some((p, n)) => (n, Some(p)),
            None => (entity, None),
        };
        let hits = self.store.find_by_name(name)?;
        let pick = match path_hint {
            Some(p) => hits.into_iter().find(|n| n.path.contains(p)),
            None => hits.into_iter().max_by(|a, b| a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal)),
        };
        pick.map(|n| n.id).ok_or_else(|| GraphError::EntityNotFound(entity.into()))
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
        let callers = idx.neighbors("permission_gate_for", &[EdgeKind::Calls], Direction::Callers, 2).unwrap();
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
}

//! SQLite 持久化：增量刷新、FTS5 检索、图邻居查询。

use crate::model::{node_id, EdgeKind, EdgeRec, GraphError, Node, NodeKind};
use crate::parser::{parse_source, Lang};
use deepseeknova_core::memory::embedding::{cosine, EmbeddingProvider};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

/// 邻居遍历方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// 入边方向：谁引用了/调用了目标（src → 目标）。
    Callers,
    /// 出边方向：目标引用了/调用了谁（目标 → dst）。
    Callees,
    /// 双向：入边与出边都展开。
    Both,
}

/// 追踪路径的默认最大边数（跳数）。
pub const DEFAULT_MAX_HOPS: usize = 6;

/// 单次追踪最多返回的路径数；超出时置 `truncated`。
const MAX_PATHS: usize = 100;

/// 追踪结果：路径按调用方向排列（callers 为「源 → … → 目标」）。
#[derive(Debug, Default)]
pub struct TraceResult {
    /// 找到的路径（每条为完整节点链）。
    pub paths: Vec<Vec<Node>>,
    /// 是否因最大跳数或路径数上限而截断。
    pub truncated: bool,
}

/// 路径追踪的展开状态：邻接表 + 方向/边类型过滤 + 结果与截断标记。
struct TraceExpander<'a> {
    fwd: HashMap<&'a str, Vec<(&'a str, EdgeKind)>>,
    rev: HashMap<&'a str, Vec<(&'a str, EdgeKind)>>,
    edge_kinds: Vec<EdgeKind>,
    dir: Direction,
    max_hops: usize,
    paths: Vec<Vec<String>>,
    truncated: bool,
}

impl<'a> TraceExpander<'a> {
    fn new(edges: &'a [EdgeRec], edge_kinds: &[EdgeKind], dir: Direction, max_hops: usize) -> Self {
        let mut fwd: HashMap<&str, Vec<(&str, EdgeKind)>> = HashMap::new();
        let mut rev: HashMap<&str, Vec<(&str, EdgeKind)>> = HashMap::new();
        for e in edges {
            fwd.entry(e.src.as_str())
                .or_default()
                .push((e.dst.as_str(), e.kind));
            rev.entry(e.dst.as_str())
                .or_default()
                .push((e.src.as_str(), e.kind));
        }
        for v in fwd.values_mut() {
            v.sort_by(|a, b| (a.1.as_str(), a.0).cmp(&(b.1.as_str(), b.0)));
        }
        for v in rev.values_mut() {
            v.sort_by(|a, b| (a.1.as_str(), a.0).cmp(&(b.1.as_str(), b.0)));
        }
        Self {
            fwd,
            rev,
            edge_kinds: edge_kinds.to_vec(),
            dir,
            max_hops,
            paths: Vec::new(),
            truncated: false,
        }
    }

    /// 按方向取邻居：Callers=入边，Callees=出边，Both=两者。
    fn adjacent(&self, cur: &str) -> Vec<(&'a str, EdgeKind)> {
        let mut out = Vec::new();
        if matches!(self.dir, Direction::Callers | Direction::Both) {
            out.extend(self.rev.get(cur).cloned().unwrap_or_default());
        }
        if matches!(self.dir, Direction::Callees | Direction::Both) {
            out.extend(self.fwd.get(cur).cloned().unwrap_or_default());
        }
        out
    }

    /// 深度受限的路径 DFS：每条路径去重防环；到深度上限后仍能延伸即置截断标记。
    fn dfs(&mut self, cur: &str, path: &mut Vec<String>, visited: &mut HashSet<String>) {
        let hops = path.len().saturating_sub(1);
        if hops >= self.max_hops {
            let has_more = self.adjacent(cur).iter().any(|(next, kind)| {
                (self.edge_kinds.is_empty() || self.edge_kinds.contains(kind))
                    && !visited.contains(*next)
            });
            if has_more {
                self.truncated = true;
            }
            if self.paths.len() < MAX_PATHS {
                self.paths.push(path.clone());
            }
            return;
        }
        let mut found = false;
        for (next, kind) in self.adjacent(cur) {
            if !(self.edge_kinds.is_empty() || self.edge_kinds.contains(&kind))
                || visited.contains(next)
            {
                continue;
            }
            found = true;
            if self.paths.len() >= MAX_PATHS {
                self.truncated = true;
                return;
            }
            visited.insert(next.to_string());
            path.push(next.to_string());
            self.dfs(next, path, visited);
            path.pop();
            visited.remove(next);
        }
        if !found && self.paths.len() < MAX_PATHS {
            self.paths.push(path.clone());
        }
    }
}

/// refresh 统计报告。
#[derive(Debug, Clone, Default)]
pub struct RefreshReport {
    /// 本次扫描到的文件总数。
    pub files_indexed: usize,
    /// 实际重新解析的文件数（内容变更）。
    pub files_reparsed: usize,
    /// refresh 后库中节点总数。
    pub nodes: usize,
    /// refresh 后库中边总数。
    pub edges: usize,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS files(path TEXT PRIMARY KEY, mtime INTEGER, hash TEXT);
CREATE TABLE IF NOT EXISTS nodes(id TEXT PRIMARY KEY, kind TEXT, name TEXT, path TEXT,
  start_line INTEGER, end_line INTEGER, signature TEXT, doc TEXT, score REAL DEFAULT 0);
CREATE TABLE IF NOT EXISTS edges(src TEXT, dst TEXT, kind TEXT);
CREATE INDEX IF NOT EXISTS idx_nodes_name ON nodes(name);
CREATE INDEX IF NOT EXISTS idx_edges_src ON edges(src);
CREATE INDEX IF NOT EXISTS idx_edges_dst ON edges(dst);
CREATE VIRTUAL TABLE IF NOT EXISTS symbol_fts USING fts5(
  name, signature, doc, id UNINDEXED, path UNINDEXED, tokenize='porter unicode61');
CREATE VIRTUAL TABLE IF NOT EXISTS symbol_fts_cjk USING fts5(
  name, signature, doc, id UNINDEXED, path UNINDEXED, tokenize='trigram');
CREATE TABLE IF NOT EXISTS raw_calls(path TEXT, caller TEXT, callee TEXT);
CREATE TABLE IF NOT EXISTS raw_imports(path TEXT, text TEXT);
CREATE TABLE IF NOT EXISTS raw_import_links(path TEXT, kind TEXT, target TEXT);
CREATE TABLE IF NOT EXISTS raw_refs(path TEXT, from_name TEXT, ref_name TEXT);
CREATE TABLE IF NOT EXISTS raw_external_deps(path TEXT, dep_name TEXT);
CREATE TABLE IF NOT EXISTS raw_trait_methods(path TEXT, trait_name TEXT, method_name TEXT, start_line INTEGER);
CREATE TABLE IF NOT EXISTS raw_impl_methods(path TEXT, trait_name TEXT, impl_type TEXT, method_name TEXT, start_line INTEGER);
CREATE INDEX IF NOT EXISTS idx_raw_calls_path ON raw_calls(path);
CREATE INDEX IF NOT EXISTS idx_raw_imports_path ON raw_imports(path);
CREATE INDEX IF NOT EXISTS idx_raw_import_links_path ON raw_import_links(path);
CREATE INDEX IF NOT EXISTS idx_raw_refs_path ON raw_refs(path);
CREATE UNIQUE INDEX IF NOT EXISTS idx_raw_external_deps_unique ON raw_external_deps(path, dep_name);
CREATE INDEX IF NOT EXISTS idx_raw_trait_methods_path ON raw_trait_methods(path);
CREATE INDEX IF NOT EXISTS idx_raw_impl_methods_path ON raw_impl_methods(path);
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);
CREATE TABLE IF NOT EXISTS node_embeddings(
  id TEXT PRIMARY KEY,
  dim INTEGER NOT NULL,
  model TEXT NOT NULL,
  embedding BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_node_embeddings_model ON node_embeddings(model);
";

/// 当前 schema 版本：v2 引入 raw_calls/raw_imports 事实表与全局边重建；
/// v3 引入 raw_trait_methods/raw_impl_methods 事实表（动态分发桥）；
/// v4 引入 raw_refs / raw_import_links / raw_external_deps（引用与依赖图）。
const SCHEMA_VERSION: &str = "4";

/// 硬排除的目录名（任何路径段命中即跳过）。
const HARD_EXCLUDES: [&str; 4] = ["target", "node_modules", ".git", "dist"];

/// SQLite 持久化的代码图存储（单线程串行；上层门面负责加锁）。
pub struct Store {
    conn: Connection,
    /// 可选语义嵌入后端；None 或查询嵌入失败时检索回落纯词法（fail-open）。
    embedder: Option<Arc<dyn EmbeddingProvider>>,
    /// 写入/查询共用的嵌入模型名；仅同模型向量参与余弦融合。
    embed_model: String,
}

/// 混合检索的分数分解（测试/诊断用）。各分量为**对总分的加性贡献**：
/// `bm25 = weight * 归一化词法分`，`cosine = (1-weight) * max(cos, 0)`，
/// `score == bm25 + cosine`。
#[derive(Debug, Clone)]
pub struct HybridHit {
    /// 命中的节点。
    pub node: Node,
    /// 归一化词法分量（`weight * 归一化 BM25`，对总分的加性贡献）。
    pub bm25: f64,
    /// 语义分量（`(1 - weight) * max(余弦, 0)`，对总分的加性贡献）。
    pub cosine: f64,
    /// 融合总分（`score == bm25 + cosine`）。
    pub score: f64,
}

fn node_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Node> {
    let kind_s: String = row.get("kind")?;
    let kind = NodeKind::parse(&kind_s).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            1,
            rusqlite::types::Type::Text,
            format!("unknown node kind: {kind_s}").into(),
        )
    })?;
    Ok(Node {
        id: row.get("id")?,
        kind,
        name: row.get("name")?,
        path: row.get("path")?,
        start_line: row.get("start_line")?,
        end_line: row.get("end_line")?,
        signature: row.get("signature")?,
        doc: row.get("doc")?,
        score: row.get("score")?,
    })
}

impl Store {
    /// 打开（或创建）数据库并建表；父目录不存在则创建。纯词法模式（无嵌入后端）。
    pub fn open(db_path: &Path) -> Result<Store, GraphError> {
        Self::open_with_embedder(db_path, None, "")
    }

    /// 是否装配了语义嵌入后端。未装配时 [`Self::search_hybrid`] 走
    /// `hybrid_fts_fallback`（结果与 `search` 存在过滤/截断差异），
    /// 上层应经 `crate::GraphIndex::search_best` 保证未装配时逐字节走 `search`。
    pub fn has_embedder(&self) -> bool {
        self.embedder.is_some()
    }

    /// 打开（或创建）数据库并建表，装配可选的语义嵌入后端（写入即嵌入 + hybrid 检索）。
    /// 嵌入不可用（None / 缺 key / 网络错）时检索回落纯词法，不阻断既有功能。
    pub fn open_with_embedder(
        db_path: &Path,
        embedder: Option<Arc<dyn EmbeddingProvider>>,
        model: &str,
    ) -> Result<Store, GraphError> {
        if let Some(parent) = db_path.parent() {
            if !parent.as_os_str().is_empty() && std::fs::create_dir_all(parent).is_err() {
                tracing::warn!(path = %parent.display(), "failed to create db parent dir");
            }
        }
        let conn = Connection::open(db_path)?;
        conn.execute_batch(SCHEMA)?;
        // 首次升级：trigram 辅助表为空时从主表回填一次。
        let cjk_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM symbol_fts_cjk", [], |r| r.get(0))?;
        if cjk_count == 0 {
            conn.execute(
                "INSERT INTO symbol_fts_cjk(name, signature, doc, id, path)
                 SELECT name, signature, doc, id, path FROM symbol_fts",
                [],
            )?;
        }
        // schema 版本核对（三态策略，与 memory store 的 ensure_schema_version 一致）：
        // - 无版本标记（旧库/新库）或**已知旧版本**（可解析为数字且 < 当前）：清空 files
        //   强制下次全量重解析，并回写当前版本；
        // - **未来版本**（可解析且 > 当前，如 '5'）或不可解析的未知版本：**不回写**、不降级、
        //   不清空，保持原版本号只读可用（避免旧二进制抹除未来库的版本簿记）。
        let version: Option<String> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        let needs_migration = match version.as_deref() {
            None => true, // 无版本标记（旧库/新库升级路径）→ 清表重索引并写当前版本
            Some(v) => match v.parse::<i64>() {
                // 仅已知旧版本（可解析且 < 当前）走清表迁移回写
                Ok(n) => n < SCHEMA_VERSION.parse::<i64>().unwrap_or(4),
                Err(_) => false, // 未知/不可解析版本：保守不回写
            },
        };
        if needs_migration {
            conn.execute("DELETE FROM files", [])?;
            conn.execute(
                "INSERT INTO meta(key, value) VALUES('schema_version', ?1)\n                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [SCHEMA_VERSION],
            )?;
        }
        Ok(Store {
            conn,
            embedder,
            embed_model: model.to_string(),
        })
    }

    /// 增量刷新：mtime/sha256 双级判定，仅重解析变更文件。
    pub fn refresh(
        &mut self,
        root: &Path,
        max_file_size: u64,
    ) -> Result<RefreshReport, GraphError> {
        let ignores = load_gitignore(root);
        let mut files: Vec<(std::path::PathBuf, String)> = Vec::new();
        collect_files(root, root, &ignores, &mut files);
        files.sort_by(|a, b| a.1.cmp(&b.1));

        let current: HashSet<&str> = files.iter().map(|(_, rel)| rel.as_str()).collect();
        let mut report = RefreshReport::default();
        let tx = self.conn.transaction()?;

        for (abs_path, rel_path) in &files {
            let lang = Lang::from_path(rel_path);
            let manifest = lang.is_none() && is_manifest(rel_path);
            if lang.is_none() && !manifest {
                continue;
            }
            let Ok(meta) = std::fs::metadata(abs_path) else {
                continue;
            };
            if meta.len() > max_file_size {
                continue;
            }
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            report.files_indexed += 1;

            let known: Option<(i64, String)> = tx
                .query_row(
                    "SELECT mtime, hash FROM files WHERE path = ?1",
                    [rel_path],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })?;
            if let Some((known_mtime, _)) = &known {
                if *known_mtime == mtime {
                    continue; // mtime 未变，视为未修改
                }
            }

            let Ok(src) = std::fs::read_to_string(abs_path) else {
                tracing::warn!(path = %rel_path, "failed to read file, skipping");
                continue;
            };
            let hash = hex::encode(Sha256::digest(src.as_bytes()));
            if let Some((_, known_hash)) = &known {
                if *known_hash == hash {
                    // 内容未变（仅 touch），只更新 mtime
                    tx.execute(
                        "UPDATE files SET mtime = ?1 WHERE path = ?2",
                        (mtime, rel_path),
                    )?;
                    continue;
                }
            }

            // 清单文件（Cargo.toml / package.json / pyproject.toml）：只写外部依赖事实。
            if manifest {
                let deps = parse_manifest_deps(rel_path, &src);
                tx.execute("DELETE FROM raw_external_deps WHERE path = ?1", [rel_path])?;
                for dep in &deps {
                    tx.execute(
                        "INSERT OR IGNORE INTO raw_external_deps(path, dep_name) VALUES(?1, ?2)",
                        (rel_path, dep),
                    )?;
                }
                report.files_reparsed += 1;
                tx.execute(
                    "INSERT INTO files(path, mtime, hash) VALUES(?1, ?2, ?3)\n                     ON CONFLICT(path) DO UPDATE SET mtime = excluded.mtime, hash = excluded.hash",
                    (rel_path, mtime, &hash),
                )?;
                continue;
            }

            // 上方 guard 已排除 lang.is_none() 且非 manifest 的文件，manifest
            // 分支也已 continue，此处 lang 保证 Some；防御性跳过而非 panic，
            // 与「不支持的语言」路径一致（文件不建索引，不崩溃）。
            let Some(lang) = lang else {
                continue;
            };
            let parsed = match parse_source(lang, rel_path, &src) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(path = %rel_path, error = %e, "parse failed, skipping");
                    continue;
                }
            };

            // 清理该文件旧数据：出入边（id 前缀为 path#）、节点、FTS 行、原始调用/导入事实
            tx.execute(
                "DELETE FROM edges WHERE src LIKE ?1 || '#%' OR dst LIKE ?1 || '#%'",
                [rel_path],
            )?;
            tx.execute("DELETE FROM nodes WHERE path = ?1", [rel_path])?;
            tx.execute("DELETE FROM symbol_fts WHERE path = ?1", [rel_path])?;
            tx.execute("DELETE FROM symbol_fts_cjk WHERE path = ?1", [rel_path])?;
            tx.execute(
                "DELETE FROM node_embeddings WHERE id LIKE ?1 || '#%'",
                [rel_path],
            )?;
            tx.execute("DELETE FROM raw_calls WHERE path = ?1", [rel_path])?;
            tx.execute("DELETE FROM raw_imports WHERE path = ?1", [rel_path])?;
            tx.execute("DELETE FROM raw_import_links WHERE path = ?1", [rel_path])?;
            tx.execute("DELETE FROM raw_refs WHERE path = ?1", [rel_path])?;
            tx.execute("DELETE FROM raw_external_deps WHERE path = ?1", [rel_path])?;
            tx.execute("DELETE FROM raw_trait_methods WHERE path = ?1", [rel_path])?;
            tx.execute("DELETE FROM raw_impl_methods WHERE path = ?1", [rel_path])?;

            let file_name = rel_path.rsplit('/').next().unwrap_or(rel_path).to_string();
            let file_node = Node {
                id: node_id(rel_path, &file_name, 0),
                kind: NodeKind::File,
                name: file_name,
                path: rel_path.clone(),
                start_line: 0,
                end_line: 0,
                signature: rel_path.clone(),
                doc: String::new(),
                score: 0.0,
            };
            insert_node(&tx, &file_node, self.embedder.as_deref(), &self.embed_model)?;
            for def in &parsed.nodes {
                insert_node(&tx, def, self.embedder.as_deref(), &self.embed_model)?;
                tx.execute(
                    "INSERT INTO edges(src, dst, kind) VALUES(?1, ?2, ?3)",
                    (&file_node.id, &def.id, EdgeKind::Contains.as_str()),
                )?;
            }
            // 持久化原始 calls/imports 事实，供后续全局重建名称级边
            for (caller, callee) in &parsed.calls {
                tx.execute(
                    "INSERT INTO raw_calls(path, caller, callee) VALUES(?1, ?2, ?3)",
                    (rel_path, caller, callee),
                )?;
            }
            for import in &parsed.imports {
                tx.execute(
                    "INSERT INTO raw_imports(path, text) VALUES(?1, ?2)",
                    (rel_path, import),
                )?;
            }
            for link in &parsed.import_links {
                tx.execute(
                    "INSERT INTO raw_import_links(path, kind, target) VALUES(?1, ?2, ?3)",
                    (rel_path, link.kind.as_str(), &link.target),
                )?;
            }
            for (from, ref_name) in &parsed.refs {
                tx.execute(
                    "INSERT INTO raw_refs(path, from_name, ref_name) VALUES(?1, ?2, ?3)",
                    (rel_path, from, ref_name),
                )?;
            }
            for (trait_name, method_name, start_line) in &parsed.trait_methods {
                tx.execute(
                    "INSERT INTO raw_trait_methods(path, trait_name, method_name, start_line)\n                     VALUES(?1, ?2, ?3, ?4)",
                    (rel_path, trait_name, method_name, start_line),
                )?;
            }
            for (trait_name, impl_type, method_name, start_line) in &parsed.impl_trait_methods {
                tx.execute(
                    "INSERT INTO raw_impl_methods(path, trait_name, impl_type, method_name, start_line)\n                     VALUES(?1, ?2, ?3, ?4, ?5)",
                    (rel_path, trait_name, impl_type, method_name, start_line),
                )?;
            }
            report.files_reparsed += 1;
            tx.execute(
                "INSERT INTO files(path, mtime, hash) VALUES(?1, ?2, ?3)\n                 ON CONFLICT(path) DO UPDATE SET mtime = excluded.mtime, hash = excluded.hash",
                (rel_path, mtime, &hash),
            )?;
        }

        // 清理磁盘上已消失文件的全部索引数据，避免幽灵实体残留
        let stale: Vec<String> = {
            let mut stmt = tx.prepare("SELECT path FROM files")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            let mut out = Vec::new();
            for row in rows {
                let path = row?;
                if !current.contains(path.as_str()) {
                    out.push(path);
                }
            }
            out
        };
        for path in &stale {
            tx.execute(
                "DELETE FROM edges WHERE src LIKE ?1 || '#%' OR dst LIKE ?1 || '#%'",
                [path],
            )?;
            tx.execute("DELETE FROM nodes WHERE path = ?1", [path])?;
            tx.execute("DELETE FROM symbol_fts WHERE path = ?1", [path])?;
            tx.execute("DELETE FROM symbol_fts_cjk WHERE path = ?1", [path])?;
            tx.execute(
                "DELETE FROM node_embeddings WHERE id LIKE ?1 || '#%'",
                [path],
            )?;
            tx.execute("DELETE FROM raw_calls WHERE path = ?1", [path])?;
            tx.execute("DELETE FROM raw_imports WHERE path = ?1", [path])?;
            tx.execute("DELETE FROM raw_import_links WHERE path = ?1", [path])?;
            tx.execute("DELETE FROM raw_refs WHERE path = ?1", [path])?;
            tx.execute("DELETE FROM raw_external_deps WHERE path = ?1", [path])?;
            tx.execute("DELETE FROM raw_trait_methods WHERE path = ?1", [path])?;
            tx.execute("DELETE FROM raw_impl_methods WHERE path = ?1", [path])?;
            tx.execute("DELETE FROM files WHERE path = ?1", [path])?;
        }

        // 全局重建 Calls/Imports 边（Contains 随节点按文件维护，不受影响）
        tx.execute(
            "DELETE FROM edges WHERE kind IN (?1, ?2)",
            (EdgeKind::Calls.as_str(), EdgeKind::Imports.as_str()),
        )?;

        // 名称级边解析：全库定义节点 name → [(id, path)]（排除 File 节点）
        let mut by_name: HashMap<String, Vec<(String, String)>> = HashMap::new();
        {
            let mut stmt = tx.prepare(
                "SELECT name, id, path FROM nodes WHERE kind != 'file'\n                 ORDER BY path, start_line",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (name, id, path) = row?;
                by_name.entry(name).or_default().push((id, path));
            }
        }
        let raw_calls: Vec<(String, String, String)> = {
            let mut stmt = tx.prepare("SELECT path, caller, callee FROM raw_calls")?;
            let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            out
        };
        for (path, caller_name, callee_name) in &raw_calls {
            let caller_id = by_name
                .get(caller_name)
                .and_then(|ids| ids.iter().find(|(_, p)| p == path).map(|(id, _)| id));
            let callee_id = by_name
                .get(callee_name)
                .and_then(|ids| ids.first().map(|(id, _)| id));
            if let (Some(src), Some(dst)) = (caller_id, callee_id) {
                tx.execute(
                    "INSERT INTO edges(src, dst, kind) VALUES(?1, ?2, ?3)",
                    (src, dst, EdgeKind::Calls.as_str()),
                )?;
            }
        }
        // 结构化 Imports 边重建：本地符号按名命中 by_name；本地文件按相对路径解析。
        let mut import_seen: HashSet<(String, String)> = HashSet::new();
        {
            let mut stmt = tx.prepare("SELECT path, kind, target FROM raw_import_links")?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            let mut links = Vec::new();
            for row in rows {
                links.push(row?);
            }
            for (path, kind, target) in links {
                let file_name = path.rsplit('/').next().unwrap_or(&path).to_string();
                let file_id = node_id(&path, &file_name, 0);
                match kind.as_str() {
                    "symbol" => {
                        if let Some((dst, _)) = by_name.get(&target).and_then(|ids| ids.first()) {
                            if import_seen.insert((file_id.clone(), dst.clone())) {
                                tx.execute(
                                    "INSERT INTO edges(src, dst, kind) VALUES(?1, ?2, ?3)",
                                    (&file_id, dst, EdgeKind::Imports.as_str()),
                                )?;
                            }
                        }
                    }
                    "file" => {
                        if let Some(dst) = resolve_file_node(&tx, &path, &target)? {
                            if import_seen.insert((file_id.clone(), dst.clone())) {
                                tx.execute(
                                    "INSERT INTO edges(src, dst, kind) VALUES(?1, ?2, ?3)",
                                    (&file_id, dst, EdgeKind::Imports.as_str()),
                                )?;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // 全局重建 Dispatch 边：trait 方法 → 同名 impl 方法（名称级动态分发桥）。
        // 老库无此边不影响既有查询；本库升级后由 raw_* 事实表重建。
        tx.execute(
            "DELETE FROM edges WHERE kind = ?1",
            [EdgeKind::Dispatch.as_str()],
        )?;
        let mut trait_method_ids: HashMap<(String, String), Vec<String>> = HashMap::new();
        {
            let mut stmt = tx.prepare(
                "SELECT tm.trait_name, tm.method_name, n.id\n                 FROM raw_trait_methods tm\n                 JOIN nodes n\n                   ON n.path = tm.path\n                  AND n.start_line = tm.start_line\n                  AND n.name = tm.method_name",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (trait_name, method_name, id) = row?;
                trait_method_ids
                    .entry((trait_name, method_name))
                    .or_default()
                    .push(id);
            }
        }
        let mut impl_method_ids: HashMap<(String, String), Vec<String>> = HashMap::new();
        {
            let mut stmt = tx.prepare(
                "SELECT im.trait_name, im.method_name, n.id\n                 FROM raw_impl_methods im\n                 JOIN nodes n\n                   ON n.path = im.path\n                  AND n.start_line = im.start_line\n                  AND n.name = im.method_name",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (trait_name, method_name, id) = row?;
                impl_method_ids
                    .entry((trait_name, method_name))
                    .or_default()
                    .push(id);
            }
        }
        let keys: Vec<(String, String)> = trait_method_ids.keys().cloned().collect();
        let mut inserted: HashSet<(String, String)> = HashSet::new();
        for key in keys {
            let Some(impl_ids) = impl_method_ids.get(&key) else {
                continue;
            };
            let trait_ids = &trait_method_ids[&key];
            for t_id in trait_ids {
                for i_id in impl_ids {
                    if inserted.insert((t_id.clone(), i_id.clone())) {
                        tx.execute(
                            "INSERT INTO edges(src, dst, kind) VALUES(?1, ?2, ?3)",
                            (t_id, i_id, EdgeKind::Dispatch.as_str()),
                        )?;
                    }
                }
            }
        }

        // 全局重建 References 边：名称级；已有 Calls 边 (src,dst) 不重复加。
        tx.execute(
            "DELETE FROM edges WHERE kind = ?1",
            [EdgeKind::References.as_str()],
        )?;
        let calls_set: HashSet<(String, String)> = {
            let mut stmt = tx.prepare("SELECT src, dst FROM edges WHERE kind = 'calls'")?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            let mut out = HashSet::new();
            for row in rows {
                out.insert(row?);
            }
            out
        };
        let raw_refs: Vec<(String, String, String)> = {
            let mut stmt = tx.prepare("SELECT path, from_name, ref_name FROM raw_refs")?;
            let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            out
        };
        let mut refs_seen: HashSet<(String, String)> = HashSet::new();
        for (path, from_name, ref_name) in &raw_refs {
            let Some(src) = by_name
                .get(from_name)
                .and_then(|ids| ids.iter().find(|(_, p)| p == path).map(|(id, _)| id))
            else {
                continue;
            };
            let Some(targets) = by_name.get(ref_name) else {
                continue;
            };
            for (dst, _) in targets {
                if dst == src {
                    continue;
                }
                if calls_set.contains(&(src.clone(), dst.clone())) {
                    continue;
                }
                if refs_seen.insert((src.clone(), dst.clone())) {
                    tx.execute(
                        "INSERT INTO edges(src, dst, kind) VALUES(?1, ?2, ?3)",
                        (src, dst, EdgeKind::References.as_str()),
                    )?;
                }
            }
        }

        report.nodes =
            tx.query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get::<_, i64>(0))? as usize;
        report.edges =
            tx.query_row("SELECT COUNT(*) FROM edges", [], |r| r.get::<_, i64>(0))? as usize;
        tx.commit()?;
        Ok(report)
    }

    /// FTS5 BM25 检索；名称精确匹配（忽略大小写）稳定置前。
    pub fn search(
        &self,
        query: &str,
        kind: Option<NodeKind>,
        limit: usize,
    ) -> Result<Vec<Node>, GraphError> {
        let scored = self.search_scored(query, kind, limit)?;
        let hits: Vec<Node> = scored.into_iter().map(|(n, _)| n).collect();
        let (mut exact, rest): (Vec<Node>, Vec<Node>) = hits
            .into_iter()
            .partition(|n| n.name.eq_ignore_ascii_case(query.trim()));
        exact.extend(rest);
        Ok(exact)
    }

    /// 混合检索（词法 ∪ 语义）：`0.5*归一化词法分 + 0.5*max(余弦, 0)` 融合排序。
    /// 无嵌入后端、查询嵌入失败或库中无同模型嵌入时回落纯词法（fail-open），
    /// 行为与 [`Self::search`] 一致。
    pub fn search_hybrid(
        &self,
        query: &str,
        kind: Option<NodeKind>,
        limit: usize,
    ) -> Result<Vec<Node>, GraphError> {
        self.search_hybrid_with_weight(query, kind, limit, 0.5)
    }

    /// 混合检索，可调词法权重 `weight`（余弦权重为 `1 - weight`）。
    pub fn search_hybrid_with_weight(
        &self,
        query: &str,
        kind: Option<NodeKind>,
        limit: usize,
        weight: f64,
    ) -> Result<Vec<Node>, GraphError> {
        Ok(self
            .search_hybrid_breakdown(query, kind, limit, weight)?
            .into_iter()
            .map(|h| h.node)
            .collect())
    }

    /// 混合检索的分数分解（测试/诊断用）。结果集/排序/总分与
    /// [`Self::search_hybrid_with_weight`] 完全一致，仅额外暴露
    /// bm25 / 余弦两个加性分量（见 [`HybridHit`]）。
    ///
    /// 查询向量在 SQL 前计算——嵌入是潜在慢 HTTP 调用，不持有连接。
    /// 失败/缺 provider → 纯词法（与 [`Self::search`] 一致）。
    pub fn search_hybrid_breakdown(
        &self,
        query: &str,
        kind: Option<NodeKind>,
        limit: usize,
        weight: f64,
    ) -> Result<Vec<HybridHit>, GraphError> {
        // 嵌入失败/缺 provider → 纯词法（fail-open）。
        let Some(qv) = self.embedder.as_deref().and_then(|p| p.embed(query).ok()) else {
            return self.hybrid_fts_fallback(query, kind, limit, weight);
        };
        let fts = self.search_scored(query, kind, limit.saturating_mul(2))?;

        // 1. 同模型嵌入全扫，算余弦（JOIN nodes 过滤已删除的孤儿向量）。
        let mut cos_map: HashMap<String, f64> = HashMap::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT e.id, e.embedding, e.dim, n.kind\n                 FROM node_embeddings e JOIN nodes n ON n.id = e.id\n                 WHERE e.model = ?1",
            )?;
            let rows = stmt.query_map([&self.embed_model], |r| {
                let id: String = r.get(0)?;
                let blob: Vec<u8> = r.get(1)?;
                let dim: i64 = r.get(2)?;
                let kind: String = r.get(3)?;
                let mut vec = Vec::with_capacity(dim as usize);
                for chunk in blob.as_chunks::<4>().0 {
                    vec.push(f32::from_le_bytes(*chunk));
                }
                Ok((id, kind, vec))
            })?;
            for row in rows {
                let (id, kind_s, vec) = row?;
                if let Some(k) = kind {
                    if kind_s != k.as_str() {
                        continue;
                    }
                }
                let c = cosine(&qv, &vec);
                if c > 0.0 {
                    cos_map.insert(id, c as f64);
                }
            }
        }

        // 2. 合并 id 集：FTS 顺序在前，语义独有命中补尾。
        let mut ids: Vec<String> = fts.iter().map(|(n, _)| n.id.clone()).collect();
        let mut seen: HashSet<String> = ids.iter().cloned().collect();
        let mut emb_only: Vec<String> = Vec::new();
        for id in cos_map.keys() {
            if seen.insert(id.clone()) {
                ids.push(id.clone());
                emb_only.push(id.clone());
            }
        }

        // 3. 节点表：FTS 命中直接复用，语义独有 id 批量补拉。
        let mut node_map: HashMap<String, Node> =
            fts.iter().map(|(n, _)| (n.id.clone(), n.clone())).collect();
        for chunk in emb_only.chunks(200) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = format!(
                "SELECT id, kind, name, path, start_line, end_line, signature, doc, score\n                 FROM nodes WHERE id IN ({placeholders})"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let refs: Vec<&str> = chunk.iter().map(|s| s.as_str()).collect();
            for r in stmt.query_map(rusqlite::params_from_iter(refs), node_from_row)? {
                let n = r?;
                node_map.insert(n.id.clone(), n);
            }
        }

        // 4. 融合打分：bm25 分已归一化到 [0,1]（越高越相关）。
        let fts_map: HashMap<String, f64> = fts.iter().map(|(n, s)| (n.id.clone(), *s)).collect();
        let mut out: Vec<HybridHit> = Vec::new();
        for id in &ids {
            let Some(node) = node_map.get(id) else {
                continue;
            };
            let bm = fts_map.get(id).copied().unwrap_or(0.0);
            let c = cos_map.get(id).copied().unwrap_or(0.0);
            let bm25 = weight * bm;
            let cosine = (1.0 - weight) * c.max(0.0);
            out.push(HybridHit {
                node: node.clone(),
                bm25,
                cosine,
                score: bm25 + cosine,
            });
        }
        Ok(Self::finalize_hybrid(out, query, limit))
    }

    /// 嵌入不可用路径：纯词法结果包装成统一分解（余弦恒 0）。
    fn hybrid_fts_fallback(
        &self,
        query: &str,
        kind: Option<NodeKind>,
        limit: usize,
        weight: f64,
    ) -> Result<Vec<HybridHit>, GraphError> {
        let hits = self.search_scored(query, kind, limit)?;
        let out = hits
            .into_iter()
            .map(|(node, bm)| {
                let bm25 = weight * bm;
                HybridHit {
                    node,
                    bm25,
                    cosine: 0.0,
                    score: bm25,
                }
            })
            .collect();
        Ok(Self::finalize_hybrid(out, query, limit))
    }

    /// 统一收尾：排序 → 剔除零分（纯语义权重下无语义来源）→ 截断 → 名称精确置前。
    fn finalize_hybrid(mut hits: Vec<HybridHit>, query: &str, limit: usize) -> Vec<HybridHit> {
        hits.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.node.id.cmp(&b.node.id))
        });
        hits.retain(|h| h.score > 0.0);
        hits.truncate(limit);
        let needle = query.trim();
        let (mut exact, rest): (Vec<HybridHit>, Vec<HybridHit>) = hits
            .into_iter()
            .partition(|h| h.node.name.eq_ignore_ascii_case(needle));
        exact.extend(rest);
        exact
    }

    /// 词法检索，返回 (Node, 归一化相关分)（`[0,1]`，越高越相关）：
    /// FTS5 BM25（负分取绝对值按最大值归一），短词走 LIKE 用 PageRank score 归一。
    fn search_scored(
        &self,
        query: &str,
        kind: Option<NodeKind>,
        limit: usize,
    ) -> Result<Vec<(Node, f64)>, GraphError> {
        let tokens: Vec<String> = query
            .split_whitespace()
            .filter(|t| !t.is_empty())
            .map(|t| t.to_string())
            .collect();
        if tokens.is_empty() {
            return Ok(Vec::new());
        }

        // 短查询（≤2 字符，含中文短词）无法走 FTS/trigram → LIKE 回退。
        if !tokens.iter().any(|t| t.chars().count() >= 3) {
            return self.search_like_scored(&tokens, kind, limit);
        }

        // 含 CJK 且存在 ≥3 字符 token → trigram 表（中文子串检索）。
        let cjk_mode = Self::has_cjk(query) && tokens.iter().any(|t| t.chars().count() >= 3);
        let table = if cjk_mode {
            "symbol_fts_cjk"
        } else {
            "symbol_fts"
        };
        let match_expr = tokens
            .iter()
            .filter(|t| !cjk_mode || t.chars().count() >= 3)
            .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" OR ");

        let mut raw: Vec<(Node, f64)> = Vec::new();
        let base = format!(
            "SELECT n.id, n.kind, n.name, n.path, n.start_line, n.end_line,\n                    n.signature, n.doc, n.score, bm25({table}) AS fts_bm25\n             FROM nodes n JOIN {table} f ON n.id = f.id\n             WHERE {table} MATCH ?1"
        );
        if let Some(k) = kind {
            let sql = format!("{base} AND n.kind = ?2 ORDER BY bm25({table}) LIMIT ?3");
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map((&match_expr, k.as_str(), limit as i64), |row| {
                let node = node_from_row(row)?;
                let s: f64 = row.get("fts_bm25")?;
                Ok((node, s))
            })?;
            for row in rows {
                raw.push(row?);
            }
        } else {
            let sql = format!("{base} ORDER BY bm25({table}) LIMIT ?2");
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map((&match_expr, limit as i64), |row| {
                let node = node_from_row(row)?;
                let s: f64 = row.get("fts_bm25")?;
                Ok((node, s))
            })?;
            for row in rows {
                raw.push(row?);
            }
        }
        // bm25 负分：绝对值越大越相关 → 归一化到 [0,1]。
        let best = raw.iter().map(|(_, s)| *s).fold(f64::INFINITY, f64::min);
        let denom = if best.is_finite() {
            (-best).max(0.0)
        } else {
            0.0
        };
        let denom = if denom > 0.0 { denom } else { 1.0 };
        Ok(raw
            .into_iter()
            .map(|(n, s)| (n, ((-s).max(0.0)) / denom))
            .collect())
    }

    /// 是否含 CJK 字符（决定走 trigram 表）。
    fn has_cjk(text: &str) -> bool {
        text.chars().any(|c| {
            matches!(c as u32,
                0x3000..=0x303F | 0x3400..=0x4DBF | 0x4E00..=0x9FFF
                | 0xF900..=0xFAFF | 0xFF00..=0xFFEF)
        })
    }

    /// LIKE 回退：短词（1-2 字符）做子串匹配（NOCASE），按 PageRank score 归一化排序。
    fn search_like_scored(
        &self,
        tokens: &[String],
        kind: Option<NodeKind>,
        limit: usize,
    ) -> Result<Vec<(Node, f64)>, GraphError> {
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let mut clauses: Vec<String> = Vec::new();
        for (i, t) in tokens.iter().enumerate() {
            let n = i + 1;
            clauses.push(format!(
                "(n.name LIKE ?{n} COLLATE NOCASE OR n.signature LIKE ?{n} COLLATE NOCASE \
                 OR n.doc LIKE ?{n} COLLATE NOCASE)"
            ));
            params.push(Box::new(format!("%{t}%")));
        }
        let mut sql = format!(
            "SELECT n.id, n.kind, n.name, n.path, n.start_line, n.end_line,\n                    n.signature, n.doc, n.score\n             FROM nodes n WHERE {}",
            clauses.join(" OR ")
        );
        if let Some(k) = kind {
            sql.push_str(" AND n.kind = ?");
            params.push(Box::new(k.as_str().to_string()));
        }
        let limit_idx = params.len() + 1;
        sql.push_str(&format!(" ORDER BY n.score DESC LIMIT ?{limit_idx}"));
        params.push(Box::new(limit as i64));

        let mut stmt = self.conn.prepare(&sql)?;
        let raw_params = rusqlite::params_from_iter(params.iter().map(|p| p.as_ref()));
        let mut hits: Vec<(Node, f64)> = Vec::new();
        let rows = stmt.query_map(raw_params, |row| {
            let node = node_from_row(row)?;
            let s: f64 = row.get("score")?;
            Ok((node, s))
        })?;
        for row in rows {
            hits.push(row?);
        }
        let max_s = hits.iter().map(|(_, s)| *s).fold(0.0, f64::max);
        let denom = if max_s > 0.0 { max_s } else { 1.0 };
        Ok(hits.into_iter().map(|(n, s)| (n, s / denom)).collect())
    }

    /// BFS 邻居：最多 hops 层，去重、排除起点，按 score 降序。
    pub fn neighbors(
        &self,
        id: &str,
        edge_kinds: &[EdgeKind],
        dir: Direction,
        hops: usize,
    ) -> Result<Vec<Node>, GraphError> {
        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(id.to_string());
        let mut frontier: Vec<String> = vec![id.to_string()];
        let mut found: Vec<String> = Vec::new();

        for _ in 0..hops {
            let mut next: Vec<String> = Vec::new();
            if matches!(dir, Direction::Callers | Direction::Both) {
                for cand in self.collect_neighbors(&frontier, edge_kinds, true)? {
                    if visited.insert(cand.clone()) {
                        found.push(cand.clone());
                        next.push(cand);
                    }
                }
            }
            if matches!(dir, Direction::Callees | Direction::Both) {
                for cand in self.collect_neighbors(&frontier, edge_kinds, false)? {
                    if visited.insert(cand.clone()) {
                        found.push(cand.clone());
                        next.push(cand);
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }

        let mut nodes: Vec<Node> = Vec::new();
        for nid in &found {
            if let Some(node) = self.get(nid)? {
                nodes.push(node);
            }
        }
        nodes.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(nodes)
    }

    /// 批量取一跳邻居 id：`callers=true` 沿入边（`dst IN (...)`），否则沿出边
    /// （`src IN (...)`）。frontier 分块 500 一次 IN 查询，规避 SQLite 变量数
    /// 上限（默认 999）；非空 `edge_kinds` 时 kind 过滤下沉到 SQL。
    fn collect_neighbors(
        &self,
        frontier: &[String],
        edge_kinds: &[EdgeKind],
        callers: bool,
    ) -> Result<Vec<String>, GraphError> {
        let (col, other) = if callers {
            ("dst", "src")
        } else {
            ("src", "dst")
        };
        let kind_sql = if edge_kinds.is_empty() {
            String::new()
        } else {
            let placeholders = vec!["?"; edge_kinds.len()].join(",");
            format!(" AND kind IN ({placeholders})")
        };
        let mut out: Vec<String> = Vec::new();
        for chunk in frontier.chunks(500) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = format!(
                "SELECT {other}, kind FROM edges WHERE {col} IN ({placeholders}){kind_sql}"
            );
            let mut params: Vec<&str> = Vec::new();
            for id in chunk {
                params.push(id.as_str());
            }
            if !edge_kinds.is_empty() {
                for k in edge_kinds {
                    params.push(k.as_str());
                }
            }
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
                row.get::<_, String>(0)
            })?;
            for row in rows {
                out.push(row?);
            }
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    /// 深度受限的路径追踪：从 `id` 出发沿 `edge_kinds` 按方向 DFS，
    /// 返回完整链（callers 方向归一为「源 → … → 目标」）。深度/路径数超限置截断标记。
    pub fn trace_paths(
        &self,
        id: &str,
        edge_kinds: &[EdgeKind],
        dir: Direction,
        max_hops: usize,
    ) -> Result<TraceResult, GraphError> {
        let edges = self.edges_of_kinds(edge_kinds)?;
        let mut expander = TraceExpander::new(&edges, edge_kinds, dir, max_hops);
        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(id.to_string());
        expander.dfs(id, &mut vec![id.to_string()], &mut visited);
        let TraceExpander {
            paths, truncated, ..
        } = expander;

        let mut out_paths = Vec::new();
        for mut p in paths {
            if dir == Direction::Callers {
                p.reverse();
            }
            let mut nodes = Vec::new();
            for nid in &p {
                if let Some(node) = self.get(nid)? {
                    nodes.push(node);
                }
            }
            if !nodes.is_empty() {
                out_paths.push(nodes);
            }
        }
        Ok(TraceResult {
            paths: out_paths,
            truncated,
        })
    }

    /// 按名称精确查找节点。
    pub fn find_by_name(&self, name: &str) -> Result<Vec<Node>, GraphError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, name, path, start_line, end_line, signature, doc, score\n             FROM nodes WHERE name = ?1 ORDER BY path, start_line",
        )?;
        let rows = stmt.query_map([name], node_from_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// 按 id 取单个节点。
    pub fn get(&self, id: &str) -> Result<Option<Node>, GraphError> {
        let result = self.conn.query_row(
            "SELECT id, kind, name, path, start_line, end_line, signature, doc, score\n             FROM nodes WHERE id = ?1",
            [id],
            node_from_row,
        );
        match result {
            Ok(node) => Ok(Some(node)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Contains 出边指向的子节点。
    pub fn children(&self, id: &str) -> Result<Vec<Node>, GraphError> {
        let mut stmt = self.conn.prepare(
            "SELECT n.id, n.kind, n.name, n.path, n.start_line, n.end_line,\n                    n.signature, n.doc, n.score\n             FROM edges e JOIN nodes n ON n.id = e.dst\n             WHERE e.src = ?1 AND e.kind = 'contains'\n             ORDER BY n.start_line",
        )?;
        let rows = stmt.query_map([id], node_from_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Contains 入边指向的直接父节点（类型/模块归属）。
    pub fn parents(&self, id: &str) -> Result<Vec<Node>, GraphError> {
        let mut stmt = self.conn.prepare(
            "SELECT n.id, n.kind, n.name, n.path, n.start_line, n.end_line,\n                    n.signature, n.doc, n.score\n             FROM edges e JOIN nodes n ON n.id = e.src\n             WHERE e.dst = ?1 AND e.kind = 'contains'\n             ORDER BY n.start_line",
        )?;
        let rows = stmt.query_map([id], node_from_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// 按相对路径取文件节点 id（依赖图文件→文件边用）。
    pub fn file_node(&self, path: &str) -> Result<Option<String>, GraphError> {
        let result = self.conn.query_row(
            "SELECT id FROM nodes WHERE path = ?1 AND kind = 'file'",
            [path],
            |r| r.get(0),
        );
        match result {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// 全库外部依赖事实（path=清单文件路径, dep_name）。
    pub fn external_deps(&self) -> Result<Vec<(String, String)>, GraphError> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, dep_name FROM raw_external_deps ORDER BY dep_name, path")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// 某源码文件所属的最近清单（如 src/lib.rs → 根 Cargo.toml）的外部依赖。
    pub fn external_deps_for_file(&self, file_path: &str) -> Result<Vec<String>, GraphError> {
        let all = self.external_deps()?;
        // 找最深（目录层级最多）且是 file_path 祖先的清单，收集其全部依赖。
        let mut best_depth: Option<usize> = None;
        let mut out: Vec<String> = Vec::new();
        for (manifest, dep) in &all {
            let dir = match manifest.rsplit_once('/') {
                Some((d, _)) if !d.is_empty() => d.to_string(),
                _ => String::new(),
            };
            let is_ancestor = if dir.is_empty() {
                true
            } else {
                file_path.starts_with(&format!("{dir}/"))
            };
            if !is_ancestor {
                continue;
            }
            let depth = dir.split('/').filter(|p| !p.is_empty()).count();
            match best_depth {
                None => {
                    best_depth = Some(depth);
                    out.push(dep.clone());
                }
                Some(d) if depth == d => out.push(dep.clone()),
                Some(d) if depth > d => {
                    best_depth = Some(depth);
                    out.clear();
                    out.push(dep.clone());
                }
                Some(_) => {}
            }
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    /// 全部节点。
    pub fn all_nodes(&self) -> Result<Vec<Node>, GraphError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, name, path, start_line, end_line, signature, doc, score FROM nodes",
        )?;
        let rows = stmt.query_map([], node_from_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// 全部边。
    pub fn all_edges(&self) -> Result<Vec<EdgeRec>, GraphError> {
        let mut stmt = self.conn.prepare("SELECT src, dst, kind FROM edges")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (src, dst, kind_s) = row?;
            if let Some(kind) = EdgeKind::parse(&kind_s) {
                out.push(EdgeRec { src, dst, kind });
            }
        }
        Ok(out)
    }

    /// 按边类型集合取全部边：空集合 = 全部边（等价 [`Self::all_edges`]），
    /// 非空 = 单次 `WHERE kind IN (...)` 查询（避免全表扫描后内存过滤）。
    fn edges_of_kinds(&self, edge_kinds: &[EdgeKind]) -> Result<Vec<EdgeRec>, GraphError> {
        if edge_kinds.is_empty() {
            return self.all_edges();
        }
        let placeholders = vec!["?"; edge_kinds.len()].join(",");
        let sql = format!("SELECT src, dst, kind FROM edges WHERE kind IN ({placeholders})");
        let mut stmt = self.conn.prepare(&sql)?;
        let params: Vec<&str> = edge_kinds.iter().map(|k| k.as_str()).collect();
        let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (src, dst, kind_s) = row?;
            if let Some(kind) = EdgeKind::parse(&kind_s) {
                out.push(EdgeRec { src, dst, kind });
            }
        }
        Ok(out)
    }

    /// 沿 Contains 边向上递归收集全部祖先节点名（单条递归 CTE，深度上限 32）。
    /// `resolve` 限定名匹配用；替代逐跳 `parents()` 查询消除 N+1。
    ///
    /// 语义拓宽：递归 CTE 的 UNION ALL 对每个父节点继续向上展开，收集**所有**
    /// 父路径上的祖先——多父扇出（嵌套模块/多继承下同一节点经不同父链可达的
    /// 全部祖先）都会计入，而非旧实现的单父链。多父场景下这是更正确的行为，
    /// 不只是单纯的 N+1 查询优化；调用方（resolve）按此语义匹配限定名。
    pub(crate) fn ancestor_names(&self, id: &str) -> Result<Vec<String>, GraphError> {
        let mut stmt = self.conn.prepare(
            "WITH RECURSIVE ancestor_names(id, name, depth) AS (\n\
             SELECT n.id, n.name, 1\n\
             FROM edges e JOIN nodes n ON n.id = e.src\n\
             WHERE e.dst = ?1 AND e.kind = 'contains'\n\
             UNION ALL\n\
             SELECT n.id, n.name, a.depth + 1\n\
             FROM ancestor_names a\n\
             JOIN edges e ON e.dst = a.id AND e.kind = 'contains'\n\
             JOIN nodes n ON n.id = e.src\n\
             WHERE a.depth < 32\n\
             )\n\
             SELECT name FROM ancestor_names",
        )?;
        let rows = stmt.query_map([id], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// 批量更新 nodes.score（单事务提交，避免每条 UPDATE 独立事务开销）。
    pub fn set_scores(&self, scores: &[(String, f64)]) -> Result<(), GraphError> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare("UPDATE nodes SET score = ?1 WHERE id = ?2")?;
            for (id, score) in scores {
                stmt.execute((score, id))?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// 当前嵌入模型下已写入嵌入的节点数（诊断/回填核对用）。
    pub fn embedding_count(&self) -> Result<usize, GraphError> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM node_embeddings WHERE model = ?1",
            [&self.embed_model],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }
}

fn insert_node(
    conn: &Connection,
    node: &Node,
    embedder: Option<&dyn EmbeddingProvider>,
    model: &str,
) -> Result<(), GraphError> {
    conn.execute(
        "INSERT OR REPLACE INTO nodes(id, kind, name, path, start_line, end_line, signature, doc, score)\n         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        (
            &node.id,
            node.kind.as_str(),
            &node.name,
            &node.path,
            node.start_line,
            node.end_line,
            &node.signature,
            &node.doc,
            node.score,
        ),
    )?;
    conn.execute(
        "INSERT INTO symbol_fts(name, signature, doc, id, path) VALUES(?1, ?2, ?3, ?4, ?5)",
        (&node.name, &node.signature, &node.doc, &node.id, &node.path),
    )?;
    conn.execute(
        "INSERT INTO symbol_fts_cjk(name, signature, doc, id, path) VALUES(?1, ?2, ?3, ?4, ?5)",
        (&node.name, &node.signature, &node.doc, &node.id, &node.path),
    )?;
    // 写入即嵌入（仅符号节点；File 只有路径名，无语义价值）。
    // 嵌入失败跳过——fail-open，下次刷新该文件时自愈重试。
    if let Some(emb) = embedder {
        if !matches!(node.kind, NodeKind::File) {
            let text = format!("{}\n{}\n{}", node.name, node.signature, node.doc);
            if let Ok(vec) = emb.embed(&text) {
                let blob: Vec<u8> = vec.iter().flat_map(|f| f.to_le_bytes()).collect();
                conn.execute(
                    "INSERT INTO node_embeddings(id, dim, model, embedding) VALUES(?1, ?2, ?3, ?4)\n                     ON CONFLICT(id) DO UPDATE SET dim = excluded.dim, model = excluded.model, embedding = excluded.embedding",
                    (&node.id, vec.len() as i64, model, blob.as_slice()),
                )?;
            } else {
                tracing::debug!(id = %node.id, "graph node embedding failed; node stays lexical-only");
            }
        }
    }
    Ok(())
}

/// 读 root/.gitignore，返回粗匹配模式（去掉注释/空行与首尾斜杠）。
fn load_gitignore(root: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(root.join(".gitignore")) else {
        return Vec::new();
    };
    text.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.trim_matches('/').to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// 递归收集 (绝对路径, rel_path)；rel_path 用 `/` 分隔。
fn collect_files(
    dir: &Path,
    root: &Path,
    ignores: &[String],
    out: &mut Vec<(std::path::PathBuf, String)>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let rel = match path.strip_prefix(root) {
            Ok(r) => r
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/"),
            Err(_) => continue,
        };
        if HARD_EXCLUDES.contains(&name.as_str()) {
            continue;
        }
        if ignores.iter().any(|pat| rel.contains(pat.as_str())) {
            continue;
        }
        if path.is_dir() {
            collect_files(&path, root, ignores, out);
        } else if path.is_file() {
            out.push((path, rel));
        }
    }
}

/// 清单文件名（外部依赖来源）。
fn is_manifest(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    matches!(
        name,
        "Cargo.toml" | "package.json" | "pyproject.toml" | "go.mod"
    )
}

/// 解析清单文件的外部依赖名（轻量行级/serde_json，不引入新依赖）。
fn parse_manifest_deps(path: &str, src: &str) -> Vec<String> {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name {
        "Cargo.toml" => parse_cargo_deps(src),
        "package.json" => parse_package_json_deps(src),
        "pyproject.toml" => parse_pyproject_deps(src),
        "go.mod" => parse_go_mod_deps(src),
        _ => Vec::new(),
    }
}

/// go.mod：require 段的 module path（支持块式与单行 require）。
fn parse_go_mod_deps(src: &str) -> Vec<String> {
    let mut deps = Vec::new();
    let mut in_require_block = false;
    for line in src.lines() {
        let t = line.trim();
        // 块起始：`require (` 或其行尾注释形态（gofmt 不产但合法，G-L4），
        // trim 后为空或以 `//` 开头即视为块首。
        if let Some(tail) = t.strip_prefix("require (") {
            let tail = tail.trim();
            if tail.is_empty() || tail.starts_with("//") {
                in_require_block = true;
                continue;
            }
        }
        if in_require_block {
            if t == ")" {
                in_require_block = false;
                continue;
            }
            if let Some(path) = t.split_whitespace().next() {
                if !path.is_empty() && !path.starts_with("//") {
                    deps.push(path.to_string());
                }
            }
            continue;
        }
        // 单行 require：require module version
        if let Some(rest) = t.strip_prefix("require ") {
            if let Some(path) = rest.split_whitespace().next() {
                if !path.is_empty() && !path.starts_with('(') {
                    deps.push(path.to_string());
                }
            }
        }
    }
    deps
}

fn parse_cargo_deps(src: &str) -> Vec<String> {
    let mut deps = Vec::new();
    let mut in_deps = false;
    for line in src.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_deps = matches!(
                t,
                "[dependencies]" | "[dev-dependencies]" | "[build-dependencies]"
            );
            continue;
        }
        if in_deps {
            if let Some(eq) = t.find('=') {
                let name = t[..eq].trim();
                if !name.is_empty() && !name.starts_with('[') {
                    deps.push(name.to_string());
                }
            }
        }
    }
    deps
}

fn parse_package_json_deps(src: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(src) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for key in [
        "dependencies",
        "devDependencies",
        "peerDependencies",
        "optionalDependencies",
    ] {
        if let Some(obj) = v.get(key).and_then(|x| x.as_object()) {
            out.extend(obj.keys().cloned());
        }
    }
    out
}

fn parse_pyproject_deps(src: &str) -> Vec<String> {
    let mut deps = Vec::new();
    let mut section = String::new();
    let mut in_project_deps_list = false;
    for line in src.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            section = t.trim_matches(['[', ']']).to_string();
            in_project_deps_list = false;
            continue;
        }
        if section == "project" {
            if t.contains("dependencies") || in_project_deps_list {
                in_project_deps_list = true;
                let mut rest = t;
                while let Some(start) = rest.find('"') {
                    let after = &rest[start + 1..];
                    match after.find('"') {
                        Some(end) => {
                            let name = &after[..end];
                            if !name.trim().is_empty() {
                                deps.push(name.to_string());
                            }
                            rest = &after[end + 1..];
                        }
                        None => break,
                    }
                }
                if t.contains(']') {
                    in_project_deps_list = false;
                }
            }
        } else if section.starts_with("tool.poetry.dependencies") {
            if let Some(eq) = t.find('=') {
                let name = t[..eq].trim();
                if name != "python" && !name.is_empty() {
                    deps.push(name.to_string());
                }
            }
        }
    }
    deps
}

/// 规范化相对路径：去掉 `.`/空段，`..` 回退一级。
fn normalize_rel_path(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

/// 把相对 specifier 解析为索引中的文件节点 id（补常见扩展名探测）。
fn resolve_file_node(
    tx: &rusqlite::Transaction<'_>,
    path: &str,
    spec: &str,
) -> Result<Option<String>, GraphError> {
    let dir = match path.rsplit_once('/') {
        Some((d, _)) if !d.is_empty() => d.to_string(),
        _ => ".".to_string(),
    };
    let base = normalize_rel_path(&format!("{dir}/{spec}"));
    let mut candidates = vec![base.clone()];
    let has_ext = base.rsplit('/').next().unwrap_or("").contains('.');
    if !has_ext {
        for ext in ["ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "rs", "go"] {
            candidates.push(format!("{base}.{ext}"));
        }
    }
    for cand in candidates {
        let found: Option<String> = tx
            .query_row(
                "SELECT id FROM nodes WHERE path = ?1 AND kind = 'file'",
                [&cand],
                |r| r.get(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        if let Some(id) = found {
            return Ok(Some(id));
        }
    }
    // 目录型导入（Go `./pkg` 指包目录、JS `./util` 指目录入口）：取该目录下
    // 按路径排序的首个源码文件作为代表节点（确定性，不依赖目录遍历顺序）。
    if !base.is_empty() {
        let found: Option<String> = tx
            .query_row(
                "SELECT id FROM nodes WHERE kind = 'file' AND path GLOB ?1 || '/*' \
                 ORDER BY path LIMIT 1",
                [&base],
                |r| r.get(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        if let Some(id) = found {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::EdgeKind;
    use deepseeknova_core::DeepseeknovaError;
    use tempfile::tempdir;

    fn write(root: &std::path::Path, rel: &str, body: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn full_then_incremental_refresh() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/a.rs",
            "pub fn alpha() { beta(); }\npub fn beta() {}\n",
        );
        write(root, "src/b.rs", "pub fn gamma() {}\n");
        let mut store = Store::open(&root.join(".deepseeknova/graph.db")).unwrap();

        let n1 = store.refresh(root, 1_048_576).unwrap();
        assert!(n1.files_indexed >= 2);
        let beta_id = store.find_by_name("beta").unwrap()[0].id.clone();

        let callers = store
            .neighbors(&beta_id, &[EdgeKind::Calls], Direction::Callers, 1)
            .unwrap();
        assert!(callers.iter().any(|n| n.name == "alpha"));

        std::thread::sleep(std::time::Duration::from_millis(1100));
        write(root, "src/b.rs", "pub fn gamma() {}\npub fn delta() {}\n");
        let n2 = store.refresh(root, 1_048_576).unwrap();
        assert_eq!(n2.files_reparsed, 1);
        assert_eq!(store.find_by_name("beta").unwrap()[0].id, beta_id);
        assert!(!store.find_by_name("delta").unwrap().is_empty());
    }

    #[test]
    fn incremental_preserves_cross_file_caller_edges() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        // a.rs 调用 b.rs 的 beta；随后只改 b.rs（新增函数，beta 行号后移）
        write(root, "src/a.rs", "pub fn alpha() { beta(); }\n");
        write(root, "src/b.rs", "pub fn beta() {}\n");
        let mut store = Store::open(&root.join(".deepseeknova/graph.db")).unwrap();
        store.refresh(root, 1_048_576).unwrap();

        // 首建：alpha 是 beta 的 caller
        let beta_id = store.find_by_name("beta").unwrap()[0].id.clone();
        let callers = store
            .neighbors(&beta_id, &[EdgeKind::Calls], Direction::Callers, 1)
            .unwrap();
        assert!(
            callers.iter().any(|n| n.name == "alpha"),
            "alpha should call beta after full build"
        );

        // 只改 b.rs：在 beta 前插一个函数使 beta 行号后移（node id 变化）
        std::thread::sleep(std::time::Duration::from_millis(1100));
        write(root, "src/b.rs", "pub fn prelude() {}\npub fn beta() {}\n");
        let r = store.refresh(root, 1_048_576).unwrap();
        assert_eq!(r.files_reparsed, 1, "only b.rs reparsed");

        // 增量后：alpha→beta 的跨文件 caller 边必须仍然存在（bug 会导致丢失）
        let beta_id2 = store.find_by_name("beta").unwrap()[0].id.clone();
        let callers2 = store
            .neighbors(&beta_id2, &[EdgeKind::Calls], Direction::Callers, 1)
            .unwrap();
        assert!(
            callers2.iter().any(|n| n.name == "alpha"),
            "cross-file caller alpha->beta must survive incremental refresh of b.rs"
        );
    }

    #[test]
    fn deleted_file_is_purged_from_index() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(root, "src/keep.rs", "pub fn keeper() {}\n");
        write(root, "src/gone.rs", "pub fn ghost() {}\n");
        let mut store = Store::open(&root.join(".deepseeknova/graph.db")).unwrap();
        store.refresh(root, 1_048_576).unwrap();
        assert!(
            !store.find_by_name("ghost").unwrap().is_empty(),
            "ghost indexed initially"
        );

        // 删除 gone.rs 后刷新
        std::fs::remove_file(root.join("src/gone.rs")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        store.refresh(root, 1_048_576).unwrap();

        assert!(
            store.find_by_name("ghost").unwrap().is_empty(),
            "ghost must be purged after its file is deleted"
        );
        assert!(
            !store.find_by_name("keeper").unwrap().is_empty(),
            "keeper remains"
        );
    }

    #[test]
    fn fts_search_ranks_name_match() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/a.rs",
            "/// permission gate\npub struct PermissionGate {}\npub fn unrelated() {}\n",
        );
        let mut store = Store::open(&root.join(".deepseeknova/graph.db")).unwrap();
        store.refresh(root, 1_048_576).unwrap();
        let hits = store.search("PermissionGate", None, 10).unwrap();
        assert_eq!(hits[0].name, "PermissionGate");
    }

    #[test]
    fn fts_search_finds_chinese_doc_via_trigram() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/zh.rs",
            "/// 处理路径分隔符归一化\npub fn normalize_path() {}\n",
        );
        let mut store = Store::open(&root.join(".deepseeknova/graph.db")).unwrap();
        store.refresh(root, 1_048_576).unwrap();

        let hits = store.search("路径分隔符", None, 10).unwrap();
        assert!(
            hits.iter().any(|n| n.name == "normalize_path"),
            "trigram 应命中中文 doc 中的函数实体"
        );
    }

    #[test]
    fn fts_search_short_english_falls_back_to_like() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/ai.rs",
            "/// Go bindings for the AI runtime\npub fn go_runtime() {}\n",
        );
        let mut store = Store::open(&root.join(".deepseeknova/graph.db")).unwrap();
        store.refresh(root, 1_048_576).unwrap();

        // "go" 仅 2 字符：LIKE NOCASE 子串匹配（旧 unicode61 无前缀匹配不稳）。
        let hits = store.search("go", None, 10).unwrap();
        assert!(
            hits.iter().any(|n| n.name == "go_runtime"),
            "短词 LIKE 回退应命中 go_runtime"
        );
    }

    #[test]
    fn fts_search_short_chinese_falls_back_to_like() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/zh2.rs",
            "/// 验证命令超时处理\npub fn verify_timeout() {}\n",
        );
        let mut store = Store::open(&root.join(".deepseeknova/graph.db")).unwrap();
        store.refresh(root, 1_048_576).unwrap();

        // "超时" 2 字中文：trigram 无法匹配，走 LIKE 回退。
        let hits = store.search("超时", None, 10).unwrap();
        assert!(
            hits.iter().any(|n| n.name == "verify_timeout"),
            "中文短词 LIKE 回退应命中"
        );
    }

    #[test]
    fn fts_multi_word_query_uses_or_semantics_and_ranks_both_highest() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/mw.rs",
            "/// alpha\npub fn only_alpha() {}\n\
             /// beta\npub fn only_beta() {}\n\
             /// alpha beta\npub fn both_terms() {}\n",
        );
        let mut store = Store::open(&root.join(".deepseeknova/graph.db")).unwrap();
        store.refresh(root, 1_048_576).unwrap();

        // OR 语义：任一 token 命中即返回；双词命中文档凭 BM25 加性居首。
        let hits = store.search("alpha beta", None, 10).unwrap();
        let names: Vec<&str> = hits.iter().map(|n| n.name.as_str()).collect();
        assert!(
            names.contains(&"only_alpha"),
            "OR 语义应返回单词命中：{names:?}"
        );
        assert!(
            names.contains(&"only_beta"),
            "OR 语义应返回单词命中：{names:?}"
        );
        assert_eq!(names[0], "both_terms", "双词文档应凭 BM25 居首：{names:?}");
    }

    #[test]
    fn fts_underscore_token_positive_and_mid_token_negative() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/tok.rs",
            "pub fn build_agent() {}\npub fn normalize_path() {}\n",
        );
        let mut store = Store::open(&root.join(".deepseeknova/graph.db")).unwrap();
        store.refresh(root, 1_048_576).unwrap();

        // unicode61 把下划线当分隔符：整 token "build" 命中 "build_agent"。
        let hits = store.search("build", None, 10).unwrap();
        assert!(
            hits.iter().any(|n| n.name == "build_agent"),
            "查询 build 应命中 build_agent：{:?}",
            hits.iter().map(|n| &n.name).collect::<Vec<_>>()
        );
        // 边界：FTS 按整 token 匹配，"norm" 不是 "normalize" 的 token → 空。
        // （≥3 字符的部分英文词不走 LIKE 回退，此为当前检索边界。）
        let empty = store.search("norm", None, 10).unwrap();
        assert!(
            empty.is_empty(),
            "部分英文词（≥3 字符）不得经 FTS 命中：{:?}",
            empty.iter().map(|n| &n.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn fts_case_insensitive_query() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/ci.rs",
            "/// permission gate\npub struct PermissionGate {}\n",
        );
        let mut store = Store::open(&root.join(".deepseeknova/graph.db")).unwrap();
        store.refresh(root, 1_048_576).unwrap();

        let hits = store.search("PERMISSIONGATE", None, 10).unwrap();
        assert_eq!(hits[0].name, "PermissionGate");
        // 多词 + 大小写混用
        let multi = store.search("PERMISSION gate", None, 10).unwrap();
        assert!(
            multi.iter().any(|n| n.name == "PermissionGate"),
            "{:?}",
            multi.iter().map(|n| &n.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn fts_kind_filter_limits_results() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/k.rs",
            "pub struct Gate {}\npub fn gate_check() {}\n",
        );
        let mut store = Store::open(&root.join(".deepseeknova/graph.db")).unwrap();
        store.refresh(root, 1_048_576).unwrap();

        let funcs = store.search("gate", Some(NodeKind::Function), 10).unwrap();
        let func_names: Vec<&str> = funcs.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(func_names, vec!["gate_check"], "{func_names:?}");
        let structs = store.search("gate", Some(NodeKind::Struct), 10).unwrap();
        let struct_names: Vec<&str> = structs.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(struct_names, vec!["Gate"], "{struct_names:?}");
    }

    #[test]
    fn search_empty_or_whitespace_query_returns_empty() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(root, "src/e.rs", "pub fn something() {}\n");
        let mut store = Store::open(&root.join(".deepseeknova/graph.db")).unwrap();
        store.refresh(root, 1_048_576).unwrap();

        for q in ["", "   "] {
            let hits = store.search(q, None, 10).unwrap();
            assert!(hits.is_empty(), "query {q:?} 应返回空");
        }
    }

    #[test]
    fn cjk_trigram_query_does_not_false_positive() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(root, "src/zh.rs", "/// 文件读写操作\npub fn file_io() {}\n");
        let mut store = Store::open(&root.join(".deepseeknova/graph.db")).unwrap();
        store.refresh(root, 1_048_576).unwrap();

        // 与库中文本无共享 trigram 的中文查询 → 空，不得误命中。
        let hits = store.search("网络请求重试", None, 10).unwrap();
        assert!(
            hits.is_empty(),
            "无共享 trigram 不得误命中：{:?}",
            hits.iter().map(|n| &n.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn parses_go_mod_deps_block_and_single_line() {
        let src = "module example.com/app\n\
\ngo 1.21\n\
\n\
require (\n\
\tgithub.com/foo/bar v1.2.3\n\
\tgolang.org/x/sync v0.1.0\n\
)\n\
\n\
require github.com/single/dep v1.0.0\n\
\n\
// 注释行不产生依赖\n";
        let deps = parse_go_mod_deps(src);
        assert!(deps.contains(&"github.com/foo/bar".to_string()), "{deps:?}");
        assert!(deps.contains(&"golang.org/x/sync".to_string()), "{deps:?}");
        assert!(
            deps.contains(&"github.com/single/dep".to_string()),
            "{deps:?}"
        );
        assert_eq!(deps.len(), 3, "注释/module/go 行不解析为依赖：{deps:?}");
    }

    #[test]
    fn go_mod_block_with_trailing_comment_and_negative_blocks() {
        // G-L4：`require ( // 尾注释` 形态（gofmt 不产但人可能写）必须识别为
        // 块起始，块内依赖不静默丢失。
        let src = "module example.com/app\n\n\
require ( // 依赖块\n\
\tgithub.com/foo/bar v1.2.3\n\
\tgolang.org/x/sync v0.1.0\n\
)\n\n\
replace (\n\
\texample.com/old => example.com/new v1.0.0\n\
)\n\n\
exclude (\n\
\tgithub.com/skip/me v1.0.0\n\
)\n\n\
require github.com/single/dep v1.0.0\n";
        let deps = parse_go_mod_deps(src);
        assert!(deps.contains(&"github.com/foo/bar".to_string()), "{deps:?}");
        assert!(deps.contains(&"golang.org/x/sync".to_string()), "{deps:?}");
        assert!(
            deps.contains(&"github.com/single/dep".to_string()),
            "{deps:?}"
        );
        // 负例：replace/exclude 段不进入依赖。
        assert!(
            !deps
                .iter()
                .any(|d| { d.contains("old") || d.contains("new") || d.contains("skip") }),
            "replace/exclude 段不得解析为依赖：{deps:?}"
        );
        assert_eq!(deps.len(), 3, "{deps:?}");
    }

    #[test]
    fn go_project_indexes_external_deps_from_go_mod() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "go.mod",
            "module example.com/app\n\ngo 1.21\n\nrequire (\n\tgithub.com/foo/bar v1.2.3\n)\n",
        );
        write(
            root,
            "main.go",
            "package main\n\nimport \"fmt\"\n\nfunc main() { fmt.Println(\"hi\") }\n",
        );
        let mut store = Store::open(&root.join(".deepseeknova/graph.db")).unwrap();
        store.refresh(root, 1_048_576).unwrap();

        let deps = store.external_deps().unwrap();
        assert!(
            deps.iter()
                .any(|(path, dep)| dep == "github.com/foo/bar" && path.ends_with("go.mod")),
            "{deps:?}"
        );
        // main.go 的实体应被索引
        let hits = store.find_by_name("main").unwrap();
        assert!(!hits.is_empty());
    }

    #[test]
    fn package_json_deps_indexed() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "package.json",
            "{\n  \"name\": \"x\",\n  \"dependencies\": { \"react\": \"^18\", \"@scope/pkg\": \"^1\" },\n  \"devDependencies\": { \"vitest\": \"^1\" }\n}\n",
        );
        write(
            root,
            "src/main.js",
            "import React from 'react';\nexport function main_fn() {}\n",
        );
        let mut store = Store::open(&root.join(".deepseeknova/graph.db")).unwrap();
        store.refresh(root, 1_048_576).unwrap();

        let deps = store.external_deps().unwrap();
        for want in ["react", "@scope/pkg", "vitest"] {
            assert!(
                deps.iter().any(|(_, d)| d == want),
                "外部依赖应含 {want}：{deps:?}"
            );
        }
        let file_deps = store.external_deps_for_file("src/main.js").unwrap();
        assert!(file_deps.contains(&"react".to_string()), "{file_deps:?}");
    }

    #[test]
    fn pyproject_toml_deps_indexed() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "pyproject.toml",
            "[project]\nname = \"x\"\nversion = \"0.1.0\"\ndependencies = [\n  \"requests\",\n  \"flask>=2.0\",\n]\n",
        );
        write(
            root,
            "src/app.py",
            "import requests\ndef main():\n    pass\n",
        );
        let mut store = Store::open(&root.join(".deepseeknova/graph.db")).unwrap();
        store.refresh(root, 1_048_576).unwrap();

        let deps = store.external_deps().unwrap();
        assert!(deps.iter().any(|(_, d)| d == "requests"), "{deps:?}");
        assert!(deps.iter().any(|(_, d)| d == "flask>=2.0"), "{deps:?}");
        let file_deps = store.external_deps_for_file("src/app.py").unwrap();
        assert!(file_deps.contains(&"requests".to_string()), "{file_deps:?}");
    }

    #[test]
    fn manifest_without_deps_yields_empty_external_deps() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "Cargo.toml",
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
        );
        write(root, "src/lib.rs", "pub fn f() {}\n");
        let mut store = Store::open(&root.join(".deepseeknova/graph.db")).unwrap();
        store.refresh(root, 1_048_576).unwrap();
        assert!(store.external_deps().unwrap().is_empty());

        // 无清单文件的纯源码项目同样为空。
        let dir2 = tempdir().unwrap();
        write(dir2.path(), "src/a.rs", "pub fn g() {}\n");
        let mut store2 = Store::open(&dir2.path().join(".deepseeknova/graph.db")).unwrap();
        store2.refresh(dir2.path(), 1_048_576).unwrap();
        assert!(store2.external_deps().unwrap().is_empty());
    }

    #[test]
    fn circular_imports_produce_edges_in_both_directions() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/a.js",
            "import { b } from './b.js';\nexport const a = 1;\n",
        );
        write(
            root,
            "src/b.js",
            "import { a } from './a.js';\nexport const b = 2;\n",
        );
        let mut store = Store::open(&root.join(".deepseeknova/graph.db")).unwrap();
        store.refresh(root, 1_048_576).unwrap();

        let imports: Vec<_> = store
            .all_edges()
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == EdgeKind::Imports)
            .collect();
        assert!(
            imports
                .iter()
                .any(|e| e.src.contains("a.js") && e.dst.contains("b.js")),
            "a→b 环边：{imports:?}"
        );
        assert!(
            imports
                .iter()
                .any(|e| e.src.contains("b.js") && e.dst.contains("a.js")),
            "b→a 环边：{imports:?}"
        );
    }

    #[test]
    fn go_relative_import_creates_file_to_file_edge() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "main.go",
            "package main\n\nimport (\n    \"fmt\"\n    \"./localpkg\"\n)\n\nfunc main() { fmt.Println(\"hi\") }\n",
        );
        write(
            root,
            "localpkg/localpkg.go",
            "package localpkg\n\nfunc Helper() int { return 1 }\n",
        );
        let mut store = Store::open(&root.join(".deepseeknova/graph.db")).unwrap();
        store.refresh(root, 1_048_576).unwrap();

        let imports: Vec<_> = store
            .all_edges()
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == EdgeKind::Imports)
            .collect();
        assert!(
            imports
                .iter()
                .any(|e| e.src.contains("main.go") && e.dst.contains("localpkg.go")),
            "main.go → localpkg.go Imports 边：{imports:?}"
        );
    }

    /// 确定性嵌入替身：子串 → 向量，命中靠语义（查询 token 不与目标共词也能召回）。
    struct FakeEmbed;

    impl EmbeddingProvider for FakeEmbed {
        fn embed(&self, text: &str) -> Result<Vec<f32>, DeepseeknovaError> {
            if text.contains("alpha") {
                Ok(vec![1.0, 0.0])
            } else if text.contains("beta") {
                Ok(vec![0.6, 0.8])
            } else if text.contains("gamma") {
                Ok(vec![0.0, 1.0])
            } else if text.contains("ferris") {
                Ok(vec![0.9, 0.1])
            } else if text.contains("needle") {
                Ok(vec![1.0, 0.0])
            } else {
                Ok(vec![0.0, 1.0])
            }
        }
    }

    /// 恒定失败的嵌入替身（模拟缺 key / 网络错误）。
    struct FailingEmbed;

    impl EmbeddingProvider for FailingEmbed {
        fn embed(&self, _text: &str) -> Result<Vec<f32>, DeepseeknovaError> {
            Err(DeepseeknovaError::provider(
                "embedding endpoint unavailable",
            ))
        }
    }

    #[test]
    fn hybrid_search_finds_semantic_only_hit_without_lexical_overlap() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/a.rs",
            "/// needle alpha formatter\npub fn alpha_needle() {}\n\
             /// ferris crab language\npub fn ferris_crab() {}\n",
        );
        let mut store = Store::open_with_embedder(
            &root.join(".deepseeknova/graph.db"),
            Some(Arc::new(FakeEmbed)),
            "test-model",
        )
        .unwrap();
        store.refresh(root, 1_048_576).unwrap();

        // 只嵌入符号节点（文件节点跳过）。
        assert_eq!(
            store.embedding_count().unwrap(),
            2,
            "alpha_needle + ferris_crab 应已嵌入"
        );

        // 纯词法：无共词，找不到 ferris_crab（语义命中）。
        let fts = store.search("needle", None, 10).unwrap();
        assert!(
            fts.iter().all(|n| n.name != "ferris_crab"),
            "FTS 不得召回语义独有命中: {:?}",
            fts.iter().map(|n| &n.name).collect::<Vec<_>>()
        );

        // hybrid：语义独有命中被召回，且词法+语义双命中居首。
        let hy = store.search_hybrid("needle", None, 10).unwrap();
        assert!(
            hy.iter().any(|n| n.name == "ferris_crab"),
            "hybrid 必须召回语义命中: {:?}",
            hy.iter().map(|n| &n.name).collect::<Vec<_>>()
        );
        assert_eq!(hy[0].name, "alpha_needle", "双命中（词法+语义）应居首");
    }

    #[test]
    fn hybrid_falls_back_to_fts_when_no_embedder() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/a.rs",
            "/// needle alpha formatter\npub fn alpha_needle() {}\n",
        );
        let mut store = Store::open(&root.join(".deepseeknova/graph.db")).unwrap();
        store.refresh(root, 1_048_576).unwrap();

        let hy = store.search_hybrid("needle", None, 10).unwrap();
        let plain = store.search("needle", None, 10).unwrap();
        assert!(!hy.is_empty(), "无嵌入后端必须回落 FTS");
        assert_eq!(hy.len(), plain.len());
        for (a, b) in hy.iter().zip(plain.iter()) {
            assert_eq!(a.id, b.id, "回落路径结果须与纯 FTS 一致");
        }
    }

    #[test]
    fn hybrid_falls_back_to_fts_when_embedding_fails() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/a.rs",
            "/// needle alpha formatter\npub fn alpha_needle() {}\n",
        );
        let mut store = Store::open_with_embedder(
            &root.join(".deepseeknova/graph.db"),
            Some(Arc::new(FailingEmbed)),
            "test-model",
        )
        .unwrap();
        // 刷新不得因嵌入失败中断（fail-open，节点仅词法可检索）。
        store.refresh(root, 1_048_576).unwrap();
        assert_eq!(store.embedding_count().unwrap(), 0, "嵌入失败则无向量落库");

        let hy = store.search_hybrid("needle", None, 10).unwrap();
        let plain = store.search("needle", None, 10).unwrap();
        assert!(!hy.is_empty(), "嵌入失败必须回落纯 FTS");
        assert_eq!(hy[0].id, plain[0].id, "回落路径结果须与纯 FTS 一致");
    }

    #[test]
    fn hybrid_fuses_bm25_and_cosine_with_weight() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/a.rs",
            "/// needle alpha formatter\npub fn alpha_needle() {}\n\
             /// needle beta utils\npub fn beta_needle() {}\n\
             /// needle gamma router\npub fn gamma_needle() {}\n\
             /// ferris crab language\npub fn ferris_crab() {}\n",
        );
        let mut store = Store::open_with_embedder(
            &root.join(".deepseeknova/graph.db"),
            Some(Arc::new(FakeEmbed)),
            "test-model",
        )
        .unwrap();
        store.refresh(root, 1_048_576).unwrap();

        // w=1.0：纯词法，顺序与 FTS 完全一致；语义独有命中（零分）被剔除。
        let pure_lex = store
            .search_hybrid_with_weight("needle", None, 10, 1.0)
            .unwrap();
        let plain = store.search("needle", None, 10).unwrap();
        let lex_names: Vec<&str> = pure_lex.iter().map(|n| n.name.as_str()).collect();
        let plain_names: Vec<&str> = plain.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(lex_names, plain_names, "w=1.0 必须与纯 FTS 同序");
        assert!(
            lex_names.iter().all(|n| *n != "ferris_crab"),
            "w=1.0 时语义独有命中不得出现在结果中"
        );

        // w=0.0：纯余弦排序（alpha=1.0 > ferris≈0.994 > beta=0.6；gamma 余弦 0 剔除）。
        let pure_sem = store
            .search_hybrid_with_weight("needle", None, 10, 0.0)
            .unwrap();
        let sem_names: Vec<&str> = pure_sem.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(
            sem_names,
            vec!["alpha_needle", "ferris_crab", "beta_needle"],
            "{sem_names:?}"
        );

        // 分数分解：各分量严格加性，融合重排一致。
        let bd = store
            .search_hybrid_breakdown("needle", None, 10, 0.5)
            .unwrap();
        for h in &bd {
            assert!(
                (h.bm25 + h.cosine - h.score).abs() < 1e-9,
                "分数分解必须严格加性: {h:?}"
            );
        }
        assert_eq!(bd[0].node.name, "alpha_needle", "0.5 融合双命中应居首");
    }

    #[test]
    fn hybrid_breakdown_decomposition_is_additive_across_weights() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/a.rs",
            "/// needle alpha formatter\npub fn alpha_needle() {}\n\
             /// ferris crab language\npub fn ferris_crab() {}\n",
        );
        let mut store = Store::open_with_embedder(
            &root.join(".deepseeknova/graph.db"),
            Some(Arc::new(FakeEmbed)),
            "test-model",
        )
        .unwrap();
        store.refresh(root, 1_048_576).unwrap();

        for w in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let bd = store
                .search_hybrid_breakdown("needle", None, 10, w)
                .unwrap();
            for h in &bd {
                assert!(
                    (h.bm25 + h.cosine - h.score).abs() < 1e-9,
                    "w={w} 分解必须严格加性：{h:?}"
                );
            }
            // alpha_needle 是唯一 FTS 命中 → 归一化 bm25=1.0、余弦=1.0，
            // 分量恰为权重，score 恒为 1.0。
            let alpha = bd
                .iter()
                .find(|h| h.node.name == "alpha_needle")
                .expect("alpha 命中必须存在");
            assert!(
                (alpha.bm25 - w).abs() < 1e-9,
                "w={w} alpha.bm25={}",
                alpha.bm25
            );
            assert!(
                (alpha.cosine - (1.0 - w)).abs() < 1e-9,
                "w={w} alpha.cosine={}",
                alpha.cosine
            );
            assert!(
                (alpha.score - 1.0).abs() < 1e-9,
                "w={w} alpha.score={}",
                alpha.score
            );
            // ferris_crab 纯语义命中：词法分为 0，仅余弦贡献。
            let ferris = bd.iter().find(|h| h.node.name == "ferris_crab");
            if w < 1.0 {
                let ferris = ferris.expect("w<1 时语义命中应保留");
                assert_eq!(ferris.bm25, 0.0, "语义独有命中词法分必须为 0");
                let expected = (1.0 - w) * cosine(&[1.0, 0.0], &[0.9, 0.1]) as f64;
                assert!(
                    (ferris.cosine - expected).abs() < 1e-4,
                    "w={w} ferris.cosine={} expected={expected}",
                    ferris.cosine
                );
            } else {
                assert!(ferris.is_none(), "w=1.0 零分语义命中应被剔除");
            }
        }
    }

    #[test]
    fn hybrid_kind_filter_excludes_semantic_only_other_kind() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/a.rs",
            "/// needle alpha formatter\npub fn alpha_needle() {}\n\
             /// ferris crab language\npub struct FerrisCrab {}\n",
        );
        let mut store = Store::open_with_embedder(
            &root.join(".deepseeknova/graph.db"),
            Some(Arc::new(FakeEmbed)),
            "test-model",
        )
        .unwrap();
        store.refresh(root, 1_048_576).unwrap();

        // 无 kind：语义独有命中 FerrisCrab 被召回。
        let all = store.search_hybrid("needle", None, 10).unwrap();
        assert!(
            all.iter().any(|n| n.name == "FerrisCrab"),
            "{:?}",
            all.iter().map(|n| &n.name).collect::<Vec<_>>()
        );
        // kind=Function：语义扫描按 kind 过滤，struct 不得进入结果。
        let funcs = store
            .search_hybrid("needle", Some(NodeKind::Function), 10)
            .unwrap();
        let func_names: Vec<&str> = funcs.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(func_names, vec!["alpha_needle"], "{func_names:?}");
    }

    #[test]
    fn hybrid_with_weight_and_breakdown_orders_agree() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/a.rs",
            "/// needle alpha formatter\npub fn alpha_needle() {}\n\
             /// needle beta utils\npub fn beta_needle() {}\n\
             /// ferris crab language\npub fn ferris_crab() {}\n",
        );
        let mut store = Store::open_with_embedder(
            &root.join(".deepseeknova/graph.db"),
            Some(Arc::new(FakeEmbed)),
            "test-model",
        )
        .unwrap();
        store.refresh(root, 1_048_576).unwrap();

        for w in [0.25, 0.5, 0.75] {
            let via_weight = store
                .search_hybrid_with_weight("needle", None, 10, w)
                .unwrap();
            let bd = store
                .search_hybrid_breakdown("needle", None, 10, w)
                .unwrap();
            let via_w: Vec<&str> = via_weight.iter().map(|n| n.name.as_str()).collect();
            let via_bd: Vec<&str> = bd.iter().map(|h| h.node.name.as_str()).collect();
            assert_eq!(
                via_w, via_bd,
                "w={w} 两条公开 API 路径必须同序：{via_w:?} vs {via_bd:?}"
            );
        }
    }

    #[test]
    fn incremental_refresh_updates_fts_on_content_change() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(root, "src/a.rs", "/// needle alpha\npub fn alpha_fn() {}\n");
        let mut store = Store::open(&root.join(".deepseeknova/graph.db")).unwrap();
        store.refresh(root, 1_048_576).unwrap();
        assert!(
            !store.search("needle", None, 10).unwrap().is_empty(),
            "初建：needle 应可检索"
        );

        // 改 doc：旧 token 消失、新 token 可检索（FTS 行随文件增量同步）。
        std::thread::sleep(std::time::Duration::from_millis(1100));
        write(root, "src/a.rs", "/// ferris beta\npub fn alpha_fn() {}\n");
        store.refresh(root, 1_048_576).unwrap();

        let old = store.search("needle", None, 10).unwrap();
        assert!(
            old.is_empty(),
            "旧 doc token 不得残留：{:?}",
            old.iter().map(|n| &n.name).collect::<Vec<_>>()
        );
        let new = store.search("ferris", None, 10).unwrap();
        assert!(
            new.iter().any(|n| n.name == "alpha_fn"),
            "{:?}",
            new.iter().map(|n| &n.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn like_fallback_respects_kind_filter() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/k.rs",
            "pub struct GoThing {}\npub fn go_run() {}\n",
        );
        let mut store = Store::open(&root.join(".deepseeknova/graph.db")).unwrap();
        store.refresh(root, 1_048_576).unwrap();

        // "go" 仅 2 字符 → LIKE 回退；kind 过滤仍生效。
        let funcs = store.search("go", Some(NodeKind::Function), 10).unwrap();
        let func_names: Vec<&str> = funcs.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(func_names, vec!["go_run"], "{func_names:?}");
        let structs = store.search("go", Some(NodeKind::Struct), 10).unwrap();
        assert!(
            structs.iter().any(|n| n.name == "GoThing"),
            "{:?}",
            structs.iter().map(|n| &n.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn hybrid_backward_compatible_with_old_index_without_embeddings() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/a.rs",
            "/// needle alpha formatter\npub fn alpha_needle() {}\n",
        );
        let db = root.join(".deepseeknova/graph.db");
        {
            let mut store = Store::open(&db).unwrap();
            store.refresh(root, 1_048_576).unwrap();
        }
        // 模拟升级前旧索引：文件无 node_embeddings 表。
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch("DROP TABLE node_embeddings;").unwrap();
        }
        // 重开：SCHEMA 增量重建表，不触发 schema_version 变更/全量重解析。
        let store = Store::open(&db).unwrap();
        assert_eq!(
            store.find_by_name("alpha_needle").unwrap().len(),
            1,
            "旧索引既有节点必须保留（不得因 schema 增量而全量清空）"
        );
        let hits = store.search("needle", None, 10).unwrap();
        assert!(!hits.is_empty(), "旧索引 FTS 检索必须正常");

        // 带嵌入后端重开也兼容：无向量 → 余弦全零 → 回落纯 FTS。
        let store2 =
            Store::open_with_embedder(&db, Some(Arc::new(FakeEmbed)), "test-model").unwrap();
        let hy = store2.search_hybrid("needle", None, 10).unwrap();
        assert!(!hy.is_empty(), "旧索引 hybrid 必须 fail-open 到 FTS");
        assert!(hy.iter().all(|n| n.name == "alpha_needle"));
    }

    #[test]
    fn future_schema_version_library_not_cleared_or_downgraded() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(root, "src/a.rs", "pub fn alpha_needle() {}\n");
        let db = root.join(".deepseeknova/graph.db");
        {
            let mut store = Store::open(&db).unwrap();
            store.refresh(root, 1_048_576).unwrap();
        }
        // 模拟未来版本库：仅把版本号改为 '5'，files 已索引内容保持不动。
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute(
                "UPDATE meta SET value = '5' WHERE key = 'schema_version'",
                [],
            )
            .unwrap();
        }
        let reopened = Store::open(&db).unwrap();
        // 版本号不得被改写回写（保持 '5'），files 不得被清空。
        let v: String = reopened
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v, "5", "未来版本库打开后不得回写降级版本号");
        let files: i64 = reopened
            .conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(files, 1, "未来版本库打开后不得清空 files");
        assert_eq!(
            reopened.find_by_name("alpha_needle").unwrap().len(),
            1,
            "未来版本库打开后既有索引仍可只读使用"
        );
    }

    #[test]
    fn old_schema_version_migrates_and_rewrites_version() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(root, "src/a.rs", "pub fn alpha_needle() {}\n");
        let db = root.join(".deepseeknova/graph.db");
        {
            let mut store = Store::open(&db).unwrap();
            store.refresh(root, 1_048_576).unwrap();
        }
        // 模拟已知旧版本库（v3）：可解析且 < 当前，应清空 files 并回写当前版本。
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute(
                "UPDATE meta SET value = '3' WHERE key = 'schema_version'",
                [],
            )
            .unwrap();
        }
        let reopened = Store::open(&db).unwrap();
        let v: String = reopened
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION, "已知旧版本库打开后应回写当前版本");
        // 既有迁移行为不变：files 被清空，强制下次全量重解析。
        let files: i64 = reopened
            .conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(files, 0, "已知旧版本库打开后应清空 files 强制全量重索引");
    }
}

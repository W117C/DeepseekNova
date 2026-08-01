//! SQLite 持久化：增量刷新、FTS5 检索、图邻居查询。

use crate::model::{node_id, EdgeKind, EdgeRec, GraphError, Node, NodeKind};
use crate::parser::{parse_source, Lang};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::UNIX_EPOCH;

/// 邻居遍历方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Callers,
    Callees,
    Both,
}

/// 追踪路径的默认最大边数（跳数）。
pub const DEFAULT_MAX_HOPS: usize = 6;

/// 单次追踪最多返回的路径数；超出时置 `truncated`。
const MAX_PATHS: usize = 100;

/// 追踪结果：路径按调用方向排列（callers 为「源 → … → 目标」）。
#[derive(Debug, Default)]
pub struct TraceResult {
    pub paths: Vec<Vec<Node>>,
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
    pub files_indexed: usize,
    pub files_reparsed: usize,
    pub nodes: usize,
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
    /// 打开（或创建）数据库并建表；父目录不存在则创建。
    pub fn open(db_path: &Path) -> Result<Store, GraphError> {
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
        // 迁移：旧版库缺 raw_calls/raw_imports 事实，清空 files 强制下次全量重解析
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
        if version.as_deref() != Some(SCHEMA_VERSION) {
            conn.execute("DELETE FROM files", [])?;
            conn.execute(
                "INSERT INTO meta(key, value) VALUES('schema_version', ?1)\n                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [SCHEMA_VERSION],
            )?;
        }
        Ok(Store { conn })
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

            let lang = lang.expect("manifest branch returned above");
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
            insert_node(&tx, &file_node)?;
            for def in &parsed.nodes {
                insert_node(&tx, def)?;
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

        // 名称级边解析：全库定义节点 name → [(id, path)]（排除 File/Directory 节点）
        let mut by_name: HashMap<String, Vec<(String, String)>> = HashMap::new();
        {
            let mut stmt = tx.prepare(
                "SELECT name, id, path FROM nodes WHERE kind NOT IN ('file', 'directory')\n                 ORDER BY path, start_line",
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

        report.nodes = tx.query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))?;
        report.edges = tx.query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))?;
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
            return self.search_like(&tokens, kind, limit);
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

        let mut hits: Vec<Node> = Vec::new();
        let base = format!(
            "SELECT n.id, n.kind, n.name, n.path, n.start_line, n.end_line,\n                    n.signature, n.doc, n.score\n             FROM nodes n JOIN {table} f ON n.id = f.id\n             WHERE {table} MATCH ?1"
        );
        if let Some(k) = kind {
            let sql = format!("{base} AND n.kind = ?2 ORDER BY bm25({table}) LIMIT ?3");
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map((&match_expr, k.as_str(), limit as i64), node_from_row)?;
            for row in rows {
                hits.push(row?);
            }
        } else {
            let sql = format!("{base} ORDER BY bm25({table}) LIMIT ?2");
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map((&match_expr, limit as i64), node_from_row)?;
            for row in rows {
                hits.push(row?);
            }
        }
        // 名称精确命中稳定置前（partition 保持相对顺序）
        let (mut exact, rest): (Vec<Node>, Vec<Node>) = hits
            .into_iter()
            .partition(|n| n.name.eq_ignore_ascii_case(query.trim()));
        exact.extend(rest);
        Ok(exact)
    }

    /// 是否含 CJK 字符（决定走 trigram 表）。
    fn has_cjk(text: &str) -> bool {
        text.chars().any(|c| {
            matches!(c as u32,
                0x3000..=0x303F | 0x3400..=0x4DBF | 0x4E00..=0x9FFF
                | 0xF900..=0xFAFF | 0xFF00..=0xFFEF)
        })
    }

    /// LIKE 回退：短词（1-2 字符）做子串匹配（NOCASE），按 PageRank score 排序，
    /// 名称精确命中稳定置前。
    fn search_like(
        &self,
        tokens: &[String],
        kind: Option<NodeKind>,
        limit: usize,
    ) -> Result<Vec<Node>, GraphError> {
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
        let mut hits: Vec<Node> = Vec::new();
        let rows = stmt.query_map(raw_params, node_from_row)?;
        for row in rows {
            hits.push(row?);
        }
        // 名称精确命中稳定置前（partition 保持相对顺序）
        let needle = tokens.join(" ");
        let (mut exact, rest): (Vec<Node>, Vec<Node>) = hits
            .into_iter()
            .partition(|n| n.name.eq_ignore_ascii_case(&needle));
        exact.extend(rest);
        Ok(exact)
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

        let mut callers_stmt = self
            .conn
            .prepare("SELECT src, kind FROM edges WHERE dst = ?1")?;
        let mut callees_stmt = self
            .conn
            .prepare("SELECT dst, kind FROM edges WHERE src = ?1")?;

        for _ in 0..hops {
            let mut next: Vec<String> = Vec::new();
            for cur in &frontier {
                let expand =
                    |stmt: &mut rusqlite::Statement<'_>| -> Result<Vec<String>, GraphError> {
                        let rows = stmt.query_map([cur], |row| {
                            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                        })?;
                        let mut out = Vec::new();
                        for row in rows {
                            let (other, kind_s) = row?;
                            let matches_kind = edge_kinds.is_empty()
                                || EdgeKind::parse(&kind_s)
                                    .is_some_and(|k| edge_kinds.contains(&k));
                            if matches_kind {
                                out.push(other);
                            }
                        }
                        Ok(out)
                    };
                let mut candidates: Vec<String> = Vec::new();
                if matches!(dir, Direction::Callers | Direction::Both) {
                    candidates.extend(expand(&mut callers_stmt)?);
                }
                if matches!(dir, Direction::Callees | Direction::Both) {
                    candidates.extend(expand(&mut callees_stmt)?);
                }
                for cand in candidates {
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
        });
        Ok(nodes)
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
        let edges = self.all_edges()?;
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

    /// 批量更新 nodes.score。
    pub fn set_scores(&self, scores: &[(String, f64)]) -> Result<(), GraphError> {
        let mut stmt = self
            .conn
            .prepare("UPDATE nodes SET score = ?1 WHERE id = ?2")?;
        for (id, score) in scores {
            stmt.execute((score, id))?;
        }
        Ok(())
    }
}

fn insert_node(conn: &Connection, node: &Node) -> Result<(), GraphError> {
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
    matches!(name, "Cargo.toml" | "package.json" | "pyproject.toml")
}

/// 解析清单文件的外部依赖名（轻量行级/serde_json，不引入新依赖）。
fn parse_manifest_deps(path: &str, src: &str) -> Vec<String> {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name {
        "Cargo.toml" => parse_cargo_deps(src),
        "package.json" => parse_package_json_deps(src),
        "pyproject.toml" => parse_pyproject_deps(src),
        _ => Vec::new(),
    }
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

/// 把 JS/TS 相对 specifier 解析为索引中的文件节点 id（补常见扩展名探测）。
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
        for ext in ["ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "rs"] {
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
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::EdgeKind;
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
}

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
pub enum Direction { Callers, Callees, Both }

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
";

/// 硬排除的目录名（任何路径段命中即跳过）。
const HARD_EXCLUDES: [&str; 4] = ["target", "node_modules", ".git", "dist"];

/// SQLite 持久化的代码图存储（单线程串行；上层门面负责加锁）。
pub struct Store {
    conn: Connection,
}

/// 单个重解析文件暂存的名称级边素材。
struct PendingEdges {
    rel_path: String,
    file_node_id: String,
    calls: Vec<(String, String)>,
    imports: Vec<String>,
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
        Ok(Store { conn })
    }

    /// 增量刷新：mtime/sha256 双级判定，仅重解析变更文件。
    pub fn refresh(&mut self, root: &Path, max_file_size: u64) -> Result<RefreshReport, GraphError> {
        let ignores = load_gitignore(root);
        let mut files: Vec<(std::path::PathBuf, String)> = Vec::new();
        collect_files(root, root, &ignores, &mut files);
        files.sort_by(|a, b| a.1.cmp(&b.1));

        let mut report = RefreshReport::default();
        let mut pending: Vec<PendingEdges> = Vec::new();
        let tx = self.conn.transaction()?;

        for (abs_path, rel_path) in &files {
            let Some(lang) = Lang::from_path(rel_path) else { continue };
            let Ok(meta) = std::fs::metadata(abs_path) else { continue };
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
                    tx.execute("UPDATE files SET mtime = ?1 WHERE path = ?2", (mtime, rel_path))?;
                    continue;
                }
            }

            let parsed = match parse_source(lang, rel_path, &src) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(path = %rel_path, error = %e, "parse failed, skipping");
                    continue;
                }
            };

            // 清理该文件旧数据：出入边（id 前缀为 path#）、节点、FTS 行
            tx.execute(
                "DELETE FROM edges WHERE src LIKE ?1 || '#%' OR dst LIKE ?1 || '#%'",
                [rel_path],
            )?;
            tx.execute("DELETE FROM nodes WHERE path = ?1", [rel_path])?;
            tx.execute("DELETE FROM symbol_fts WHERE path = ?1", [rel_path])?;

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
            pending.push(PendingEdges {
                rel_path: rel_path.clone(),
                file_node_id: file_node.id,
                calls: parsed.calls,
                imports: parsed.imports,
            });
            report.files_reparsed += 1;
            tx.execute(
                "INSERT INTO files(path, mtime, hash) VALUES(?1, ?2, ?3)\n                 ON CONFLICT(path) DO UPDATE SET mtime = excluded.mtime, hash = excluded.hash",
                (rel_path, mtime, &hash),
            )?;
        }

        // 名称级边解析：全库定义节点 name → [(id, path)]（排除 File/Directory 节点）
        let mut by_name: HashMap<String, Vec<(String, String)>> = HashMap::new();
        {
            let mut stmt = tx.prepare(
                "SELECT name, id, path FROM nodes WHERE kind NOT IN ('file', 'directory')\n                 ORDER BY path, start_line",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
            })?;
            for row in rows {
                let (name, id, path) = row?;
                by_name.entry(name).or_default().push((id, path));
            }
        }
        for pend in &pending {
            for (caller_name, callee_name) in &pend.calls {
                let caller_id = by_name.get(caller_name).and_then(|ids| {
                    ids.iter().find(|(_, p)| *p == pend.rel_path).map(|(id, _)| id)
                });
                let callee_id = by_name.get(callee_name).and_then(|ids| ids.first().map(|(id, _)| id));
                if let (Some(src), Some(dst)) = (caller_id, callee_id) {
                    tx.execute(
                        "INSERT INTO edges(src, dst, kind) VALUES(?1, ?2, ?3)",
                        (src, dst, EdgeKind::Calls.as_str()),
                    )?;
                }
            }
            for import in &pend.imports {
                for (name, ids) in &by_name {
                    if import.contains(name.as_str()) {
                        if let Some((dst, _)) = ids.first() {
                            tx.execute(
                                "INSERT INTO edges(src, dst, kind) VALUES(?1, ?2, ?3)",
                                (&pend.file_node_id, dst, EdgeKind::Imports.as_str()),
                            )?;
                        }
                    }
                }
            }
        }

        report.nodes = tx.query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))?;
        report.edges = tx.query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))?;
        tx.commit()?;
        Ok(report)
    }

    /// FTS5 BM25 检索；名称精确匹配（忽略大小写）稳定置前。
    pub fn search(&self, query: &str, kind: Option<NodeKind>, limit: usize) -> Result<Vec<Node>, GraphError> {
        let tokens: Vec<String> = query
            .split_whitespace()
            .filter(|t| !t.is_empty())
            .map(|t| format!("\"{}\"", t.replace('\"', "\"\"")))
            .collect();
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        let match_expr = tokens.join(" OR ");

        let mut hits: Vec<Node> = Vec::new();
        let base = "SELECT n.id, n.kind, n.name, n.path, n.start_line, n.end_line,\n                    n.signature, n.doc, n.score\n             FROM nodes n JOIN symbol_fts f ON n.id = f.id\n             WHERE symbol_fts MATCH ?1";
        if let Some(k) = kind {
            let sql = format!("{base} AND n.kind = ?2 ORDER BY bm25(symbol_fts) LIMIT ?3");
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map((&match_expr, k.as_str(), limit as i64), node_from_row)?;
            for row in rows {
                hits.push(row?);
            }
        } else {
            let sql = format!("{base} ORDER BY bm25(symbol_fts) LIMIT ?2");
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map((&match_expr, limit as i64), node_from_row)?;
            for row in rows {
                hits.push(row?);
            }
        }
        // 名称精确命中稳定置前（partition 保持相对顺序）
        let (mut exact, rest): (Vec<Node>, Vec<Node>) =
            hits.into_iter().partition(|n| n.name.eq_ignore_ascii_case(query.trim()));
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

        let mut callers_stmt = self.conn.prepare("SELECT src, kind FROM edges WHERE dst = ?1")?;
        let mut callees_stmt = self.conn.prepare("SELECT dst, kind FROM edges WHERE src = ?1")?;

        for _ in 0..hops {
            let mut next: Vec<String> = Vec::new();
            for cur in &frontier {
                let expand = |stmt: &mut rusqlite::Statement<'_>| -> Result<Vec<String>, GraphError> {
                    let rows = stmt.query_map([cur], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?;
                    let mut out = Vec::new();
                    for row in rows {
                        let (other, kind_s) = row?;
                        let matches_kind = edge_kinds.is_empty()
                            || EdgeKind::parse(&kind_s).is_some_and(|k| edge_kinds.contains(&k));
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
        nodes.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        Ok(nodes)
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
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
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
        let mut stmt = self.conn.prepare("UPDATE nodes SET score = ?1 WHERE id = ?2")?;
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
    let Ok(entries) = std::fs::read_dir(dir) else { return };
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
        write(root, "src/a.rs", "pub fn alpha() { beta(); }\npub fn beta() {}\n");
        write(root, "src/b.rs", "pub fn gamma() {}\n");
        let mut store = Store::open(&root.join(".deepseeknova/graph.db")).unwrap();

        let n1 = store.refresh(root, 1_048_576).unwrap();
        assert!(n1.files_indexed >= 2);
        let beta_id = store.find_by_name("beta").unwrap()[0].id.clone();

        let callers = store.neighbors(&beta_id, &[EdgeKind::Calls], Direction::Callers, 1).unwrap();
        assert!(callers.iter().any(|n| n.name == "alpha"));

        std::thread::sleep(std::time::Duration::from_millis(1100));
        write(root, "src/b.rs", "pub fn gamma() {}\npub fn delta() {}\n");
        let n2 = store.refresh(root, 1_048_576).unwrap();
        assert_eq!(n2.files_reparsed, 1);
        assert_eq!(store.find_by_name("beta").unwrap()[0].id, beta_id);
        assert!(!store.find_by_name("delta").unwrap().is_empty());
    }

    #[test]
    fn fts_search_ranks_name_match() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(root, "src/a.rs", "/// permission gate\npub struct PermissionGate {}\npub fn unrelated() {}\n");
        let mut store = Store::open(&root.join(".deepseeknova/graph.db")).unwrap();
        store.refresh(root, 1_048_576).unwrap();
        let hits = store.search("PermissionGate", None, 10).unwrap();
        assert_eq!(hits[0].name, "PermissionGate");
    }
}

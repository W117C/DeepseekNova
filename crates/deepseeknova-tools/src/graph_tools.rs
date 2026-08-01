//! 图检索工具：search_code / traverse_graph / retrieve_entity /
//! trace_code / impact_code / explore_code。
//! 索引句柄经 `ToolContext.extensions` 注入（`GraphHandle`），缺失时优雅降级。

use async_trait::async_trait;
use deepseeknova_core::{Tool, ToolContext, ToolSchema};
use deepseeknova_graph::{Direction, EdgeKind, GraphError, NodeKind};
use serde::Deserialize;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// 共享代码图索引句柄（Task10 由 agent 注入）。
pub type GraphHandle = std::sync::Arc<std::sync::Mutex<deepseeknova_graph::GraphIndex>>;

/// 索引缺失时的降级提示。
const NO_INDEX_MSG: &str = "代码图索引构建中或未启用，请改用 grep 检索。";

/// 从 ctx.extensions 取索引句柄（缺失返回 None，调用方降级）。
fn graph_handle(ctx: &ToolContext) -> Option<GraphHandle> {
    ctx.extensions.get::<GraphHandle>().cloned()
}

/// 把任意 `GraphError` 映射为对模型友好的文字提示（规格 §8：不打断 run）。
fn graph_error_message(action: &str, err: &GraphError) -> String {
    match err {
        GraphError::EntityNotFound(name) => {
            format!("entity '{name}' not found; try search_code first.")
        }
        GraphError::IndexBusy => {
            format!("code graph is refreshing while {action}; retry shortly or use grep.")
        }
        GraphError::Parse { path, .. } => {
            format!("code graph parse issue near {path} while {action}; result may be partial.")
        }
        GraphError::Storage(_) => {
            format!("code graph storage error while {action}; try again or rebuild the index.")
        }
    }
}

fn lock_index(
    handle: &GraphHandle,
) -> anyhow::Result<std::sync::MutexGuard<'_, deepseeknova_graph::GraphIndex>> {
    handle
        .lock()
        .map_err(|_| anyhow::anyhow!("graph index lock poisoned"))
}

pub struct SearchCodeTool;

#[derive(Deserialize)]
struct SearchCodeArgs {
    query: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

#[async_trait]
impl Tool for SearchCodeTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "search_code".to_string(),
            description: "Finds code entities by symbol.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Symbol/keyword."
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["directory", "file", "struct", "enum", "trait", "class", "function", "method"],
                        "description": "Kind (optional)."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max (10 default, 50 cap)."
                    }
                },
                "required": ["query"]
            }),
        }
    }

    fn read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        deepseeknova_security::context::enforce_capability(
            ctx,
            deepseeknova_security::capability::Capability::FileRead,
        )?;
        let parsed: SearchCodeArgs = serde_json::from_str(args)?;
        let handle = match graph_handle(ctx) {
            Some(h) => h,
            None => return Ok(NO_INDEX_MSG.to_string()),
        };
        let idx = lock_index(&handle)?;

        let kind = parsed.kind.as_deref().and_then(NodeKind::parse);
        let limit = parsed.limit.unwrap_or(10).min(50);
        let nodes = match idx.search(&parsed.query, kind, limit) {
            Ok(n) => n,
            Err(e) => return Ok(graph_error_message("searching code", &e)),
        };

        if nodes.is_empty() {
            return Ok(format!("no matches for '{}'", parsed.query));
        }
        let lines: Vec<String> = nodes
            .iter()
            .enumerate()
            .map(|(i, node)| {
                format!(
                    "{}. {} {} — {}:{}-{} · {}",
                    i + 1,
                    node.kind.as_str(),
                    node.name,
                    node.path,
                    node.start_line,
                    node.end_line,
                    node.signature
                )
            })
            .collect();
        Ok(lines.join("\n"))
    }
}

pub struct TraverseGraphTool;

#[derive(Deserialize)]
struct TraverseGraphArgs {
    entity: String,
    #[serde(default)]
    direction: Option<String>,
    #[serde(default)]
    edge_kinds: Option<Vec<String>>,
    #[serde(default)]
    hops: Option<usize>,
}

#[async_trait]
impl Tool for TraverseGraphTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "traverse_graph".to_string(),
            description: "Traverses graph neighbors.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "entity": {
                        "type": "string",
                        "description": "Entity."
                    },
                    "direction": {
                        "type": "string",
                        "enum": ["callers", "callees", "both"],
                        "description": "Direction (default both)."
                    },
                    "edge_kinds": {
                        "type": "array",
                        "items": {
                            "type": "string",
                            "enum": ["contains", "imports", "calls", "implements", "references", "dispatch"]
                        },
                        "description": "Edges (default calls)."
                    },
                    "hops": {
                        "type": "integer",
                        "description": "Depth (2 default, cap 3)."
                    }
                },
                "required": ["entity"]
            }),
        }
    }

    fn read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        deepseeknova_security::context::enforce_capability(
            ctx,
            deepseeknova_security::capability::Capability::FileRead,
        )?;
        let parsed: TraverseGraphArgs = serde_json::from_str(args)?;
        let handle = match graph_handle(ctx) {
            Some(h) => h,
            None => return Ok(NO_INDEX_MSG.to_string()),
        };
        let idx = lock_index(&handle)?;

        let direction_label = match parsed.direction.as_deref() {
            Some("callers") => "callers",
            Some("callees") => "callees",
            _ => "both",
        };
        let dir = match direction_label {
            "callers" => Direction::Callers,
            "callees" => Direction::Callees,
            _ => Direction::Both,
        };
        let edge_kinds: Vec<EdgeKind> = parsed
            .edge_kinds
            .unwrap_or_default()
            .iter()
            .filter_map(|s| EdgeKind::parse(s))
            .collect();
        let edge_kinds = if edge_kinds.is_empty() {
            vec![EdgeKind::Calls]
        } else {
            edge_kinds
        };
        let hops = parsed.hops.unwrap_or(2).min(3);

        let neighbors = match idx.neighbors(&parsed.entity, &edge_kinds, dir, hops) {
            Ok(nodes) => nodes,
            Err(e) => return Ok(graph_error_message("traversing the graph", &e)),
        };

        if neighbors.is_empty() {
            let edges: Vec<&str> = edge_kinds.iter().map(|k| k.as_str()).collect();
            return Ok(format!(
                "{} has no {} via {}",
                parsed.entity,
                direction_label,
                edges.join(",")
            ));
        }

        let mut out = format!("{}（{}）:\n", parsed.entity, direction_label);
        let shown = neighbors.len().min(40);
        for n in &neighbors[..shown] {
            out.push_str(&format!(
                "  {} {} — {}:{}\n",
                n.kind.as_str(),
                n.name,
                n.path,
                n.start_line
            ));
        }
        if neighbors.len() > shown {
            out.push_str(&format!("  …(+{} more)\n", neighbors.len() - shown));
        }
        if out.len() > 8000 {
            let end = out.floor_char_boundary(8000);
            out.truncate(end);
        }
        Ok(out)
    }
}

pub struct RetrieveEntityTool;

#[derive(Deserialize)]
struct RetrieveEntityArgs {
    entity: String,
    #[serde(default)]
    view: Option<String>,
}

#[async_trait]
impl Tool for RetrieveEntityTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "retrieve_entity".to_string(),
            description: "skeleton=doc+signatures; full=source lines.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "entity": {
                        "type": "string",
                        "description": "Entity."
                    },
                    "view": {
                        "type": "string",
                        "enum": ["skeleton", "full"],
                        "description": "skeleton/full."
                    }
                },
                "required": ["entity"]
            }),
        }
    }

    fn read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        deepseeknova_security::context::enforce_capability(
            ctx,
            deepseeknova_security::capability::Capability::FileRead,
        )?;
        let parsed: RetrieveEntityArgs = serde_json::from_str(args)?;
        let handle = match graph_handle(ctx) {
            Some(h) => h,
            None => return Ok(NO_INDEX_MSG.to_string()),
        };
        let idx = lock_index(&handle)?;

        if parsed.view.as_deref() == Some("full") {
            let (rel_path, start, end) = match idx.location(&parsed.entity) {
                Ok(loc) => loc,
                Err(e) => return Ok(graph_error_message("locating the entity", &e)),
            };
            let abs = deepseeknova_security::path::sanitize_path(&ctx.workspace_root, &rel_path)?;
            let content = match std::fs::read_to_string(&abs) {
                Ok(c) => c,
                Err(e) => {
                    return Ok(format!(
                        "failed to read {rel_path}: {e}; index may be stale"
                    ));
                }
            };
            let lines: Vec<String> = content
                .lines()
                .enumerate()
                .filter(|(i, _)| {
                    let lineno = (i + 1) as u32;
                    lineno >= start && lineno <= end
                })
                .map(|(i, line)| format!("{:>5} | {}", i + 1, line))
                .collect();
            return Ok(format!("{rel_path}:{start}-{end}\n{}", lines.join("\n")));
        }

        match idx.skeleton(&parsed.entity) {
            Ok(sk) => Ok(sk),
            Err(e) => Ok(graph_error_message("retrieving the skeleton", &e)),
        }
    }
}

/// 渲染一条路径：`name @ path:line → name @ path:line → …`。
fn render_path(path: &[deepseeknova_graph::Node]) -> String {
    path.iter()
        .map(|n| format!("{} @ {}:{}", n.name, n.path, n.start_line))
        .collect::<Vec<_>>()
        .join(" → ")
}

fn parse_direction(s: Option<&str>) -> Direction {
    match s {
        Some("callers") => Direction::Callers,
        Some("callees") => Direction::Callees,
        _ => Direction::Both,
    }
}

fn direction_label(dir: Direction) -> &'static str {
    match dir {
        Direction::Callers => "callers",
        Direction::Callees => "callees",
        Direction::Both => "both",
    }
}

pub struct TraceCodeTool;

#[derive(Deserialize)]
struct TraceCodeArgs {
    entity: String,
    #[serde(default)]
    direction: Option<String>,
    #[serde(default)]
    edge_kinds: Option<Vec<String>>,
    #[serde(default)]
    max_hops: Option<usize>,
}

#[async_trait]
impl Tool for TraceCodeTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "trace_code".to_string(),
            description: "Traces multi-hop call paths, including dynamic dispatch.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "entity": {
                        "type": "string",
                        "description": "Entity to trace."
                    },
                    "direction": {
                        "type": "string",
                        "enum": ["callers", "callees", "both"],
                        "description": "Path direction (default both)."
                    },
                    "edge_kinds": {
                        "type": "array",
                        "items": {
                            "type": "string",
                            "enum": ["contains", "imports", "calls", "implements", "references", "dispatch"]
                        },
                        "description": "Edges (default calls,references,dispatch)."
                    },
                    "max_hops": {
                        "type": "integer",
                        "description": "Depth (6 default, cap 6)."
                    }
                },
                "required": ["entity"]
            }),
        }
    }

    fn read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        deepseeknova_security::context::enforce_capability(
            ctx,
            deepseeknova_security::capability::Capability::FileRead,
        )?;
        let parsed: TraceCodeArgs = serde_json::from_str(args)?;
        let handle = match graph_handle(ctx) {
            Some(h) => h,
            None => return Ok(NO_INDEX_MSG.to_string()),
        };
        let idx = lock_index(&handle)?;

        let dir = parse_direction(parsed.direction.as_deref());
        let edge_kinds: Vec<EdgeKind> = parsed
            .edge_kinds
            .unwrap_or_default()
            .iter()
            .filter_map(|s| EdgeKind::parse(s))
            .collect();
        let edge_kinds = if edge_kinds.is_empty() {
            vec![EdgeKind::Calls, EdgeKind::References, EdgeKind::Dispatch]
        } else {
            edge_kinds
        };
        let hops = parsed.max_hops.unwrap_or(6).min(6);

        let tr = match idx.trace(&parsed.entity, &edge_kinds, dir, hops) {
            Ok(t) => t,
            Err(e) => return Ok(graph_error_message("tracing the entity", &e)),
        };
        if tr.paths.is_empty() {
            return Ok(format!("no paths found for '{}'", parsed.entity));
        }
        let mut out = format!(
            "trace_code({}, direction={}):\n",
            parsed.entity,
            direction_label(dir)
        );
        for path in &tr.paths {
            out.push_str(&render_path(path));
            out.push('\n');
        }
        if tr.truncated {
            out.push_str(&format!("(truncated: chains beyond {hops} hops omitted)\n"));
        }
        if out.len() > 8000 {
            let end = out.floor_char_boundary(8000);
            out.truncate(end);
        }
        Ok(out)
    }
}

pub struct ImpactCodeTool;

#[derive(Deserialize)]
struct ImpactCodeArgs {
    entity: String,
    #[serde(default)]
    max_hops: Option<usize>,
}

#[async_trait]
impl Tool for ImpactCodeTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "impact_code".to_string(),
            description: "Estimates refactor blast radius: who reaches the entity.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "entity": {
                        "type": "string",
                        "description": "Entity to estimate impact for."
                    },
                    "max_hops": {
                        "type": "integer",
                        "description": "Depth (6 default, cap 6)."
                    }
                },
                "required": ["entity"]
            }),
        }
    }

    fn read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        deepseeknova_security::context::enforce_capability(
            ctx,
            deepseeknova_security::capability::Capability::FileRead,
        )?;
        let parsed: ImpactCodeArgs = serde_json::from_str(args)?;
        let handle = match graph_handle(ctx) {
            Some(h) => h,
            None => return Ok(NO_INDEX_MSG.to_string()),
        };
        let idx = lock_index(&handle)?;

        let hops = parsed.max_hops.unwrap_or(6).min(6);
        let kinds = vec![EdgeKind::Calls, EdgeKind::References, EdgeKind::Dispatch];
        let tr = match idx.trace(&parsed.entity, &kinds, Direction::Callers, hops) {
            Ok(t) => t,
            Err(e) => return Ok(graph_error_message("estimating impact", &e)),
        };
        if tr.paths.is_empty() {
            return Ok(format!("no impact paths found for '{}'", parsed.entity));
        }

        // 按文件聚合：文件 → 符号 → 经过该符号的路径文本（不含目标本身）。
        let mut by_file: BTreeMap<String, BTreeMap<String, BTreeSet<String>>> = BTreeMap::new();
        let mut all_paths: BTreeSet<String> = BTreeSet::new();
        for path in &tr.paths {
            let text = render_path(path);
            all_paths.insert(text.clone());
            for node in &path[..path.len().saturating_sub(1)] {
                by_file
                    .entry(node.path.clone())
                    .or_default()
                    .entry(node.name.clone())
                    .or_default()
                    .insert(text.clone());
            }
        }

        let mut out = format!("impact_code({}, max_hops={}):\n", parsed.entity, hops);
        for (file, symbols) in &by_file {
            out.push_str(&format!("{} ({} symbols):\n", file, symbols.len()));
            for (symbol, paths) in symbols {
                out.push_str(&format!("  {} ({} paths):\n", symbol, paths.len()));
                for p in paths {
                    out.push_str(&format!("    {p}\n"));
                }
            }
        }
        out.push_str(&format!(
            "total: {} files, {} paths\n",
            by_file.len(),
            all_paths.len()
        ));
        if tr.truncated {
            out.push_str(&format!("(truncated: chains beyond {hops} hops omitted)\n"));
        }
        if out.len() > 8000 {
            let end = out.floor_char_boundary(8000);
            out.truncate(end);
        }
        Ok(out)
    }
}

pub struct ExploreCodeTool;

#[derive(Deserialize)]
struct ExploreCodeArgs {
    entities: Vec<String>,
    #[serde(default)]
    view: Option<String>,
}

#[async_trait]
impl Tool for ExploreCodeTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "explore_code".to_string(),
            description: "Reads entities as line-numbered source grouped by file.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "entities": {
                        "type": "array",
                        "items": {
                            "type": "string"
                        },
                        "description": "Entities to explore (grouped by file)."
                    },
                    "view": {
                        "type": "string",
                        "enum": ["full", "skeleton"],
                        "description": "full=source lines (default); skeleton=doc+signatures."
                    }
                },
                "required": ["entities"]
            }),
        }
    }

    fn read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        deepseeknova_security::context::enforce_capability(
            ctx,
            deepseeknova_security::capability::Capability::FileRead,
        )?;
        let parsed: ExploreCodeArgs = serde_json::from_str(args)?;
        let handle = match graph_handle(ctx) {
            Some(h) => h,
            None => return Ok(NO_INDEX_MSG.to_string()),
        };
        let idx = lock_index(&handle)?;

        let view = parsed.view.as_deref().unwrap_or("full");
        let mut located: Vec<(String, String, u32, u32)> = Vec::new(); // name, path, start, end
        let mut missing: Vec<String> = Vec::new();
        for entity in &parsed.entities {
            match idx.location(entity) {
                Ok((path, start, end)) => located.push((entity.clone(), path, start, end)),
                Err(e) => missing.push(format!(
                    "{} ({})",
                    entity,
                    graph_error_message("locating the entity", &e)
                )),
            }
        }
        if located.is_empty() {
            return Ok(missing
                .first()
                .cloned()
                .unwrap_or_else(|| "no entities provided".to_string()));
        }

        let mut out = String::new();
        if view == "skeleton" {
            let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for (name, path, _, _) in &located {
                match idx.skeleton(name) {
                    Ok(sk) => grouped
                        .entry(path.clone())
                        .or_default()
                        .push(format!("{name}:\n{}", indent(&sk, 2))),
                    Err(e) => missing.push(format!(
                        "{} ({})",
                        name,
                        graph_error_message("retrieving the skeleton", &e)
                    )),
                }
            }
            for (path, entries) in &grouped {
                out.push_str(&format!("## {path}\n"));
                for entry in entries {
                    out.push_str(entry);
                }
            }
        } else {
            let mut grouped: BTreeMap<String, Vec<(String, u32, u32)>> = BTreeMap::new();
            for (name, path, start, end) in located {
                grouped.entry(path).or_default().push((name, start, end));
            }
            for (path, mut ranges) in grouped {
                ranges.sort_by_key(|r| (r.1, r.2));
                // 合并相邻/重叠区间，按文件一次读取。
                let mut merged: Vec<(u32, u32)> = Vec::new();
                for (_, start, end) in ranges {
                    if let Some(last) = merged.last_mut() {
                        if start <= last.1.saturating_add(1) {
                            last.1 = last.1.max(end);
                            continue;
                        }
                    }
                    merged.push((start, end));
                }
                let abs = deepseeknova_security::path::sanitize_path(&ctx.workspace_root, &path)?;
                let content = match std::fs::read_to_string(&abs) {
                    Ok(c) => c,
                    Err(e) => {
                        return Ok(format!("failed to read {path}: {e}; index may be stale"));
                    }
                };
                out.push_str(&format!("## {path}\n"));
                for (start, end) in merged {
                    for (i, line) in content.lines().enumerate() {
                        let lineno = (i + 1) as u32;
                        if lineno >= start && lineno <= end {
                            out.push_str(&format!("{:>5} | {line}\n", lineno));
                        }
                    }
                }
            }
        }
        if !missing.is_empty() {
            out.push_str(&format!("(not found: {})\n", missing.join("; ")));
        }
        if out.len() > 8000 {
            let end = out.floor_char_boundary(8000);
            out.truncate(end);
        }
        Ok(out)
    }
}

fn indent(text: &str, spaces: usize) -> String {
    let pad = " ".repeat(spaces);
    text.lines()
        .map(|l| format!("{pad}{l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// graph.enabled 时由 runtime 注册的三个高级图查询工具。
/// 注册点选在 runtime（白名单内），不动 all_builtin 列表及其 schema 预算测试。
pub fn graph_query_tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(TraceCodeTool),
        Arc::new(ImpactCodeTool),
        Arc::new(ExploreCodeTool),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_core::tool::{Tool, ToolContext};
    use std::sync::{Arc, Mutex};
    use tempfile::tempdir;

    fn ctx_with_index(root: &std::path::Path) -> ToolContext {
        let mut idx = deepseeknova_graph::GraphIndex::open(root, 1_048_576).unwrap();
        idx.refresh().unwrap();
        let handle: GraphHandle = Arc::new(Mutex::new(idx));
        ToolContext::new("c1")
            .with_workspace(root.to_path_buf())
            .with_extension(handle)
            .with_extension(deepseeknova_security::context::SecurityContext::with_safe_defaults())
    }

    #[tokio::test]
    async fn search_then_traverse_then_retrieve() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn build_agent() { permission_gate_for(); }\npub fn permission_gate_for() {}\n",
        )
        .unwrap();
        let ctx = ctx_with_index(root);

        let s = SearchCodeTool
            .execute(&ctx, r#"{"query":"build_agent"}"#)
            .await
            .unwrap();
        assert!(s.contains("build_agent"));

        let t = TraverseGraphTool
            .execute(
                &ctx,
                r#"{"entity":"permission_gate_for","direction":"callers"}"#,
            )
            .await
            .unwrap();
        assert!(t.contains("build_agent"));

        let r = RetrieveEntityTool
            .execute(&ctx, r#"{"entity":"permission_gate_for","view":"full"}"#)
            .await
            .unwrap();
        assert!(r.contains("pub fn permission_gate_for"));
    }

    #[tokio::test]
    async fn tools_never_bubble_graph_errors() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub fn only() {}\n").unwrap();
        let ctx = ctx_with_index(root);

        // 不存在实体：三工具都应返回 Ok(提示)，不 Err。
        let t = TraverseGraphTool
            .execute(&ctx, r#"{"entity":"no_such","direction":"callers"}"#)
            .await;
        assert!(t.is_ok(), "traverse must not bubble error");
        assert!(t.unwrap().contains("not found"));
        let r = RetrieveEntityTool
            .execute(&ctx, r#"{"entity":"no_such","view":"full"}"#)
            .await;
        assert!(r.is_ok(), "retrieve must not bubble error");
        assert!(r.unwrap().contains("not found"));
        let sk = RetrieveEntityTool
            .execute(&ctx, r#"{"entity":"no_such"}"#)
            .await;
        assert!(sk.is_ok(), "skeleton must not bubble error");
        assert!(sk.unwrap().contains("not found"));
    }

    #[tokio::test]
    async fn degrades_without_index() {
        let ctx = ToolContext::new("c2")
            .with_extension(deepseeknova_security::context::SecurityContext::with_safe_defaults());
        let out = SearchCodeTool
            .execute(&ctx, r#"{"query":"x"}"#)
            .await
            .unwrap();
        assert!(out.contains("索引") || out.to_lowercase().contains("index"));
    }

    #[tokio::test]
    async fn trace_code_returns_multi_hop_call_chain() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/chain.rs"),
            "pub fn a() { b(); }\npub fn b() { c(); }\npub fn c() {}\n",
        )
        .unwrap();
        let ctx = ctx_with_index(root);

        let out = TraceCodeTool
            .execute(&ctx, r#"{"entity":"c","direction":"callers"}"#)
            .await
            .unwrap();
        assert!(
            out.contains("a @ src/chain.rs:1 → b @ src/chain.rs:2 → c @ src/chain.rs:3"),
            "trace 应输出带行号的完整链：{out}"
        );
    }

    #[tokio::test]
    async fn trace_code_follows_dynamic_dispatch_to_impls() {
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
        let ctx = ctx_with_index(root);

        let out = TraceCodeTool
            .execute(
                &ctx,
                r#"{"entity":"make_noise","direction":"callees","edge_kinds":["calls","dispatch"]}"#,
            )
            .await
            .unwrap();
        assert!(
            out.contains("speak @ src/animals.rs:7") && out.contains("speak @ src/animals.rs:12"),
            "dyn 调用应桥接到两个 impl 候选：{out}"
        );
        assert_eq!(
            out.lines()
                .filter(|l| l.contains("make_noise @ src/animals.rs:15"))
                .count(),
            2,
            "应有两条从 make_noise 出发的链：{out}"
        );
    }

    #[tokio::test]
    async fn impact_code_aggregates_by_file() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/a.rs"),
            "pub fn a1() { b(); }\npub fn main() { a2(); }\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/b.rs"),
            "pub fn b() {}\npub fn a2() { b(); }\n",
        )
        .unwrap();
        let ctx = ctx_with_index(root);

        let out = ImpactCodeTool
            .execute(&ctx, r#"{"entity":"b"}"#)
            .await
            .unwrap();
        assert!(
            out.contains("src/a.rs (2 symbols)"),
            "a.rs 应聚合 a1 与 main：{out}"
        );
        assert!(
            out.contains("src/b.rs (1 symbols)"),
            "b.rs 应聚合 a2：{out}"
        );
        assert!(out.contains("a1 @ src/a.rs:1 → b @ src/b.rs:1"), "{out}");
        assert!(
            out.contains("main @ src/a.rs:2 → a2 @ src/b.rs:2 → b @ src/b.rs:1"),
            "{out}"
        );
        assert!(out.contains("total: 2 files, 2 paths"), "{out}");
    }

    #[tokio::test]
    async fn explore_code_groups_entities_by_file() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "pub fn f1() {}\npub fn f2() {}\n").unwrap();
        std::fs::write(root.join("src/b.rs"), "pub fn g() {}\n").unwrap();
        let ctx = ctx_with_index(root);

        let out = ExploreCodeTool
            .execute(&ctx, r#"{"entities":["f1","g","f2"]}"#)
            .await
            .unwrap();
        assert!(out.contains("## src/a.rs"), "{out}");
        assert!(out.contains("## src/b.rs"), "{out}");
        assert!(out.contains("1 | pub fn f1() {}"), "{out}");
        assert!(out.contains("2 | pub fn f2() {}"), "{out}");
        assert!(out.contains("1 | pub fn g() {}"), "{out}");
    }

    #[tokio::test]
    async fn advanced_tools_never_bubble_graph_errors() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub fn only() {}\n").unwrap();
        let ctx = ctx_with_index(root);

        let t = TraceCodeTool.execute(&ctx, r#"{"entity":"no_such"}"#).await;
        assert!(t.is_ok(), "trace must not bubble error");
        assert!(t.unwrap().contains("not found"));

        let i = ImpactCodeTool
            .execute(&ctx, r#"{"entity":"no_such"}"#)
            .await;
        assert!(i.is_ok(), "impact must not bubble error");
        assert!(i.unwrap().contains("not found"));

        let e = ExploreCodeTool
            .execute(&ctx, r#"{"entities":["no_such"]}"#)
            .await;
        assert!(e.is_ok(), "explore must not bubble error");
        assert!(e.unwrap().contains("not found"));
    }
}

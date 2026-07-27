//! 图检索工具：search_code / traverse_graph / retrieve_entity。
//! 索引句柄经 `ToolContext.extensions` 注入（`GraphHandle`），缺失时优雅降级。

use async_trait::async_trait;
use deepseeknova_core::{Tool, ToolContext, ToolSchema};
use deepseeknova_graph::{Direction, EdgeKind, GraphError, NodeKind};
use serde::Deserialize;
use serde_json::json;

/// 共享代码图索引句柄（Task10 由 agent 注入）。
pub type GraphHandle = std::sync::Arc<std::sync::Mutex<deepseeknova_graph::GraphIndex>>;

/// 索引缺失时的降级提示。
const NO_INDEX_MSG: &str = "代码图索引构建中或未启用，请改用 grep 检索。";

/// 从 ctx.extensions 取索引句柄（缺失返回 None，调用方降级）。
fn graph_handle(ctx: &ToolContext) -> Option<GraphHandle> {
    ctx.extensions.get::<GraphHandle>().cloned()
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
            description:
                "按符号名/关键词定位代码实体（函数、结构体、trait 等），替代全片 grep。返回排名后的实体列表：kind、name、path、行区间与签名。"
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Symbol name or keyword to search for."
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["directory", "file", "struct", "enum", "trait", "class", "function", "method"],
                        "description": "Restrict results to this entity kind (optional)."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max results to return (default 10, capped at 50)."
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
        let nodes = idx.search(&parsed.query, kind, limit)?;

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
            description:
                "沿代码图边遍历实体邻居：查找 callers（谁调用它）/callees（它调用谁）等关系，用于影响面与调用链分析。"
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "entity": {
                        "type": "string",
                        "description": "Entity to start from: name, 'path:name', or full id."
                    },
                    "direction": {
                        "type": "string",
                        "enum": ["callers", "callees", "both"],
                        "description": "Traversal direction (default both)."
                    },
                    "edge_kinds": {
                        "type": "array",
                        "items": {
                            "type": "string",
                            "enum": ["contains", "imports", "calls", "implements", "references"]
                        },
                        "description": "Edge kinds to follow (default ['calls'])."
                    },
                    "hops": {
                        "type": "integer",
                        "description": "Max traversal depth (default 2, capped at 3)."
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
            Err(GraphError::EntityNotFound(_)) => {
                return Ok(format!(
                    "entity '{}' not found; try search_code first",
                    parsed.entity
                ));
            }
            Err(e) => return Err(e.into()),
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
            description:
                "按实体名精确取码。skeleton（默认）返回 doc+签名+子实体签名；full 只返回该实体的行区间源码（省 token，优于整文件读取）。"
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "entity": {
                        "type": "string",
                        "description": "Entity to retrieve: name, 'path:name', or full id."
                    },
                    "view": {
                        "type": "string",
                        "enum": ["skeleton", "full"],
                        "description": "skeleton = doc + signatures (default); full = exact source lines of the entity only."
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
                Err(GraphError::EntityNotFound(_)) => {
                    return Ok(format!(
                        "entity '{}' not found; try search_code first",
                        parsed.entity
                    ));
                }
                Err(e) => return Err(e.into()),
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
            Err(GraphError::EntityNotFound(_)) => Ok(format!(
                "entity '{}' not found; try search_code first",
                parsed.entity
            )),
            Err(e) => Err(e.into()),
        }
    }
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
    async fn degrades_without_index() {
        let ctx = ToolContext::new("c2")
            .with_extension(deepseeknova_security::context::SecurityContext::with_safe_defaults());
        let out = SearchCodeTool
            .execute(&ctx, r#"{"query":"x"}"#)
            .await
            .unwrap();
        assert!(out.contains("索引") || out.to_lowercase().contains("index"));
    }
}

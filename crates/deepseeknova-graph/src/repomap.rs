//! token 预算内的骨架 repo map 渲染。

use crate::model::Node;

/// 估算 token 数（沿用项目惯例 chars/4）。
fn est_tokens(s: &str) -> usize { s.chars().count() / 4 }

/// 在 token 预算内渲染骨架 repo map。入参 nodes 由 GraphIndex::repo_map 传入。
pub fn render_repo_map(nodes: &[Node], token_budget: usize) -> String {
    if nodes.is_empty() || token_budget == 0 { return String::new(); }
    let mut ranked: Vec<&Node> = nodes.iter().collect();
    ranked.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

    use std::collections::BTreeMap;
    let mut per_file: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut order: Vec<&str> = Vec::new();
    let mut used = 0usize;

    for n in ranked {
        let line_cost = est_tokens(&n.signature) + 1;
        if used + line_cost > token_budget { continue; }
        if !per_file.contains_key(n.path.as_str()) { order.push(n.path.as_str()); }
        per_file.entry(n.path.as_str()).or_default().push(&n.signature);
        used += line_cost;
    }

    let mut out = String::new();
    for path in order {
        out.push_str(path);
        out.push_str(":\n");
        for sig in &per_file[path] {
            out.push_str("│ ");
            out.push_str(sig);
            out.push('\n');
        }
        out.push_str("⋮\n");
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Node, NodeKind};

    fn node(name: &str, path: &str, sig: &str, score: f64) -> Node {
        Node { id: format!("{path}#{name}#1"), kind: NodeKind::Function, name: name.into(),
               path: path.into(), start_line: 1, end_line: 2, signature: sig.into(),
               doc: String::new(), score }
    }

    #[test]
    fn respects_token_budget_and_orders_by_score() {
        let nodes = vec![
            node("high", "a.rs", "pub fn high()", 0.9),
            node("mid",  "a.rs", "pub fn mid()",  0.5),
            node("low",  "b.rs", "pub fn low()",  0.1),
        ];
        let map = render_repo_map(&nodes, 40);
        assert!(map.contains("a.rs:"));
        assert!(map.contains("pub fn high()"));
        assert!(map.find("high").unwrap() < map.find("mid").unwrap());
        assert!(map.chars().count() <= 40 * 4 + 40);
    }

    #[test]
    fn empty_nodes_yield_empty_map() {
        assert_eq!(render_repo_map(&[], 100), "");
    }
}

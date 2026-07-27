//! 个性化 PageRank（幂迭代）。

use std::collections::HashMap;

/// 个性化 PageRank（幂迭代）。
///
/// - `nodes`：全部节点 id。
/// - `edges`：有向边 (src, dst)。
/// - `personalization`：种子 id 列表；非空时 teleport 只落到种子（个性化），
///   空时均匀 teleport（标准 PageRank）。
/// - `damping`：阻尼系数（典型 0.85）。
/// - `iters`：迭代次数（50 足够收敛）。
///
/// 返回 id → score，所有分数之和为 1。
pub fn pagerank(
    nodes: &[String],
    edges: &[(String, String)],
    personalization: &[String],
    damping: f64,
    iters: usize,
) -> HashMap<String, f64> {
    let n = nodes.len();
    if n == 0 {
        return HashMap::new();
    }
    let idx: HashMap<&str, usize> = nodes.iter().enumerate().map(|(i, s)| (s.as_str(), i)).collect();

    let mut out: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (s, d) in edges {
        if let (Some(&si), Some(&di)) = (idx.get(s.as_str()), idx.get(d.as_str())) {
            out[si].push(di);
        }
    }

    let mut tele = vec![0.0; n];
    let seeds: Vec<usize> = personalization
        .iter()
        .filter_map(|s| idx.get(s.as_str()).copied())
        .collect();
    if seeds.is_empty() {
        for t in tele.iter_mut() {
            *t = 1.0 / n as f64;
        }
    } else {
        for &s in &seeds {
            tele[s] = 1.0 / seeds.len() as f64;
        }
    }

    let mut rank = vec![1.0 / n as f64; n];
    for _ in 0..iters {
        let mut next = vec![0.0; n];
        let mut dangling = 0.0;
        for i in 0..n {
            if out[i].is_empty() {
                dangling += rank[i];
            } else {
                let share = rank[i] / out[i].len() as f64;
                for &j in &out[i] {
                    next[j] += share;
                }
            }
        }
        for i in 0..n {
            next[i] = (1.0 - damping) * tele[i] + damping * (next[i] + dangling * tele[i]);
        }
        rank = next;
    }
    nodes.iter().cloned().zip(rank).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_sum_to_one_and_hub_wins() {
        let edges = vec![("a".to_string(),"c".to_string()), ("b".to_string(),"c".to_string())];
        let nodes = vec!["a".to_string(),"b".to_string(),"c".to_string()];
        let scores = pagerank(&nodes, &edges, &[], 0.85, 50);
        let sum: f64 = scores.values().sum();
        assert!((sum - 1.0).abs() < 1e-6, "sum={sum}");
        assert!(scores["c"] > scores["a"]);
        assert!(scores["c"] > scores["b"]);
    }

    #[test]
    fn personalization_boosts_seed() {
        let edges = vec![("a".to_string(),"b".to_string())];
        let nodes = vec!["a".to_string(),"b".to_string()];
        let base = pagerank(&nodes, &edges, &[], 0.85, 50);
        let seeded = pagerank(&nodes, &edges, &["a".to_string()], 0.85, 50);
        assert!(seeded["a"] > base["a"], "seed should raise its own score");
    }
}

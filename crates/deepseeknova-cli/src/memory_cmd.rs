//! `memory` 子命令的实现（list / edit / delete / replay）。
//!
//! 从 `main.rs` 的 `Commands::Memory` 分支抽出，把过滤、渲染与 I/O 分离，
//! 便于单元测试（P1-11：记忆浏览 / 编辑 / 删除 / 召回回放）。

use deepseeknova_core::memory::engine::MemoryEngine;
use deepseeknova_core::memory::lifecycle::LifecycleMeta;
use deepseeknova_core::memory::store::{MemoryCategory, MemoryEntry, MemoryScoreBreakdown};
use std::io::BufRead;

/// `memory list` 的过滤参数。
#[derive(Debug, Clone, Default)]
pub struct ListFilter<'a> {
    pub stage: Option<&'a str>,
    pub tag: Option<&'a str>,
    pub search: Option<&'a str>,
}

/// 解析 `--category` 值（task/skill/user_profile/all）。
pub fn parse_category(s: &str) -> Result<MemoryCategory, String> {
    match s {
        "task" => Ok(MemoryCategory::Task),
        "skill" => Ok(MemoryCategory::Skill),
        "user_profile" => Ok(MemoryCategory::UserProfile),
        "all" => Err("all".to_string()),
        other => Err(format!(
            "unknown category `{other}` (expected task|skill|user_profile|all)"
        )),
    }
}

/// 应用 stage/tag/search 过滤（全部满足才保留；缺省项不过滤）。
pub fn filter_memories(
    items: Vec<(MemoryEntry, LifecycleMeta)>,
    f: &ListFilter,
) -> Vec<(MemoryEntry, LifecycleMeta)> {
    items
        .into_iter()
        .filter(|(e, meta)| {
            if let Some(stage) = f.stage {
                if meta.stage.as_str() != stage {
                    return false;
                }
            }
            if let Some(tag) = f.tag {
                if !e.tags.iter().any(|t| t == tag) {
                    return false;
                }
            }
            if let Some(kw) = f.search {
                if !e.content.contains(kw) {
                    return false;
                }
            }
            true
        })
        .collect()
}

/// 最近召回的人类可读描述（"N d ago" / "never"）。
fn recency_label(last_recalled_at: Option<i64>) -> String {
    match last_recalled_at {
        Some(ts) => {
            let days = (chrono::Utc::now().timestamp() - ts).max(0) / 86_400;
            if days <= 0 {
                "today".to_string()
            } else {
                format!("{days} d ago")
            }
        }
        None => "never".to_string(),
    }
}

/// 单条记忆的 list 行（id/类目/stage/importance/recall_count/recency）。
pub fn render_entry_line(e: &MemoryEntry, meta: &LifecycleMeta) -> String {
    format!(
        "[{}] ({}) stage={} imp={:.2} recall={} last={}",
        e.id,
        e.category.as_str(),
        meta.stage.as_str(),
        e.importance,
        meta.recall_count,
        recency_label(meta.last_recalled_at),
    )
}

/// `memory list`：拉取 → 过滤 → 分页 → 打印。
pub fn run_list(
    engine: &MemoryEngine,
    category: &str,
    limit: usize,
    offset: usize,
    stage: Option<&str>,
    tag: Option<&str>,
    search: Option<&str>,
) -> Result<(), deepseeknova_core::DeepseeknovaError> {
    let entries: Vec<(MemoryEntry, LifecycleMeta)> = if category == "all" {
        let mut v = Vec::new();
        for c in [
            MemoryCategory::Task,
            MemoryCategory::Skill,
            MemoryCategory::UserProfile,
        ] {
            v.extend(engine.list_with_lifecycle(c)?);
        }
        v
    } else {
        let cat = parse_category(category)
            .map_err(|e| deepseeknova_core::DeepseeknovaError::Config(e.to_string()))?;
        engine.list_with_lifecycle(cat)?
    };
    let filtered = filter_memories(entries, &ListFilter { stage, tag, search });
    let total = filtered.len();
    let page: Vec<_> = filtered.into_iter().skip(offset).take(limit).collect();
    for (e, meta) in &page {
        let preview: String = e.content.chars().take(120).collect();
        println!("{}", render_entry_line(e, meta));
        println!("  {preview}");
    }
    println!(
        "-- {}/{} entries (offset {offset}, limit {limit})",
        page.len(),
        total
    );
    Ok(())
}

/// `memory edit <id> <content...>`：改内容，启用嵌入时强制重算向量。
pub fn run_edit(
    engine: &MemoryEngine,
    id: &str,
    content: &[String],
    embedder_enabled: bool,
) -> Result<(), deepseeknova_core::DeepseeknovaError> {
    let new_content = content.join(" ");
    if new_content.trim().is_empty() {
        return Err(deepseeknova_core::DeepseeknovaError::Config(
            "memory edit <id> <content>：内容不能为空".to_string(),
        ));
    }
    match engine.edit(id, &new_content)? {
        true => {
            let embed = if embedder_enabled {
                "（已重算嵌入）"
            } else {
                "（嵌入未启用）"
            };
            println!("updated memory '{id}'{embed}");
        }
        false => println!("memory '{id}' not found"),
    }
    Ok(())
}

/// 从 `reader` 读取一行并判定确认（y/yes → true；其余 → false）。
pub fn confirm_delete_with<R: BufRead>(
    id: &str,
    reader: &mut R,
) -> Result<bool, deepseeknova_core::DeepseeknovaError> {
    use std::io::Write;
    print!("delete memory '{id}'? [y/N] ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let t = line.trim().to_ascii_lowercase();
    Ok(t == "y" || t == "yes")
}

/// 标准输入确认（`memory delete` 的交互路径）。
pub fn confirm_delete(id: &str) -> Result<bool, deepseeknova_core::DeepseeknovaError> {
    confirm_delete_with(id, &mut std::io::stdin().lock())
}

/// `memory delete <id>`：二次确认（--yes 跳过），删除不可逆。
pub fn run_delete(
    engine: &MemoryEngine,
    id: &str,
    yes: bool,
) -> Result<(), deepseeknova_core::DeepseeknovaError> {
    let confirmed = yes || confirm_delete(id)?;
    if !confirmed {
        println!("aborted: memory '{id}' not deleted");
        return Ok(());
    }
    match engine.forget(id)? {
        true => println!("deleted memory '{id}'"),
        false => println!("memory '{id}' not found"),
    }
    Ok(())
}

/// `memory replay <query>` 的输出渲染（分数分解）。
pub fn render_replay(query: &str, hits: &[MemoryScoreBreakdown]) -> String {
    if hits.is_empty() {
        return format!("no matches for '{query}'");
    }
    let mode = if hits[0].hybrid { "hybrid" } else { "fts" };
    let weight = hits[0].weight;
    let mut out = format!(
        "replay '{query}' — {} hit(s), rank_weight={weight}, mode={mode}\n",
        hits.len()
    );
    for (i, h) in hits.iter().enumerate() {
        out.push_str(&format!(
            "{}. [{}] score={:.4} bm25={:.4} cosine={:.4} lifecycle={:.4}\n",
            i + 1,
            h.entry.id,
            h.score,
            h.bm25,
            h.cosine,
            h.lifecycle,
        ));
        let preview: String = h.snippet.chars().take(140).collect();
        out.push_str(&format!("   {preview}\n"));
    }
    out
}

/// `memory replay <query>`：执行与 recall 同源的混合检索，展示分数分解。
pub fn run_replay(
    engine: &MemoryEngine,
    query: &[String],
    top_k: usize,
    rank_weight: f64,
) -> Result<(), deepseeknova_core::DeepseeknovaError> {
    let q = query.join(" ");
    if q.trim().is_empty() {
        return Err(deepseeknova_core::DeepseeknovaError::Config(
            "memory replay <query>：查询不能为空".to_string(),
        ));
    }
    let hits = engine.replay(&q, top_k, rank_weight)?;
    print!("{}", render_replay(&q, &hits));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_core::memory::engine::MemoryEngine;
    use deepseeknova_core::memory::lifecycle::{LifecycleMeta, MemoryLifecycleStage};
    use deepseeknova_core::memory::store::make_entry;

    fn entry(id: &str, content: &str, tags: Vec<&str>) -> (MemoryEntry, LifecycleMeta) {
        let mut e = make_entry(
            content,
            MemoryCategory::Task,
            tags.into_iter().map(String::from).collect(),
            "t",
            0.6,
        );
        e.id = id.to_string();
        let meta = LifecycleMeta {
            stage: MemoryLifecycleStage::Candidate,
            recall_count: 0,
            last_recalled_at: None,
            created_at: e.created_at,
            importance: e.importance,
        };
        (e, meta)
    }

    fn verified(id: &str, content: &str) -> (MemoryEntry, LifecycleMeta) {
        let (e, mut meta) = entry(id, content, vec![]);
        meta.stage = MemoryLifecycleStage::Verified;
        meta.recall_count = 2;
        meta.last_recalled_at = Some(chrono::Utc::now().timestamp() - 3 * 86_400);
        (e, meta)
    }

    // ── 过滤 ────────────────────────────────────────────────────────────

    #[test]
    fn filter_by_stage() {
        let items = vec![entry("a", "one", vec![]), verified("b", "two")];
        let out = filter_memories(
            items,
            &ListFilter {
                stage: Some("verified"),
                tag: None,
                search: None,
            },
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0.id, "b");
    }

    #[test]
    fn filter_by_tag() {
        let items = vec![
            entry("a", "rust", vec!["rust", "cli"]),
            entry("b", "python", vec!["web"]),
        ];
        let out = filter_memories(
            items,
            &ListFilter {
                stage: None,
                tag: Some("cli"),
                search: None,
            },
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0.id, "a");
    }

    #[test]
    fn filter_by_search_and_combined() {
        let items = vec![
            entry("a", "rust borrow checker", vec!["rust"]),
            entry("b", "rust lifetimes", vec!["rust"]),
            entry("c", "python web", vec!["web"]),
        ];
        let by_search = filter_memories(
            items.clone(),
            &ListFilter {
                stage: None,
                tag: None,
                search: Some("lifetime"),
            },
        );
        assert_eq!(by_search.len(), 1);
        assert_eq!(by_search[0].0.id, "b");
        let combined = filter_memories(
            items,
            &ListFilter {
                stage: None,
                tag: Some("rust"),
                search: Some("borrow"),
            },
        );
        assert_eq!(combined.len(), 1);
        assert_eq!(combined[0].0.id, "a");
    }

    #[test]
    fn parse_category_accepts_known_and_rejects_unknown() {
        assert_eq!(parse_category("task"), Ok(MemoryCategory::Task));
        assert_eq!(parse_category("skill"), Ok(MemoryCategory::Skill));
        assert_eq!(
            parse_category("user_profile"),
            Ok(MemoryCategory::UserProfile)
        );
        assert!(parse_category("all").is_err());
        assert!(parse_category("bogus").is_err());
    }

    #[test]
    fn render_entry_line_shows_lifecycle_fields() {
        let (e, meta) = verified("k", "x");
        let line = render_entry_line(&e, &meta);
        assert!(line.contains("stage=verified"), "{line}");
        assert!(line.contains("imp=0.60"), "{line}");
        assert!(line.contains("recall=2"), "{line}");
        assert!(line.contains("3 d ago"), "{line}");
        // 从未召回 → never
        let (_, m2) = entry("a", "x", vec![]);
        let line2 = render_entry_line(&e, &m2);
        assert!(line2.contains("last=never"), "{line2}");
    }

    // ── edit ────────────────────────────────────────────────────────────

    #[test]
    fn run_edit_updates_content_and_missing_is_reported() {
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        eng.remember("k", "old content", vec![]).unwrap();
        run_edit(
            &eng,
            "k",
            &[String::from("new"), String::from("content")],
            false,
        )
        .unwrap();
        let items = eng.list_with_lifecycle(MemoryCategory::Task).unwrap();
        assert_eq!(items[0].0.content, "new content");
        // 缺失 id：edit 返回 false，内容不变。
        let before = eng.stats().unwrap().total;
        run_edit(&eng, "nope", &["x".into()], false).unwrap();
        assert_eq!(eng.stats().unwrap().total, before);
        // 空内容 → 报错。
        assert!(run_edit(&eng, "k", &[], false).is_err());
    }

    #[test]
    fn run_edit_reembeds_when_embedder_enabled() {
        // CLI 层 edit 必须触发 engine::edit 的强制重嵌：旧语义（ferris 向量）
        // 被新内容（plain → 正交向量）覆盖。
        use deepseeknova_core::memory::embedding::EmbeddingProvider;
        use deepseeknova_core::DeepseeknovaError;
        struct Fake;
        impl EmbeddingProvider for Fake {
            fn embed(&self, text: &str) -> Result<Vec<f32>, DeepseeknovaError> {
                if text.contains("ferris") {
                    Ok(vec![1.0, 0.0])
                } else {
                    Ok(vec![0.0, 1.0])
                }
            }
        }
        let eng = MemoryEngine::open_in_memory_with_embedder(
            true,
            Some(std::sync::Arc::new(Fake)),
            Some("test-model".to_string()),
        )
        .unwrap();
        eng.remember("k", "ferris crab language", vec![]).unwrap();
        run_edit(&eng, "k", &["plain note".into()], true).unwrap();
        // 旧语义：query "ferris" 与重算后的向量正交 → 不应再命中。
        let ferris_hits = eng.replay("ferris", 5, 0.3).unwrap();
        assert!(
            ferris_hits.is_empty(),
            "重嵌后旧语义（ferris）必须消失: {ferris_hits:?}"
        );
        // 新语义：query "plain" 应命中。
        let plain_hits = eng.replay("plain", 5, 0.3).unwrap();
        assert!(
            plain_hits.iter().any(|h| h.entry.id == "k"),
            "重嵌后新语义必须可召回"
        );
    }

    // ── delete ──────────────────────────────────────────────────────────

    #[test]
    fn confirm_delete_with_parses_answer() {
        let mut yes = std::io::Cursor::new(b"y\n".to_vec());
        assert!(confirm_delete_with("k", &mut yes).unwrap());
        let mut yes_word = std::io::Cursor::new(b"YES\n".to_vec());
        assert!(confirm_delete_with("k", &mut yes_word).unwrap());
        let mut no = std::io::Cursor::new(b"n\n".to_vec());
        assert!(!confirm_delete_with("k", &mut no).unwrap());
        let mut empty = std::io::Cursor::new(b"\n".to_vec());
        assert!(!confirm_delete_with("k", &mut empty).unwrap());
    }

    #[test]
    fn run_delete_with_yes_removes_and_reports_missing() {
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        eng.remember("k", "to delete", vec![]).unwrap();
        run_delete(&eng, "k", true).unwrap();
        assert!(eng.list(MemoryCategory::Task).unwrap().is_empty());
        // 已删除：not found。
        run_delete(&eng, "k", true).unwrap();
        assert!(eng.list(MemoryCategory::Task).unwrap().is_empty());
    }

    // ── replay 渲染 ─────────────────────────────────────────────────────

    #[test]
    fn render_replay_lists_breakdown_columns() {
        let (e, _) = entry("k", "rust borrow checker", vec![]);
        let hits = vec![MemoryScoreBreakdown {
            entry: e,
            snippet: "rust borrow checker".to_string(),
            score: 0.42,
            bm25: 0.5,
            cosine: 0.3,
            lifecycle: -0.38,
            weight: 0.3,
            hybrid: true,
        }];
        let out = render_replay("rust", &hits);
        assert!(
            out.contains("replay 'rust' — 1 hit(s), rank_weight=0.3, mode=hybrid"),
            "{out}"
        );
        assert!(
            out.contains("score=0.4200 bm25=0.5000 cosine=0.3000 lifecycle=-0.3800"),
            "{out}"
        );
        assert!(out.contains("rust borrow checker"), "{out}");
        // 空命中。
        let empty = render_replay("zzz", &[]);
        assert_eq!(empty, "no matches for 'zzz'");
    }

    #[test]
    fn run_replay_executes_and_renders() {
        let eng = MemoryEngine::open_in_memory(true).unwrap();
        eng.remember("k", "rust borrow checker", vec![]).unwrap();
        // 直接调用 run_replay 会打印到 stdout；此处验证其返回与底层行为一致。
        run_replay(&eng, &["rust".into()], 5, 0.3).unwrap();
        let hits = eng.replay("rust", 5, 0.3).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(!hits[0].hybrid);
        // 空查询 → 报错。
        assert!(run_replay(&eng, &[], 5, 0.3).is_err());
    }
}

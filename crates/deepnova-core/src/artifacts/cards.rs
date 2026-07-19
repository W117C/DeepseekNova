//! # Knowledge Cards
//!
//! Generates structured knowledge cards from project experience.
//! Each card captures a key decision, lesson learned, or reusable pattern.
//!
//! ## Card Format (Markdown)
//!
//! ```markdown
//! ---
//! id: card-001
//! title: "Use FTS5 for memory recall"
//! tags: [search, sqlite, memory]
//! created: 2026-07-19
//! source: project-alpha
//! ---
//!
//! ## Context
//! ...
//!
//! ## Key Insight
//! ...
//!
//! ## Code Example
//! ...
//!
//! ## Related
//! - [[card-002]]
//! ```

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::info;

/// A knowledge card.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeCard {
    pub id: String,
    pub title: String,
    pub tags: Vec<String>,
    pub created: String,
    pub source: String,
    pub context: String,
    pub key_insight: String,
    pub code_example: Option<String>,
    pub related: Vec<String>,
}

impl KnowledgeCard {
    pub fn to_markdown(&self) -> String {
        let mut md = format!(
            "---\nid: {}\ntitle: \"{}\"\ntags: {:?}\ncreated: {}\nsource: {}\n---\n\n",
            self.id, self.title, self.tags, self.created, self.source
        );
        md.push_str("## Context\n\n");
        md.push_str(&self.context);
        md.push_str("\n\n## Key Insight\n\n");
        md.push_str(&self.key_insight);

        if let Some(ref code) = self.code_example {
            md.push_str("\n\n## Code Example\n\n");
            md.push_str(&format!("```\n{}\n```", code));
        }

        if !self.related.is_empty() {
            md.push_str("\n\n## Related\n\n");
            for r in &self.related {
                md.push_str(&format!("- [[{}]]\n", r));
            }
        }

        md.push('\n');
        md
    }

    pub fn from_markdown(content: &str) -> Option<Self> {
        let content = content.trim();
        if !content.starts_with("---") {
            return None;
        }
        let end = content[3..].find("---")?;
        let yaml = &content[3..3 + end];
        let body = content[3 + end + 3..].trim();

        let frontmatter: KnowledgeCardFrontmatter = serde_yaml::from_str(yaml).ok()?;

        // Parse body sections
        let context = extract_section(body, "Context").unwrap_or_default();
        let key_insight = extract_section(body, "Key Insight").unwrap_or_default();
        let code_example = extract_section(body, "Code Example");
        let related = extract_section(body, "Related")
            .map(|s| {
                s.lines()
                    .filter_map(|l| {
                        let l = l.trim();
                        l.strip_prefix("- [[")
                            .and_then(|l| l.strip_suffix("]]"))
                            .map(|s| s.to_string())
                    })
                    .collect()
            })
            .unwrap_or_default();

        Some(Self {
            id: frontmatter.id,
            title: frontmatter.title,
            tags: frontmatter.tags.unwrap_or_default(),
            created: frontmatter.created,
            source: frontmatter.source,
            context,
            key_insight,
            code_example,
            related,
        })
    }
}

#[derive(Debug, Deserialize)]
struct KnowledgeCardFrontmatter {
    id: String,
    title: String,
    #[serde(default)]
    tags: Option<Vec<String>>,
    created: String,
    source: String,
}

fn extract_section(body: &str, section_name: &str) -> Option<String> {
    let header = format!("## {}", section_name);
    let start = body.find(&header)? + header.len();
    let rest = &body[start..];
    let end = rest.find("## ").unwrap_or(rest.len());
    Some(rest[..end].trim().to_string())
}

/// Knowledge card generator.
pub struct CardGenerator {
    cards: Vec<KnowledgeCard>,
    output_dir: PathBuf,
}

impl CardGenerator {
    pub fn new(output_dir: impl Into<PathBuf>) -> Self {
        Self {
            cards: Vec::new(),
            output_dir: output_dir.into(),
        }
    }

    pub fn add_card(&mut self, card: KnowledgeCard) {
        self.cards.push(card);
    }

    pub fn generate(&self) -> anyhow::Result<Vec<PathBuf>> {
        std::fs::create_dir_all(&self.output_dir)?;
        let mut written = Vec::new();
        for card in &self.cards {
            let path = self.output_dir.join(format!("{}.md", card.id));
            std::fs::write(&path, card.to_markdown())?;
            written.push(path);
        }
        info!(count = written.len(), "knowledge cards generated");
        Ok(written)
    }

    pub fn card_count(&self) -> usize {
        self.cards.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_card_roundtrip() {
        let card = KnowledgeCard {
            id: "card-001".into(),
            title: "Use FTS5 for memory recall".into(),
            tags: vec!["search".into(), "sqlite".into()],
            created: "2026-07-19".into(),
            source: "deepnova".into(),
            context: "Need fast full-text search across sessions".into(),
            key_insight: "SQLite FTS5 with BM25 ranking gives millisecond recall".into(),
            code_example: Some("CREATE VIRTUAL TABLE memory_fts USING fts5(content);".into()),
            related: vec!["card-002".into()],
        };

        let md = card.to_markdown();
        assert!(md.contains("card-001"));
        assert!(md.contains("FTS5"));
        assert!(md.contains("sqlite"));

        let parsed = KnowledgeCard::from_markdown(&md).unwrap();
        assert_eq!(parsed.id, card.id);
        assert_eq!(parsed.title, card.title);
        assert!(parsed.context.contains("full-text search"));
        assert!(parsed.key_insight.contains("BM25"));
        assert!(parsed.related.contains(&"card-002".to_string()));
    }

    #[test]
    fn test_card_generation() {
        let dir = tempfile::tempdir().unwrap();
        let mut gen = CardGenerator::new(dir.path());
        gen.add_card(KnowledgeCard {
            id: "card-001".into(),
            title: "Test".into(),
            tags: vec![],
            created: "2026-01-01".into(),
            source: "test".into(),
            context: "Context".into(),
            key_insight: "Insight".into(),
            code_example: None,
            related: vec![],
        });
        gen.add_card(KnowledgeCard {
            id: "card-002".into(),
            title: "Test 2".into(),
            tags: vec!["test".into()],
            created: "2026-01-02".into(),
            source: "test".into(),
            context: "Context 2".into(),
            key_insight: "Insight 2".into(),
            code_example: None,
            related: vec!["card-001".into()],
        });

        assert_eq!(gen.card_count(), 2);
        let files = gen.generate().unwrap();
        assert_eq!(files.len(), 2);
    }
}

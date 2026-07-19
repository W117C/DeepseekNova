//! # Repo Wiki Generator
//!
//! Generates a structured project wiki from conversation history, code changes,
//! and project metadata. Output is Markdown files suitable for GitHub Wiki.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::info;

/// Configuration for wiki generation.
#[derive(Debug, Clone)]
pub struct WikiConfig {
    /// Output directory for wiki files.
    pub output_dir: PathBuf,
    /// Whether to include architecture decision records.
    pub include_adrs: bool,
    /// Whether to include an API reference section.
    pub include_api: bool,
    /// Whether to include a dependency graph.
    pub include_deps: bool,
    /// Whether to include a changelog.
    pub include_changelog: bool,
}

impl Default for WikiConfig {
    fn default() -> Self {
        Self {
            output_dir: PathBuf::from("wiki"),
            include_adrs: true,
            include_api: true,
            include_deps: true,
            include_changelog: true,
        }
    }
}

/// An architecture decision record (ADR).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Adr {
    pub id: String,
    pub title: String,
    pub status: AdrStatus,
    pub date: String,
    pub context: String,
    pub decision: String,
    pub consequences: String,
    pub alternatives: Vec<String>,
}

/// ADR status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AdrStatus {
    Proposed,
    Accepted,
    Deprecated,
    Superseded,
}

impl Adr {
    pub fn to_markdown(&self) -> String {
        format!(
            "# ADR-{id}: {title}\n\n\
             - **Status**: {status}\n\
             - **Date**: {date}\n\n\
             ## Context\n\n{context}\n\n\
             ## Decision\n\n{decision}\n\n\
             ## Alternatives Considered\n\n{alternatives}\n\n\
             ## Consequences\n\n{consequences}\n",
            id = self.id,
            title = self.title,
            status = match self.status {
                AdrStatus::Proposed => "Proposed",
                AdrStatus::Accepted => "Accepted",
                AdrStatus::Deprecated => "Deprecated",
                AdrStatus::Superseded => "Superseded",
            },
            date = self.date,
            context = self.context,
            decision = self.decision,
            alternatives = self
                .alternatives
                .iter()
                .enumerate()
                .map(|(i, a)| format!("{}. {}", i + 1, a))
                .collect::<Vec<_>>()
                .join("\n"),
            consequences = self.consequences,
        )
    }
}

/// A project wiki entry (a single wiki page).
#[derive(Debug, Clone)]
pub struct WikiPage {
    pub title: String,
    pub filename: String,
    pub content: String,
    pub category: PageCategory,
}

/// Wiki page category.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PageCategory {
    Home,
    Architecture,
    Adr,
    Api,
    Guide,
    Changelog,
    Dependency,
}

impl PageCategory {
    pub fn dir(&self) -> &str {
        match self {
            Self::Home => "",
            Self::Architecture => "architecture",
            Self::Adr => "adr",
            Self::Api => "api",
            Self::Guide => "guides",
            Self::Changelog => "",
            Self::Dependency => "dependencies",
        }
    }
}

/// Wiki generator — collects pages and writes them to disk.
pub struct WikiGenerator {
    config: WikiConfig,
    pages: Vec<WikiPage>,
}

impl WikiGenerator {
    pub fn new(config: WikiConfig) -> Self {
        Self {
            config,
            pages: Vec::new(),
        }
    }

    /// Add a pre-built page.
    pub fn add_page(&mut self, page: WikiPage) {
        self.pages.push(page);
    }

    /// Add an ADR page.
    pub fn add_adr(&mut self, adr: Adr) {
        self.pages.push(WikiPage {
            title: format!("ADR-{}: {}", adr.id, adr.title),
            filename: format!("adr-{}.md", adr.id),
            content: adr.to_markdown(),
            category: PageCategory::Adr,
        });
    }

    /// Generate the home page from a project summary.
    pub fn add_home_page(&mut self, summary: &ProjectSummary) {
        let mut content = format!("# {}\n\n", summary.name);
        content.push_str(&format!("{}\n\n", summary.description));

        if !summary.modules.is_empty() {
            content.push_str("## Modules\n\n");
            for module in &summary.modules {
                content.push_str(&format!(
                    "- [{}](architecture/{}) — {}\n",
                    module.name, module.doc_link, module.description
                ));
            }
            content.push('\n');
        }

        if !summary.key_decisions.is_empty() {
            content.push_str("## Key Decisions\n\n");
            for (i, decision) in summary.key_decisions.iter().enumerate() {
                content.push_str(&format!("{}. {}\n", i + 1, decision));
            }
            content.push('\n');
        }

        if !summary.metrics.is_empty() {
            content.push_str("## Metrics\n\n");
            content.push_str("| Metric | Value |\n|--------|-------|\n");
            for (key, value) in &summary.metrics {
                content.push_str(&format!("| {} | {} |\n", key, value));
            }
        }

        self.pages.push(WikiPage {
            title: "Home".to_string(),
            filename: "Home.md".to_string(),
            content,
            category: PageCategory::Home,
        });
    }

    /// Write all pages to disk.
    pub fn generate(&self) -> anyhow::Result<Vec<PathBuf>> {
        std::fs::create_dir_all(&self.config.output_dir)?;
        let mut written = Vec::new();

        for page in &self.pages {
            let dir = self.config.output_dir.join(page.category.dir());
            if !page.category.dir().is_empty() {
                std::fs::create_dir_all(&dir)?;
            }
            let path = dir.join(&page.filename);
            std::fs::write(&path, &page.content)?;
            written.push(path);
        }

        // Generate _Sidebar.md
        let sidebar = self.generate_sidebar();
        let sidebar_path = self.config.output_dir.join("_Sidebar.md");
        std::fs::write(&sidebar_path, sidebar)?;
        written.push(sidebar_path);

        info!(pages = written.len(), "wiki generated");
        Ok(written)
    }

    /// Generate the wiki sidebar navigation.
    fn generate_sidebar(&self) -> String {
        let mut sections: HashMap<PageCategory, Vec<&WikiPage>> = HashMap::new();
        for page in &self.pages {
            sections
                .entry(page.category.clone())
                .or_default()
                .push(page);
        }

        let mut sidebar = String::from("## Navigation\n\n");
        for cat in [
            PageCategory::Home,
            PageCategory::Architecture,
            PageCategory::Adr,
            PageCategory::Api,
            PageCategory::Guide,
            PageCategory::Dependency,
            PageCategory::Changelog,
        ] {
            if let Some(pages) = sections.get(&cat) {
                let title = match cat {
                    PageCategory::Home => "Home",
                    PageCategory::Architecture => "Architecture",
                    PageCategory::Adr => "Decisions (ADRs)",
                    PageCategory::Api => "API Reference",
                    PageCategory::Guide => "Guides",
                    PageCategory::Dependency => "Dependencies",
                    PageCategory::Changelog => "Changelog",
                };
                sidebar.push_str(&format!("**{}**\n", title));
                for page in pages {
                    let link = if page.category.dir().is_empty() {
                        page.filename.replace(".md", "")
                    } else {
                        format!(
                            "{}/{}",
                            page.category.dir(),
                            page.filename.replace(".md", "")
                        )
                    };
                    sidebar.push_str(&format!("- [{}]({})\n", page.title, link));
                }
                sidebar.push('\n');
            }
        }

        sidebar
    }
}

/// Project summary for the home page.
#[derive(Debug, Clone)]
pub struct ProjectSummary {
    pub name: String,
    pub description: String,
    pub modules: Vec<ModuleSummary>,
    pub key_decisions: Vec<String>,
    pub metrics: Vec<(String, String)>,
}

/// Module summary.
#[derive(Debug, Clone)]
pub struct ModuleSummary {
    pub name: String,
    pub description: String,
    pub doc_link: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adr_markdown() {
        let adr = Adr {
            id: "001".into(),
            title: "Use SQLite for memory storage".into(),
            status: AdrStatus::Accepted,
            date: "2026-07-19".into(),
            context: "Need fast full-text search across sessions".into(),
            decision: "Use SQLite with FTS5 extension".into(),
            consequences: "Adds SQLite dependency but enables millisecond recall".into(),
            alternatives: vec!["PostgreSQL".into(), "In-memory hash".into()],
        };
        let md = adr.to_markdown();
        assert!(md.contains("ADR-001"));
        assert!(md.contains("Accepted"));
        assert!(md.contains("SQLite"));
    }

    #[test]
    fn test_wiki_generation() {
        let dir = tempfile::tempdir().unwrap();
        let config = WikiConfig {
            output_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let mut gen = WikiGenerator::new(config);
        gen.add_home_page(&ProjectSummary {
            name: "DeepNova".into(),
            description: "Agent framework".into(),
            modules: vec![ModuleSummary {
                name: "core".into(),
                description: "Core types".into(),
                doc_link: "core".into(),
            }],
            key_decisions: vec!["Use Rust".into()],
            metrics: vec![("Tests".into(), "375".into())],
        });
        gen.add_adr(Adr {
            id: "001".into(),
            title: "Use Rust".into(),
            status: AdrStatus::Accepted,
            date: "2026-07-19".into(),
            context: "Need performance and safety".into(),
            decision: "Rust".into(),
            consequences: "Great perf".into(),
            alternatives: vec![],
        });

        let files = gen.generate().unwrap();
        assert!(files.len() >= 3); // Home + ADR + Sidebar
        assert!(files.iter().any(|f| f.file_name().unwrap() == "Home.md"));
        assert!(files.iter().any(|f| f.file_name().unwrap() == "adr-001.md"));
        assert!(files
            .iter()
            .any(|f| f.file_name().unwrap() == "_Sidebar.md"));
    }
}

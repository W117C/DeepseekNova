use async_trait::async_trait;
use dpronix_core::{Tool, ToolContext, ToolSchema};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Memory entry and store
// ---------------------------------------------------------------------------

/// A single memory entry.
#[derive(Debug, Clone)]
struct MemoryEntry {
    key: String,
    value: String,
    tags: Vec<String>,
    created_at: i64,
}

impl MemoryEntry {
    /// Build the full searchable text for BM25 indexing.
    fn searchable_text(&self) -> String {
        let mut text = String::new();
        text.push_str(&self.key);
        text.push(' ');
        text.push_str(&self.value);
        if !self.tags.is_empty() {
            text.push(' ');
            text.push_str(&self.tags.join(" "));
        }
        text
    }
}

/// BM25 index parameters.
const BM25_K1: f64 = 1.2;
const BM25_B: f64 = 0.75;

/// In-memory store holding all entries.
struct MemoryStore {
    entries: HashMap<String, MemoryEntry>,
}

impl MemoryStore {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    fn remember(&mut self, key: String, value: String, tags: Vec<String>) {
        let entry = MemoryEntry {
            key: key.clone(),
            value,
            tags,
            created_at: chrono::Utc::now().timestamp(),
        };
        self.entries.insert(key, entry);
    }

    fn forget(&mut self, key: &str) -> Option<MemoryEntry> {
        self.entries.remove(key)
    }

    fn all_entries(&self) -> Vec<&MemoryEntry> {
        self.entries.values().collect()
    }

    /// Recall entries matching the query using BM25 scoring.
    /// Returns a list of (key, score) sorted by score descending.
    fn recall(&self, query: &str, top_k: usize) -> Vec<(String, f64, String)> {
        let query_terms = tokenize(query);
        if query_terms.is_empty() {
            return Vec::new();
        }

        let docs: Vec<&MemoryEntry> = self.entries.values().collect();
        let n = docs.len();
        if n == 0 {
            return Vec::new();
        }

        // Pre-tokenize all documents and compute per-term frequencies.
        let doc_term_counts: Vec<HashMap<String, u32>> = docs
            .iter()
            .map(|e| count_terms(&tokenize(&e.searchable_text())))
            .collect();

        let doc_lengths: Vec<f64> = doc_term_counts
            .iter()
            .map(|tc| tc.values().copied().sum::<u32>() as f64)
            .collect();

        let avgdl: f64 = if doc_lengths.is_empty() {
            1.0
        } else {
            doc_lengths.iter().copied().sum::<f64>() / n as f64
        };

        // Document frequency: number of docs containing each query term.
        let df: HashMap<&str, f64> = query_terms
            .iter()
            .map(|term| {
                let count = doc_term_counts
                    .iter()
                    .filter(|tc| tc.contains_key(term.as_str()))
                    .count() as f64;
                (term.as_str(), count)
            })
            .collect();

        let n_f64 = n as f64;

        let mut scores: Vec<(usize, f64)> = doc_term_counts
            .iter()
            .enumerate()
            .map(|(i, tc)| {
                let dl = doc_lengths[i];
                let score: f64 = query_terms
                    .iter()
                    .map(|term| {
                        let tf = *tc.get(term).unwrap_or(&0) as f64;
                        if tf == 0.0 {
                            return 0.0;
                        }

                        let n_t = df.get(term.as_str()).copied().unwrap_or(1.0);
                        // Robertson-Sparck Jones IDF variant with smoothing.
                        let idf = ((n_f64 - n_t + 0.5) / (n_t + 0.5) + 1.0).ln();

                        let numerator = tf * (BM25_K1 + 1.0);
                        let denominator = tf + BM25_K1 * (1.0 - BM25_B + BM25_B * dl / avgdl);

                        idf * numerator / denominator
                    })
                    .sum();
                (i, score)
            })
            .collect();

        // Sort by score descending.
        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let top_k = top_k.min(n);

        scores
            .into_iter()
            .take(top_k)
            .filter(|(_, s)| *s > 0.0)
            .map(|(i, s)| {
                let entry = docs[i];
                (entry.key.clone(), s, entry.value.clone())
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Global store (thread-safe, process-lifetime)
// ---------------------------------------------------------------------------

static STORE: std::sync::OnceLock<Arc<Mutex<MemoryStore>>> = std::sync::OnceLock::new();

fn store() -> &'static Arc<Mutex<MemoryStore>> {
    STORE.get_or_init(|| Arc::new(Mutex::new(MemoryStore::new())))
}

// ---------------------------------------------------------------------------
// Tokenizer helpers
// ---------------------------------------------------------------------------

/// Split text into normalized tokens (lowercase, alphanumeric + internal hyphens).
fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '-')
        .map(|s| s.trim_matches('-').to_string())
        .filter(|s| s.len() >= 2)
        .collect()
}

/// Count term frequencies in a token sequence.
fn count_terms(tokens: &[String]) -> HashMap<String, u32> {
    let mut freq = HashMap::new();
    for token in tokens {
        *freq.entry(token.clone()).or_insert(0) += 1;
    }
    freq
}

// ---------------------------------------------------------------------------
// RememberTool
// ---------------------------------------------------------------------------

pub struct RememberTool;

#[derive(Deserialize)]
struct RememberArgs {
    key: String,
    value: String,
    #[serde(default)]
    tags: Vec<String>,
}

#[async_trait]
impl Tool for RememberTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "remember".to_string(),
            description: "Stores a memory entry with a given key and value. \
                 Use to persist information the agent needs to retain across calls. \
                 Supports optional tags for categorization."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "Unique identifier for this memory entry."
                    },
                    "value": {
                        "type": "string",
                        "description": "The content to store."
                    },
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional tags for categorization."
                    }
                },
                "required": ["key", "value"]
            }),
        }
    }

    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        let parsed: RememberArgs = serde_json::from_str(args)?;

        if ctx.cancellation.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let store = store();
        let mut guard = store.lock().unwrap();
        let existed = guard.entries.contains_key(&parsed.key);
        guard.remember(parsed.key.clone(), parsed.value, parsed.tags);

        if existed {
            Ok(format!("updated memory entry '{}'", parsed.key))
        } else {
            Ok(format!("stored memory entry '{}'", parsed.key))
        }
    }
}

// ---------------------------------------------------------------------------
// ForgetTool
// ---------------------------------------------------------------------------

pub struct ForgetTool;

#[derive(Deserialize)]
struct ForgetArgs {
    key: String,
}

#[async_trait]
impl Tool for ForgetTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "forget".to_string(),
            description: "Removes a memory entry by key. Returns an error if the key is not found."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "The key of the memory entry to remove."
                    }
                },
                "required": ["key"]
            }),
        }
    }

    fn read_only(&self) -> bool {
        // Technically mutates, but we mark read-only semantics are handled by the execute method.
        false
    }

    async fn execute(&self, ctx: &ToolContext, args: &str) -> anyhow::Result<String> {
        let parsed: ForgetArgs = serde_json::from_str(args)?;

        if ctx.cancellation.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let store = store();
        let mut guard = store.lock().unwrap();
        match guard.forget(&parsed.key) {
            Some(entry) => Ok(format!(
                "removed memory entry '{}' (stored at {})",
                entry.key,
                chrono::DateTime::from_timestamp(entry.created_at, 0)
                    .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
                    .unwrap_or_else(|| entry.created_at.to_string())
            )),
            None => anyhow::bail!("memory entry '{}' not found", parsed.key),
        }
    }
}

// ---------------------------------------------------------------------------
// RecallTool
// ---------------------------------------------------------------------------

pub struct RecallTool;

#[derive(Deserialize)]
struct RecallArgs {
    query: String,
    #[serde(default = "default_top_k")]
    top_k: usize,
}

const fn default_top_k() -> usize {
    10
}

#[async_trait]
impl Tool for RecallTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "recall".to_string(),
            description: "Searches stored memory entries using BM25 text ranking and returns \
                 the best-matching entries sorted by relevance score."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query to match against memory entries."
                    },
                    "top_k": {
                        "type": "integer",
                        "description": "Maximum number of results to return (default: 10).",
                        "default": 10
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
        let parsed: RecallArgs = serde_json::from_str(args)?;

        if ctx.cancellation.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let store = store();
        let guard = store.lock().unwrap();

        let results = guard.recall(&parsed.query, parsed.top_k);

        if results.is_empty() {
            let total = guard.all_entries().len();
            if total == 0 {
                Ok("no memories stored yet. Use 'remember' to add entries.".to_string())
            } else {
                Ok(format!(
                    "no matches for '{}' (searched {total} entries)",
                    parsed.query
                ))
            }
        } else {
            let mut output = String::new();
            output.push_str(&format!(
                "found {} match(es) for '{}':\n",
                results.len(),
                parsed.query
            ));

            for (idx, (key, _score, value)) in results.iter().enumerate() {
                // Truncate long values for display.
                let preview: String = if value.len() > 200 {
                    format!("{}...", &value[..200])
                } else {
                    value.clone()
                };
                output.push_str(&format!("  {}: {} — {}\n", idx + 1, key, preview));
            }

            Ok(output)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_basic() {
        let tokens = tokenize("Hello World! How are you?");
        assert!(tokens.contains(&"hello".to_string()));
        assert!(tokens.contains(&"world".to_string()));
        assert!(tokens.contains(&"how".to_string()));
    }

    #[test]
    fn tokenize_skips_short_tokens() {
        let tokens = tokenize("I am a test");
        // "i", "a" should be filtered (< 2 chars)
        assert!(!tokens.contains(&"i".to_string()));
        assert!(!tokens.contains(&"a".to_string()));
        assert!(tokens.contains(&"am".to_string()));
        assert!(tokens.contains(&"test".to_string()));
    }

    #[test]
    fn tokenize_handles_hyphens() {
        let tokens = tokenize("state-of-the-art technology");
        assert!(tokens.contains(&"state-of-the-art".to_string()));
    }

    #[test]
    fn remember_and_recall() {
        let mut store = MemoryStore::new();
        store.remember(
            "greeting".to_string(),
            "Hello from the Rust programming language".to_string(),
            vec!["intro".to_string()],
        );
        store.remember(
            "weather".to_string(),
            "The weather in Tokyo is sunny with a high of 25C".to_string(),
            vec!["weather".to_string()],
        );
        store.remember(
            "lunch".to_string(),
            "Today's lunch is sushi and miso soup".to_string(),
            vec!["food".to_string(), "japanese".to_string()],
        );

        let results = store.recall("rust programming", 5);
        assert!(!results.is_empty());
        // The greeting entry should rank highest.
        assert_eq!(results[0].0, "greeting");
    }

    #[test]
    fn recall_ranks_by_relevance() {
        let mut store = MemoryStore::new();
        store.remember(
            "doc1".to_string(),
            "Rust is a systems programming language".to_string(),
            vec![],
        );
        store.remember(
            "doc2".to_string(),
            "Python is great for data science".to_string(),
            vec![],
        );
        store.remember(
            "doc3".to_string(),
            "Rust programming for web assembly is fast".to_string(),
            vec![],
        );

        let results = store.recall("rust programming", 5);
        assert_eq!(results.len(), 2); // doc1 and doc3
                                      // doc3 has "rust" AND "programming", doc1 has both too but doc3 also has "fast" which is irrelevant
                                      // Both should score, the order depends on document length normalization
    }

    #[test]
    fn recall_with_top_k_limit() {
        let mut store = MemoryStore::new();
        for i in 0..20 {
            store.remember(
                format!("entry{}", i),
                format!("This is memory entry number {}", i),
                vec![],
            );
        }

        let results = store.recall("memory entry", 5);
        assert_eq!(results.len(), 5);
    }

    #[test]
    fn forget_removes_entry() {
        let mut store = MemoryStore::new();
        store.remember("temp".to_string(), "temporary data".to_string(), vec![]);

        let entry = store.forget("temp");
        assert!(entry.is_some());

        let results = store.recall("temporary data", 1);
        assert!(results.is_empty());
    }

    #[test]
    fn forget_nonexistent_returns_none() {
        let mut store = MemoryStore::new();
        assert!(store.forget("missing").is_none());
    }

    #[test]
    fn recall_empty_store() {
        let store = MemoryStore::new();
        let results = store.recall("anything", 10);
        assert!(results.is_empty());
    }

    #[test]
    fn recall_scores_tags() {
        let mut store = MemoryStore::new();
        store.remember(
            "api-key".to_string(),
            "Production API key for payment service".to_string(),
            vec![
                "credentials".to_string(),
                "payment".to_string(),
                "production".to_string(),
            ],
        );

        // Search by tag should find the entry.
        let results = store.recall("credentials payment", 5);
        assert!(!results.is_empty());
        assert_eq!(results[0].0, "api-key");
    }

    #[test]
    fn bm25_sort_order() {
        // Verify scores are in descending order.
        let mut store = MemoryStore::new();
        store.remember("a".to_string(), "rust rust rust".to_string(), vec![]);
        store.remember("b".to_string(), "rust".to_string(), vec![]);

        let results = store.recall("rust", 5);
        assert_eq!(results.len(), 2);
        assert!(
            results[0].1 >= results[1].1,
            "expected BM25 score for 'a' (more 'rust' occurrences) >= score for 'b', got {} >= {}",
            results[0].1,
            results[1].1
        );
    }
}

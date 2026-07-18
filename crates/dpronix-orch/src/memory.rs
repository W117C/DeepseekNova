//! # Vector Memory — Persistent, Searchable Agent Memory
//!
//! Inspired by Ruflo's AgentDB (HNSW vector database) and ECC's Instinct system.
//! Provides semantic memory for agents: store observations, retrieve by similarity,
//! and learn from past sessions.
//!
//! ## Architecture
//!
//! ```text
//! Agent Experience → MemoryRecord → VectorStore (HNSW / brute-force)
//!                                       ↓
//!                                 Similarity Search
//!                                       ↓
//!                               Relevant Past Experiences
//! ```
//!
//! ## DeepSeek-V4 optimizations
//!
//! - Memory records include `reasoning_content` for thinking mode continuity
//! - Cache-aware storage format for fast retrieval
//! - Session-level memory compaction

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tracing::info;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A single memory record stored in the vector database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRecord {
    /// Unique identifier.
    pub id: String,
    /// Timestamp of when the memory was created.
    pub created_at: i64,
    /// The text content of the memory.
    pub content: String,
    /// Optional reasoning content (for DeepSeek thinking mode continuity).
    pub reasoning: Option<String>,
    /// The embedding vector for similarity search.
    #[serde(default)]
    pub embedding: Vec<f32>,
    /// Metadata tags for filtering.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Source agent or session.
    pub source: String,
    /// Importance score (0.0 - 1.0) for memory compaction.
    #[serde(default)]
    pub importance: f32,
}

/// Search result with similarity score.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub record: MemoryRecord,
    pub score: f32,
}

/// Configuration for the vector memory store.
#[derive(Debug, Clone)]
pub struct MemoryConfig {
    /// Dimension of embedding vectors.
    pub vector_dim: usize,
    /// Maximum number of records to keep.
    pub max_records: usize,
    /// Similarity threshold for search (0.0 - 1.0).
    pub similarity_threshold: f32,
    /// Whether to persist to disk.
    pub persist: bool,
    /// Path for persistence.
    pub persist_path: Option<PathBuf>,
    /// Optional embedding API configuration (OpenAI-compatible).
    pub embedding_api: Option<EmbeddingApiConfig>,
}

/// Configuration for an OpenAI-compatible embedding API.
/// Supports DeepSeek-aligned providers and standard OpenAI endpoints.
#[derive(Debug, Clone)]
pub struct EmbeddingApiConfig {
    /// Base URL (e.g. https://api.openai.com/v1).
    pub base_url: String,
    /// Embedding model name (e.g. text-embedding-3-small).
    pub model: String,
    /// Environment variable name for the API key.
    pub api_key_env: String,
    /// Vector dimension for this model.
    pub vector_dim: usize,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            vector_dim: 256,
            max_records: 10000,
            similarity_threshold: 0.6,
            persist: false,
            persist_path: None,
            embedding_api: None,
        }
    }
}

// ---------------------------------------------------------------------------
// VectorStore trait
// ---------------------------------------------------------------------------

pub trait VectorStore: Send + Sync {
    /// Insert a new memory record.
    fn insert(&mut self, record: MemoryRecord) -> anyhow::Result<()>;

    /// Search for similar records by embedding.
    fn search(&self, query_embedding: &[f32], top_k: usize) -> Vec<SearchResult>;

    /// Search by text content (computes embedding internally).
    fn search_by_text(&self, text: &str, top_k: usize) -> Vec<SearchResult>;

    /// Get a record by ID.
    fn get(&self, id: &str) -> Option<&MemoryRecord>;

    /// Remove a record by ID.
    fn remove(&mut self, id: &str) -> bool;

    /// Number of records.
    fn len(&self) -> usize;

    /// Compact memory — remove low-importance records.
    fn compact(&mut self, threshold: f32);

    /// Persist to disk.
    fn persist(&self) -> anyhow::Result<()>;
}

// ---------------------------------------------------------------------------
// InMemoryVectorStore — brute-force cosine similarity
// ---------------------------------------------------------------------------

pub struct InMemoryVectorStore {
    records: Vec<MemoryRecord>,
    config: MemoryConfig,
    /// Simple text → embedding cache (used for both API and hash modes).
    embed_cache: HashMap<String, Vec<f32>>,
    /// HTTP client for embedding API calls.
    http_client: Option<reqwest::Client>,
    /// API key for embedding API.
    api_key: Option<String>,
}

impl InMemoryVectorStore {
    pub fn new(config: MemoryConfig) -> Self {
        let (http_client, api_key) = if let Some(ref api_cfg) = config.embedding_api {
            let key = std::env::var(&api_cfg.api_key_env).ok();
            let client = reqwest::Client::new();
            (Some(client), key)
        } else {
            (None, None)
        };

        let mut store = Self {
            records: Vec::new(),
            embed_cache: HashMap::new(),
            config: config.clone(),
            http_client,
            api_key,
        };

        // Load persisted data if available
        if config.persist {
            if let Some(ref path) = config.persist_path {
                if path.exists() {
                    if let Ok(data) = fs::read_to_string(path) {
                        if let Ok(records) = serde_json::from_str::<Vec<MemoryRecord>>(&data) {
                            store.records = records;
                            info!(count = store.records.len(), "loaded persisted memory");
                        }
                    }
                }
            }
        }

        store
    }

    /// Compute a "word-level hash embedding" that captures semantic similarity
    /// better than raw character n-grams. Splits on whitespace/punctuation,
    /// applies multiple hash projections per word, and uses idf-like weighting.
    /// Falls back when no embedding API is configured.
    fn text_to_embedding(&self, text: &str) -> Vec<f32> {
        if let Some(cached) = self.embed_cache.get(text) {
            return cached.clone();
        }

        let dim = self.config.vector_dim;
        let mut embedding = vec![0.0_f32; dim];

        let text_lower = text.to_lowercase();
        let words: Vec<&str> = text_lower
            .split(|c: char| c.is_whitespace() || c.is_ascii_punctuation())
            .filter(|w| !w.is_empty())
            .collect();

        if words.is_empty() {
            return embedding;
        }

        // Word frequency for idf-like weighting
        let mut word_counts: HashMap<&str, usize> = HashMap::new();
        for w in &words {
            *word_counts.entry(*w).or_insert(0) += 1;
        }
        let total = words.len() as f32;

        for (pos, word) in words.iter().enumerate() {
            let tf = *word_counts.get(word).unwrap_or(&1) as f32 / total;
            let pos_weight = 1.0 - (pos as f32 / words.len() as f32) * 0.3; // slight positional decay

            // Multiple hash projections per word for better dimension coverage
            let h1 = hash_word(word, 0);
            let h2 = hash_word(word, 1);
            let h3 = hash_word(word, 2);

            let idx1 = (h1 as usize) % dim;
            let idx2 = (h2 as usize) % dim;
            let idx3 = (h3 as usize) % dim;

            let weight = tf * pos_weight;
            embedding[idx1] += weight;
            embedding[idx2] += weight * 0.7;
            embedding[idx3] += weight * 0.5;

            // Bigram context: hash adjacent word pairs for phrase awareness
            if pos > 0 {
                let bigram = format!("{}_{}", words[pos - 1], word);
                let hb = hash_word(&bigram, 0);
                let idx = (hb as usize) % dim;
                embedding[idx] += weight * 0.4;
            }
        }

        // Normalize to unit vector
        let magnitude: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        if magnitude > 0.0 {
            for val in embedding.iter_mut() {
                *val /= magnitude;
            }
        }

        embedding
    }

    /// Compute embedding via the configured API (async).
    /// Returns the embedding vector, or empty vec on failure.
    pub async fn embed_via_api(&self, text: &str) -> Vec<f32> {
        if let Some(cached) = self.embed_cache.get(text) {
            return cached.clone();
        }

        let (Some(client), Some(api_key), Some(ref api_cfg)) =
            (&self.http_client, &self.api_key, &self.config.embedding_api)
        else {
            return self.text_to_embedding(text);
        };

        let url = format!("{}/embeddings", api_cfg.base_url.trim_end_matches('/'));
        let body = serde_json::json!({
            "model": api_cfg.model,
            "input": text,
        });

        match client
            .post(&url)
            .bearer_auth(api_key)
            .json(&body)
            .send()
            .await
        {
            Ok(resp) => {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    if let Some(emb) = json["data"][0]["embedding"].as_array() {
                        let vec: Vec<f32> = emb
                            .iter()
                            .filter_map(|v| v.as_f64())
                            .map(|v| v as f32)
                            .collect();
                        if vec.len() == api_cfg.vector_dim {
                            return vec;
                        }
                    }
                }
                // API failed — fall through to hash
                tracing::warn!("embedding API call failed, falling back to hash embedding");
            }
            Err(e) => {
                tracing::warn!("embedding API error: {e}, falling back to hash embedding");
            }
        }

        self.text_to_embedding(text)
    }
}

/// Hash a word with a seed for multiple projections.
fn hash_word(word: &str, seed: u64) -> u64 {
    let mut hash: u64 = 5381u64.wrapping_add(seed);
    for b in word.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(b as u64);
    }
    hash
}

/// Compute cosine similarity between two vectors.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

impl VectorStore for InMemoryVectorStore {
    fn insert(&mut self, mut record: MemoryRecord) -> anyhow::Result<()> {
        // Generate embedding if empty
        if record.embedding.is_empty() {
            record.embedding = self.text_to_embedding(&record.content);
        }

        if self.records.len() >= self.config.max_records {
            // Remove lowest-importance record
            if let Some(min_idx) = self
                .records
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| a.importance.partial_cmp(&b.importance).unwrap())
                .map(|(idx, _)| idx)
            {
                self.records.remove(min_idx);
            }
        }

        self.records.push(record);

        if self.config.persist {
            self.persist()?;
        }

        Ok(())
    }

    fn search(&self, query_embedding: &[f32], top_k: usize) -> Vec<SearchResult> {
        let mut results: Vec<SearchResult> = self
            .records
            .iter()
            .map(|record| {
                let score = cosine_similarity(query_embedding, &record.embedding);
                SearchResult {
                    record: record.clone(),
                    score,
                }
            })
            .filter(|r| r.score >= self.config.similarity_threshold)
            .collect();

        // Sort by score descending
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(top_k);
        results
    }

    fn search_by_text(&self, text: &str, top_k: usize) -> Vec<SearchResult> {
        let embedding = self.text_to_embedding(text);
        self.search(&embedding, top_k)
    }

    fn get(&self, id: &str) -> Option<&MemoryRecord> {
        self.records.iter().find(|r| r.id == id)
    }

    fn remove(&mut self, id: &str) -> bool {
        let len_before = self.records.len();
        self.records.retain(|r| r.id != id);
        self.records.len() < len_before
    }

    fn len(&self) -> usize {
        self.records.len()
    }

    fn compact(&mut self, threshold: f32) {
        let before = self.records.len();
        self.records.retain(|r| r.importance >= threshold);
        let after = self.records.len();
        info!(before, after, "memory compacted");
    }

    fn persist(&self) -> anyhow::Result<()> {
        if let Some(ref path) = self.config.persist_path {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let json = serde_json::to_string(&self.records)?;
            fs::write(path, json)?;
            info!(count = self.records.len(), path = %path.display(), "memory persisted");
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_record(id: &str, content: &str) -> MemoryRecord {
        MemoryRecord {
            id: id.to_string(),
            created_at: Utc::now().timestamp(),
            content: content.to_string(),
            reasoning: None,
            embedding: vec![],
            tags: vec![],
            source: "test".to_string(),
            importance: 0.8,
        }
    }

    #[test]
    fn test_insert_and_search() {
        let config = MemoryConfig {
            vector_dim: 64,
            similarity_threshold: 0.0,
            ..Default::default()
        };
        let mut store = InMemoryVectorStore::new(config);

        store.insert(make_record("1", "hello world")).unwrap();
        store.insert(make_record("2", "goodbye world")).unwrap();
        store.insert(make_record("3", "rust programming")).unwrap();

        assert_eq!(store.len(), 3);

        let results = store.search_by_text("hello", 5);
        assert!(!results.is_empty(), "should find similar texts");
        assert_eq!(
            results[0].record.id, "1",
            "most similar should be 'hello world'"
        );
    }

    #[test]
    fn test_cosine_similarity() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 1e-6);

        let c = vec![0.0, 1.0, 0.0];
        assert!((cosine_similarity(&a, &c) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn test_compact() {
        let config = MemoryConfig::default();
        let mut store = InMemoryVectorStore::new(config);

        let mut record = make_record("1", "important");
        record.importance = 0.9;
        store.insert(record).unwrap();

        let mut record = make_record("2", "unimportant");
        record.importance = 0.1;
        store.insert(record).unwrap();

        store.compact(0.5);
        assert_eq!(store.len(), 1);
        assert_eq!(store.get("1").is_some(), true);
        assert_eq!(store.get("2").is_some(), false);
    }
}

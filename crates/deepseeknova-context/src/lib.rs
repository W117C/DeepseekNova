//! # Context — Workspace indexing and working memory
//!
//! Builds and maintains the agent's contextual understanding of the
//! workspace: file trees, project memory (DEEPSEEKNOVA.md), and session state.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented,
        clippy::dbg_macro
    )
)]

pub mod history;

use chrono::{DateTime, Utc};
use deepseeknova_core::registry::Command;
use deepseeknova_core::types::{Message, Role, ToolSchema};
use deepseeknova_core::DeepseeknovaError;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// ContextProvider trait — Runtime depends on this, not a concrete engine
// ---------------------------------------------------------------------------

/// Provides access to the runtime's contextual state: workspace index,
/// working memory, and project memory. The runtime depends on this trait
/// rather than on a concrete engine.
#[async_trait::async_trait]
pub trait ContextProvider: Send + Sync {
    /// Returns the scanned workspace index.
    fn workspace(&self) -> &WorkspaceIndex;
    /// Returns the session-scoped working memory.
    fn working_memory(&self) -> &WorkingMemory;
    /// Returns the project-scoped memory loaded from the workspace.
    fn project_memory(&self) -> &ProjectMemory;
}

// ---------------------------------------------------------------------------
// WorkspaceIndex — scan real filesystem
// ---------------------------------------------------------------------------

/// The scanned state of the workspace filesystem: the root path and the
/// resulting file tree, produced by [`WorkspaceIndex::scan`].
pub struct WorkspaceIndex {
    /// Workspace root directory.
    pub root: PathBuf,
    /// Tree of files and directories discovered under the root.
    pub file_tree: FileTree,
}

impl WorkspaceIndex {
    /// Scan a directory and return a file tree. Respects .gitignore.
    pub fn scan(root: &Path) -> Result<Self, DeepseeknovaError> {
        let mut entries = Vec::new();
        let mut gitignore_patterns = Vec::new();

        // Load .gitignore if present
        let gi_path = root.join(".gitignore");
        if gi_path.exists() {
            let content = std::fs::read_to_string(&gi_path)?;
            for line in content.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() && !trimmed.starts_with('#') {
                    gitignore_patterns.push(trimmed.to_string());
                }
            }
        }

        scan_dir(root, root, &mut entries, &gitignore_patterns)?;

        Ok(Self {
            root: root.to_path_buf(),
            file_tree: FileTree { entries },
        })
    }

    /// Reload the workspace index.
    pub fn refresh(&mut self) -> Result<(), DeepseeknovaError> {
        *self = Self::scan(&self.root)?;
        Ok(())
    }
}

/// Recursively scan a directory, respecting gitignore patterns.
fn scan_dir(
    base: &Path,
    dir: &Path,
    entries: &mut Vec<FileEntry>,
    ignores: &[String],
) -> Result<(), DeepseeknovaError> {
    // Skip hidden directories except .git and .deepseeknova
    if let Some(name) = dir.file_name().and_then(|n| n.to_str()) {
        if name.starts_with('.') && name != "." && name != ".deepseeknova" {
            return Ok(());
        }
    }

    // Check gitignore
    let rel = dir.strip_prefix(base).unwrap_or(dir);
    let rel_str = rel.to_string_lossy();
    for pat in ignores {
        if simple_glob_match(pat, &rel_str) {
            return Ok(());
        }
    }

    let read_dir = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return Ok(()), // skip unreadable dirs
    };

    for entry in read_dir {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let path = entry.path();
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };

        let rel_path = path.strip_prefix(base).unwrap_or(&path).to_path_buf();

        let size = if ft.is_file() {
            match entry.metadata() {
                Ok(m) => m.len(),
                Err(_) => 0,
            }
        } else {
            0
        };

        entries.push(FileEntry {
            path: rel_path.clone(),
            is_dir: ft.is_dir(),
            size,
        });

        if ft.is_dir() {
            scan_dir(base, &path, entries, ignores)?;
        }
    }

    Ok(())
}

/// Simple glob matching for gitignore patterns.
fn simple_glob_match(pattern: &str, name: &str) -> bool {
    // Very basic: if pattern ends with / it's a dir pattern
    let pattern = pattern.trim_end_matches('/');
    // If pattern starts with /, it's anchored to root
    let pattern = pattern.trim_start_matches('/');

    if pattern == name {
        return true;
    }
    // Suffix match: *.ext
    if let Some(ext) = pattern.strip_prefix("*.") {
        return name.ends_with(ext);
    }
    // Prefix match: dir/*
    if let Some(prefix) = pattern.strip_suffix("/*") {
        return name.starts_with(prefix);
    }
    // Contains match: *word*
    if pattern.starts_with('*') && pattern.ends_with('*') && pattern.len() > 1 {
        let inner = &pattern[1..pattern.len() - 1];
        return name.contains(inner);
    }

    false
}

#[derive(Debug, Clone)]
/// The set of files and directories discovered by a workspace scan.
pub struct FileTree {
    /// Flat list of entries (files and directories) under the scanned root.
    pub entries: Vec<FileEntry>,
}

#[derive(Debug, Clone)]
/// A single file or directory found during a workspace scan.
pub struct FileEntry {
    /// Path relative to the scan root.
    pub path: PathBuf,
    /// Whether this entry is a directory.
    pub is_dir: bool,
    /// File size in bytes (0 for directories).
    pub size: u64,
}

// ---------------------------------------------------------------------------
// PromptBuilder — injects tools into messages
// ---------------------------------------------------------------------------

/// Builds a message sequence for the provider by injecting tool schemas,
/// project context, and the repo map into the system prompt.
pub struct PromptBuilder;

impl PromptBuilder {
    /// Build messages for the provider. Injects tool schemas into the system prompt.
    pub fn build(
        system_prompt: &str,
        tools: &[ToolSchema],
        working_memory: &WorkingMemory,
        project_memory: &ProjectMemory,
        repo_map: Option<&str>,
    ) -> Vec<Message> {
        let mut messages = Vec::new();

        // Build system prompt with tools injected
        let mut system_content = String::with_capacity(system_prompt.len() + 2048);
        system_content.push_str(system_prompt);

        // Inject tool descriptions (P0.7：段序与 CacheAwarePromptBuilder 对齐
        // —— 静态 system → 工具 → 项目记忆 → repo map，保证前缀缓存稳定性
        // 语义在两条构建路径上一致；此前 tools 段被放在 repo map 之后，
        // 与缓存感知路径漂移)。
        if !tools.is_empty() {
            system_content.push_str("\n\n## Available Tools\n\n");
            for tool in tools {
                system_content.push_str(&format!("- **{}**: {}\n", tool.name, tool.description));
            }
        }

        // Inject project memory
        if let Some(ref deepseeknova_md) = project_memory.deepseeknova_md {
            system_content.push_str("\n\n---\n## Project Context\n\n");
            system_content.push_str(deepseeknova_md);
        }

        // Inject repo map (stable prefix region: after project context).
        if let Some(map) = repo_map {
            if !map.is_empty() {
                system_content.push_str("\n\n---\n## Repo Map\n\n```\n");
                system_content.push_str(map);
                system_content.push_str("\n```\n");
            }
        }

        messages.push(Message {
            role: Role::System,
            content: system_content,
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            reasoning_signature: None,
        });

        // Conversation history from working memory
        messages.extend(working_memory.conversation.iter().cloned());

        // Compaction digest injected after system prompt
        if let Some(ref digest) = working_memory.compaction_digest {
            if messages.len() > 1 {
                messages.insert(
                    1,
                    Message {
                        role: Role::User,
                        content: format!(
                            "<conversation-summary>\n{digest}\n</conversation-summary>"
                        ),
                        name: None,
                        tool_calls: None,
                        tool_call_id: None,
                        reasoning_content: None,
                        reasoning_signature: None,
                    },
                );
            }
        }

        messages
    }
}

// ---------------------------------------------------------------------------
// CacheAwarePromptBuilder — DeepSeek V4 prefix cache optimization
// ---------------------------------------------------------------------------
///
/// DeepSeek V4 uses disk-level automatic prefix caching: identical byte-level
/// prefixes across requests hit the cache, reducing input token cost by ~90%.
/// This builder enforces the "stable prefix + volatile suffix" structure.
///
/// agent 主路径通过 [`CacheAwarePromptBuilder::build_prefix`] 构造 system
/// 消息前缀（system prompt + repo_map），获得 prefix hash 用于 cache miss
/// 诊断；完整 `build` 供嵌入方构造含工具描述与项目记忆的完整 messages。
///
/// ```text
/// [System Prompt — byte-level fixed]
/// [Tool Schemas — fixed order, no per-request changes]
/// [Project Memory — relatively stable]
/// ─────────── CACHE PREFIX BOUNDARY ───────────
/// [Conversation History]
/// [Current User Input / Tool Results — most volatile]
/// ```
pub struct CacheAwarePromptBuilder {
    /// SHA256 hash of the last stable prefix built.
    last_prefix_hash: Option<String>,
    /// Whether to emit tracing warnings on cache miss.
    warn_on_cache_miss: bool,
}

impl CacheAwarePromptBuilder {
    /// Create a new builder. When `warn_on_cache_miss` is true, a tracing
    /// warning is emitted whenever the computed prefix hash changes between
    /// calls (the next request will miss the prefix cache).
    pub fn new(warn_on_cache_miss: bool) -> Self {
        Self {
            last_prefix_hash: None,
            warn_on_cache_miss,
        }
    }

    /// 只构造稳定前缀（system prompt + tools + project_memory + repo_map），
    /// 返回 `(prefix_content, prefix_hash)`。供 agent 主路径等只需要前缀部分、
    /// 不需要 builder 构造完整 messages 的调用方使用。
    ///
    /// `tools` 传空切片则跳过工具描述注入（agent 主路径由 provider 层负责工具
    /// schema 注入，此处传 `&[]`）。`project_memory` 传 `None` 则跳过项目记忆
    /// 注入。
    pub fn build_prefix(
        &mut self,
        system_prompt: &str,
        tools: &[ToolSchema],
        project_memory: Option<&ProjectMemory>,
        repo_map: Option<&str>,
    ) -> (String, String) {
        use sha2::{Digest, Sha256};

        // ── STABLE PREFIX ──────────────────────────────────
        let mut prefix_parts = Vec::new();

        // 1. System prompt (most stable)
        prefix_parts.push(system_prompt.to_string());

        // 2. Tool schemas in fixed alphabetical order
        let mut sorted_tools: Vec<&ToolSchema> = tools.iter().collect();
        sorted_tools.sort_by(|a, b| a.name.cmp(&b.name));
        let tools_text: String = sorted_tools
            .iter()
            .map(|t| format!("- {}: {}", t.name, t.description))
            .collect::<Vec<_>>()
            .join("\n");
        if !tools_text.is_empty() {
            prefix_parts.push(format!("## Available Tools\n\n{tools_text}"));
        }

        // 3. Global rules (AGENTS.md — 行业标准项目规则，most stable)。
        //    B6 三层缓存分层：global → project。AGENTS.md 变化只 invalidate
        //    本段（置于 project 之前，静态优先动态最后）。
        if let Some(pm) = project_memory {
            if let Some(ref agents) = pm.agents_md {
                if !agents.is_empty() {
                    prefix_parts.push(format!("## Project Rules\n\n{agents}"));
                }
            }
        }

        // 4. Project memory (DEEPSEEKNOVA.md — stable between config changes)
        if let Some(pm) = project_memory {
            if let Some(ref deepseeknova_md) = pm.deepseeknova_md {
                prefix_parts.push(format!("## Project Context\n\n{deepseeknova_md}"));
            }
        }

        // 5. Repo map (global code graph — stable within an index generation).
        //    Part of the cache prefix so it does not invalidate the volatile suffix.
        if let Some(map) = repo_map {
            if !map.is_empty() {
                prefix_parts.push(format!("## Repo Map\n\n```\n{map}\n```"));
            }
        }

        let prefix_content = prefix_parts.join("\n\n---\n\n");

        // Compute prefix hash for cache diagnostics
        let mut hasher = Sha256::new();
        hasher.update(prefix_content.as_bytes());
        let prefix_hash = hex::encode(hasher.finalize());

        // Detect cache-invalidating prefix changes
        if self.warn_on_cache_miss {
            if let Some(ref last) = self.last_prefix_hash {
                if last != &prefix_hash {
                    tracing::warn!(
                        previous = %last,
                        current = %prefix_hash,
                        "cache prefix changed — next request will be a cache miss"
                    );
                }
            }
        }
        self.last_prefix_hash = Some(prefix_hash.clone());

        (prefix_content, prefix_hash)
    }

    /// Build messages optimized for DeepSeek V4 prefix caching.
    ///
    /// Returns (messages, prefix_hash) where prefix_hash identifies the
    /// stable portion of the prompt. Callers can compare across requests
    /// to detect cache-invalidating changes.
    pub fn build(
        &mut self,
        system_prompt: &str,
        tools: &[ToolSchema],
        project_memory: &ProjectMemory,
        repo_map: Option<&str>,   // stable: global code graph repo map
        conversation: &[Message], // volatile: conversation history
        user_input: &str,         // volatile: current user message
    ) -> (Vec<Message>, String) {
        let (prefix_content, prefix_hash) =
            self.build_prefix(system_prompt, tools, Some(project_memory), repo_map);

        let mut messages = Vec::new();

        // Push the stable prefix as system message
        messages.push(Message {
            role: Role::System,
            content: prefix_content,
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            reasoning_signature: None,
        });

        // ── VOLATILE SUFFIX ────────────────────────────────
        // 4. Conversation history
        messages.extend(conversation.iter().cloned());

        // 5. Current user input (most volatile — always at the end)
        if !user_input.is_empty() {
            messages.push(Message {
                role: Role::User,
                content: user_input.to_string(),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                reasoning_signature: None,
            });
        }

        (messages, prefix_hash)
    }

    /// Returns the hash of the last stable prefix, if any.
    pub fn last_prefix_hash(&self) -> Option<&str> {
        self.last_prefix_hash.as_deref()
    }
}

// ---------------------------------------------------------------------------
// SectionStability — type-level ordering for cache prefix integrity
// ---------------------------------------------------------------------------

/// Each prompt section's position on the stability spectrum.
/// The builder enforces non-decreasing stability order — once you've
/// added volatile content, you can't go back and insert static content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SectionStability {
    /// Byte-level identical for the entire session: system prompt.
    Static = 0,
    /// Identical for a given tool set (alphabetical order, no dynamic changes).
    SemiStatic = 1,
    /// Only grows, never shrinks or mutates: conversation history.
    AppendOnly = 2,
    /// Changes every request: current time, latest tool results.
    Volatile = 3,
}

/// A section of the prompt with its stability classification.
pub struct PromptSection {
    /// The stability classification of this section.
    pub stability: SectionStability,
    /// The raw bytes of this section's content.
    pub bytes: Vec<u8>,
}

/// Error when inserting a section would break cache prefix structure.
#[derive(Debug, thiserror::Error)]
#[error("inserting {attempted:?} section after {last:?} — would break cache prefix ordering")]
pub struct BuilderOrderError {
    /// The stability level that was attempted to be inserted.
    pub attempted: SectionStability,
    /// The stability level of the most recently inserted section.
    pub last: SectionStability,
}

/// 把 [`BuilderOrderError`] 转换为 [`deepseeknova_core::DeepseeknovaError`]。
///
/// orphan rule：impl 放在拥有 `BuilderOrderError` 的本 crate。`?` 可直接把
/// `Result<_, BuilderOrderError>` 用于返回 `Result<_, DeepseeknovaError>` 的函数。
impl From<BuilderOrderError> for deepseeknova_core::DeepseeknovaError {
    fn from(err: BuilderOrderError) -> Self {
        deepseeknova_core::DeepseeknovaError::Context(Box::new(err))
    }
}

/// **库级公开 API**：供嵌入方自行接线到更严格的前缀稳定性排序场景。
///
/// Enhanced prompt builder that enforces stability ordering at the type level.
pub struct OrderedPromptBuilder {
    sections: Vec<PromptSection>,
}

impl OrderedPromptBuilder {
    /// Create an empty builder with no sections.
    pub fn new() -> Self {
        Self {
            sections: Vec::new(),
        }
    }

    /// Add a section, enforcing that stability is non-decreasing.
    /// This prevents the anti-pattern of inserting static content after
    /// volatile content, which would break DeepSeek V4 prefix caching.
    pub fn push_section(
        &mut self,
        stability: SectionStability,
        bytes: Vec<u8>,
    ) -> Result<(), BuilderOrderError> {
        if let Some(last) = self.sections.last() {
            if stability < last.stability {
                return Err(BuilderOrderError {
                    attempted: stability,
                    last: last.stability,
                });
            }
        }
        self.sections.push(PromptSection { stability, bytes });
        Ok(())
    }

    /// Build the final prompt.
    ///
    /// Returns both the full byte stream and the cache prefix
    /// (everything up to but not including the first Volatile section).
    pub fn build(&self) -> BuiltPrompt {
        let cache_prefix_end: usize = self
            .sections
            .iter()
            .take_while(|s| s.stability != SectionStability::Volatile)
            .map(|s| s.bytes.len())
            .sum();

        let full: Vec<u8> = self.sections.iter().flat_map(|s| s.bytes.clone()).collect();

        let cache_prefix = if cache_prefix_end <= full.len() {
            full[..cache_prefix_end].to_vec()
        } else {
            full.clone()
        };

        BuiltPrompt {
            cache_prefix,
            full_bytes: full,
        }
    }
}

impl Default for OrderedPromptBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// The output of building a cache-aware prompt.
pub struct BuiltPrompt {
    /// Bytes that form the cacheable prefix (Static + SemiStatic + AppendOnly).
    pub cache_prefix: Vec<u8>,
    /// Complete prompt bytes for the request.
    pub full_bytes: Vec<u8>,
}

// ---------------------------------------------------------------------------
// PromptCacheStabilityTracker — detect cache-invalidating changes
// ---------------------------------------------------------------------------

/// Tracks whether the cacheable prefix has changed between requests.
///
/// Use this to correlate predicted cache behavior with actual
/// `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens` from the API.
pub struct PromptCacheStabilityTracker {
    last_prefix_hash: Option<u64>,
    last_prefix_len: usize,
}

/// Result of checking prefix stability.
pub enum CacheStabilityReport {
    /// First call — no previous prefix to compare.
    FirstCall,
    /// Prefix unchanged since last call — cache hit expected.
    Stable,
    /// Prefix changed — next request will be a cache miss.
    Changed {
        /// Length of the previously checked prefix.
        previous_len: usize,
        /// Length of the current prefix.
        current_len: usize,
    },
}

impl PromptCacheStabilityTracker {
    /// Create a tracker with no previous prefix recorded.
    pub fn new() -> Self {
        Self {
            last_prefix_hash: None,
            last_prefix_len: 0,
        }
    }

    /// Check the given prefix against the last known prefix.
    pub fn check(&mut self, prefix: &[u8]) -> CacheStabilityReport {
        let hash = hash_prefix_bytes(prefix);
        let report = match self.last_prefix_hash {
            None => CacheStabilityReport::FirstCall,
            Some(prev) if prev == hash => CacheStabilityReport::Stable,
            Some(_) => CacheStabilityReport::Changed {
                previous_len: self.last_prefix_len,
                current_len: prefix.len(),
            },
        };
        self.last_prefix_hash = Some(hash);
        self.last_prefix_len = prefix.len();
        report
    }

    /// Length of the most recently checked prefix.
    pub fn last_prefix_len(&self) -> usize {
        self.last_prefix_len
    }
}

impl Default for PromptCacheStabilityTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Fast, non-cryptographic hash for prefix comparison.
fn hash_prefix_bytes(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 5381;
    for &b in bytes {
        hash = hash.wrapping_mul(33).wrapping_add(b as u64);
    }
    hash
}

// =========================================================================
// Cache stability tests
// =========================================================================

#[cfg(test)]
mod cache_tests {
    use super::*;

    #[test]
    fn identical_inputs_produce_identical_prefix() {
        let mut b1 = OrderedPromptBuilder::new();
        b1.push_section(
            SectionStability::Static,
            b"system: you are a coder".to_vec(),
        )
        .unwrap();
        b1.push_section(SectionStability::Volatile, b"user: hello".to_vec())
            .unwrap();
        let p1 = b1.build();

        let mut b2 = OrderedPromptBuilder::new();
        b2.push_section(
            SectionStability::Static,
            b"system: you are a coder".to_vec(),
        )
        .unwrap();
        b2.push_section(SectionStability::Volatile, b"user: hello".to_vec())
            .unwrap();
        let p2 = b2.build();

        assert_eq!(p1.cache_prefix, p2.cache_prefix);
    }

    #[test]
    fn inserting_static_after_volatile_is_rejected() {
        let mut builder = OrderedPromptBuilder::new();
        builder
            .push_section(SectionStability::Volatile, b"user: hi".to_vec())
            .unwrap();
        let result = builder.push_section(SectionStability::Static, b"system: late".to_vec());
        assert!(result.is_err());
    }

    #[test]
    fn history_growth_keeps_old_prefix_as_strict_prefix() {
        let mut b1 = OrderedPromptBuilder::new();
        b1.push_section(SectionStability::Static, b"sys".to_vec())
            .unwrap();
        b1.push_section(SectionStability::AppendOnly, b"turn1".to_vec())
            .unwrap();
        b1.push_section(SectionStability::Volatile, b"now".to_vec())
            .unwrap();
        let p1 = b1.build();

        let mut b2 = OrderedPromptBuilder::new();
        b2.push_section(SectionStability::Static, b"sys".to_vec())
            .unwrap();
        b2.push_section(SectionStability::AppendOnly, b"turn1".to_vec())
            .unwrap();
        b2.push_section(SectionStability::AppendOnly, b"turn2".to_vec())
            .unwrap();
        b2.push_section(SectionStability::Volatile, b"now".to_vec())
            .unwrap();
        let p2 = b2.build();

        // Old prefix must be a strict prefix of new prefix (for cache reuse)
        assert!(p2.cache_prefix.starts_with(&p1.cache_prefix));
        assert!(p2.cache_prefix.len() > p1.cache_prefix.len());
    }

    #[test]
    fn tracker_reports_stable_on_identical_prefix() {
        let mut tracker = PromptCacheStabilityTracker::new();
        let prefix = b"static prefix content";

        let r1 = tracker.check(prefix);
        assert!(matches!(r1, CacheStabilityReport::FirstCall));

        let r2 = tracker.check(prefix);
        assert!(matches!(r2, CacheStabilityReport::Stable));
    }

    #[test]
    fn tracker_reports_changed_on_different_prefix() {
        let mut tracker = PromptCacheStabilityTracker::new();
        tracker.check(b"prefix v1");
        let report = tracker.check(b"prefix v2 -- changed");
        assert!(matches!(report, CacheStabilityReport::Changed { .. }));
    }

    #[test]
    fn tool_schema_serialization_is_order_deterministic() {
        // Verify that tool schemas sorted by name produce identical bytes
        use deepseeknova_core::types::ToolSchema;
        let tools = vec![
            ToolSchema {
                name: "zebra".into(),
                description: "last".into(),
                parameters: serde_json::json!({"type": "object"}),
            },
            ToolSchema {
                name: "alpha".into(),
                description: "first".into(),
                parameters: serde_json::json!({"type": "object"}),
            },
        ];

        let serialize = |t: &[ToolSchema]| -> Vec<u8> {
            let mut sorted: Vec<&ToolSchema> = t.iter().collect();
            sorted.sort_by(|a, b| a.name.cmp(&b.name));
            sorted
                .iter()
                .map(|t| format!("{}:{}", t.name, t.description))
                .collect::<Vec<_>>()
                .join("\n")
                .into_bytes()
        };

        let b1 = serialize(&tools);
        let b2 = serialize(&tools);
        assert_eq!(b1, b2, "tool schema serialization must be deterministic");
        // Verify order: alpha before zebra
        let text = String::from_utf8(b1).unwrap();
        let alpha_pos = text.find("alpha").unwrap();
        let zebra_pos = text.find("zebra").unwrap();
        assert!(alpha_pos < zebra_pos, "alpha must come before zebra");
    }
}

// ---------------------------------------------------------------------------
// Memory — three tiers
// ---------------------------------------------------------------------------

/// The session's in-memory conversation state: message history, the optional
/// compaction digest, and pinned messages that survive clearing.
pub struct WorkingMemory {
    /// Conversation messages in chronological order.
    pub conversation: VecDeque<Message>,
    /// Optional digest of a previously compacted prefix of the conversation.
    pub compaction_digest: Option<String>,
    /// Messages pinned to survive `clear` (system prompt, first turn, …).
    pub pinned: Vec<Message>,
}

impl WorkingMemory {
    /// Create an empty working memory.
    pub fn new() -> Self {
        Self {
            conversation: VecDeque::new(),
            compaction_digest: None,
            pinned: Vec::new(),
        }
    }

    /// Append a message to the conversation history.
    pub fn add_message(&mut self, message: Message) {
        self.conversation.push_back(message);
    }

    /// Clone the full conversation history as a `Vec<Message>`.
    pub fn get_all(&self) -> Vec<Message> {
        self.conversation.iter().cloned().collect()
    }

    /// Clear the conversation history and the compaction digest. Pinned
    /// messages are kept.
    pub fn clear(&mut self) {
        self.conversation.clear();
        self.compaction_digest = None;
    }

    /// Remove the last `count` messages from the conversation.
    pub fn rewind(&mut self, count: usize) {
        for _ in 0..count {
            self.conversation.pop_back();
        }
    }

    /// Pin a message (survives compaction; useful for system prompt, first turn).
    pub fn pin(&mut self, message: Message) {
        self.pinned.push(message);
    }
}

impl Default for WorkingMemory {
    fn default() -> Self {
        Self::new()
    }
}

/// Project-level memory loaded from the workspace: persistent memory entries,
/// the optional DEEPSEEKNOVA.md content, and custom slash commands.
pub struct ProjectMemory {
    /// Persistent memory entries keyed by name, loaded from `.deepseeknova/memory`.
    pub auto_memory: HashMap<String, MemoryEntry>,
    /// Content of DEEPSEEKNOVA.md at the workspace root, if present.
    pub deepseeknova_md: Option<String>,
    /// B6：Content of AGENTS.md at the workspace root, if present — treated as
    /// the **global/stable** rule layer (industry-standard file read by
    /// Claude Code / Codex / opencode), injected before project memory so a
    /// rule change invalidates only the global segment of the prefix cache.
    pub agents_md: Option<String>,
    /// Custom slash commands loaded from `.deepseeknova/commands`.
    pub custom_commands: Vec<Command>,
}

impl ProjectMemory {
    /// Create an empty project memory. Populate it with the `load_*` methods.
    pub fn new() -> Self {
        Self {
            auto_memory: HashMap::new(),
            deepseeknova_md: None,
            agents_md: None,
            custom_commands: Vec::new(),
        }
    }

    /// Load DEEPSEEKNOVA.md from the workspace root if present.
    pub fn load_deepseeknova_md(&mut self, root: &Path) {
        let path = root.join("DEEPSEEKNOVA.md");
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                self.deepseeknova_md = Some(content);
            }
        }
    }

    /// B6：Load AGENTS.md from the workspace root if present (global rule layer).
    /// AGENTS.md 是行业标准文件名（Claude Code / Codex / opencode / DeepseekNova
    /// 均自动识别），作为稳定前缀的 global 段注入，先于 project 段。
    pub fn load_agents_md(&mut self, root: &Path) {
        let path = root.join("AGENTS.md");
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                self.agents_md = Some(content);
            }
        }
    }

    /// Load all persistent memory entries from .deepseeknova/memory/*.md files.
    pub fn load_memory_files(&mut self, root: &Path) {
        let memory_dir = root.join(".deepseeknova").join("memory");
        if !memory_dir.is_dir() {
            return;
        }
        if let Ok(entries) = std::fs::read_dir(&memory_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_none_or(|e| e != "md") {
                    continue;
                }
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Some(mem) = parse_memory_md(&content) {
                        let name = path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("unknown")
                            .to_string();
                        self.auto_memory.insert(name, mem);
                    }
                }
            }
        }
    }

    /// Load custom slash commands from .deepseeknova/commands/*.md files.
    pub fn load_custom_commands(&mut self, root: &Path) {
        let commands_dir = root.join(".deepseeknova").join("commands");
        if !commands_dir.is_dir() {
            return;
        }
        if let Ok(entries) = std::fs::read_dir(&commands_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_none_or(|e| e != "md") {
                    continue;
                }
                if let Ok(content) = std::fs::read_to_string(&path) {
                    let name = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("unknown")
                        .to_string();

                    let (description, _body) = split_frontmatter(&content);

                    self.custom_commands.push(Command {
                        name,
                        description: description.unwrap_or_default(),
                        builtin: false,
                    });
                }
            }
        }
    }
}

/// Parse a memory markdown file with optional frontmatter.
fn parse_memory_md(content: &str) -> Option<MemoryEntry> {
    let (frontmatter, _body) = split_raw_frontmatter(content);
    let fm = frontmatter?;

    let name = fm
        .lines()
        .find_map(|l| l.strip_prefix("name:").map(|v| v.trim().to_string()))
        .unwrap_or_default();

    let description = fm
        .lines()
        .find_map(|l| l.strip_prefix("description:").map(|v| v.trim().to_string()))
        .unwrap_or_default();

    Some(MemoryEntry {
        name,
        description,
        content: content.to_string(),
        metadata: MemoryMetadata {
            memory_type: MemoryType::Project,
            created: chrono::Utc::now(),
            updated: chrono::Utc::now(),
        },
    })
}

/// Split YAML frontmatter from markdown content.
/// Returns (frontmatter_lines, body).
fn split_raw_frontmatter(content: &str) -> (Option<String>, String) {
    if let Some(rest) = content.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---") {
            let fm = rest[..end].to_string();
            let body = rest[end + 4..].trim().to_string();
            return (Some(fm), body);
        }
    }
    (None, content.to_string())
}

/// Split frontmatter returning (description, body).
fn split_frontmatter(content: &str) -> (Option<String>, String) {
    let (fm, body) = split_raw_frontmatter(content);
    let desc = fm.and_then(|f| {
        f.lines().find_map(|l| {
            l.strip_prefix("description:")
                .map(|v| v.trim().trim_matches('"').to_string())
        })
    });
    (desc, body)
}

impl Default for ProjectMemory {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
/// A single persistent memory entry with optional frontmatter metadata.
pub struct MemoryEntry {
    /// Entry name (file stem when loaded from disk).
    pub name: String,
    /// Human-readable description from frontmatter.
    pub description: String,
    /// Full markdown body of the entry.
    pub content: String,
    /// Frontmatter metadata (type, created, updated).
    pub metadata: MemoryMetadata,
}

#[derive(Debug, Clone)]
/// Frontmatter metadata attached to a [`MemoryEntry`].
pub struct MemoryMetadata {
    /// The kind of memory this entry represents.
    pub memory_type: MemoryType,
    /// When the entry was created.
    pub created: DateTime<Utc>,
    /// When the entry was last updated.
    pub updated: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// The kind of a persistent memory entry.
pub enum MemoryType {
    /// Memory explicitly provided by the user.
    User,
    /// Memory derived from user feedback.
    Feedback,
    /// Memory associated with the current project.
    Project,
    /// Reference material that is not project-specific.
    Reference,
}

// ---------------------------------------------------------------------------
// ContextEngine — concrete implementation
// ---------------------------------------------------------------------------

/// Concrete [`ContextProvider`]: owns the workspace index, prompt builder,
/// working memory, and project memory for one session.
pub struct ContextEngine {
    /// The scanned workspace index.
    pub workspace: WorkspaceIndex,
    /// Builder used to assemble provider messages.
    pub prompt_builder: PromptBuilder,
    /// Session-scoped conversation memory.
    pub working_memory: WorkingMemory,
    /// Project-scoped memory loaded from the workspace.
    pub project_memory: ProjectMemory,
}

impl ContextEngine {
    /// Create a context engine for the given workspace root: scans the
    /// workspace and loads project memory (DEEPSEEKNOVA.md, memory files,
    /// custom commands).
    pub fn new(root: PathBuf) -> Result<Self, DeepseeknovaError> {
        let workspace = WorkspaceIndex::scan(&root)?;
        let mut project_memory = ProjectMemory::new();
        project_memory.load_deepseeknova_md(&root);
        project_memory.load_memory_files(&root);
        project_memory.load_custom_commands(&root);

        Ok(Self {
            workspace,
            prompt_builder: PromptBuilder,
            working_memory: WorkingMemory::new(),
            project_memory,
        })
    }
}

impl ContextProvider for ContextEngine {
    fn workspace(&self) -> &WorkspaceIndex {
        &self.workspace
    }

    fn working_memory(&self) -> &WorkingMemory {
        &self.working_memory
    }

    fn project_memory(&self) -> &ProjectMemory {
        &self.project_memory
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- simple_glob_match ---

    #[test]
    fn glob_exact_match() {
        assert!(simple_glob_match("target", "target"));
    }

    #[test]
    fn glob_suffix_ext() {
        assert!(simple_glob_match("*.rs", "main.rs"));
        assert!(!simple_glob_match("*.rs", "main.txt"));
    }

    #[test]
    fn glob_prefix_dir() {
        assert!(simple_glob_match("target/*", "target/debug/build"));
    }

    #[test]
    fn glob_contains() {
        assert!(simple_glob_match("*node_modules*", "path/node_modules/pkg"));
    }

    #[test]
    fn glob_strips_leading_slash() {
        // Patterns like "/target" should match "target"
        assert!(simple_glob_match("/target", "target"));
    }

    #[test]
    fn glob_trailing_slash_dir() {
        // Patterns ending with / should match directory name
        assert!(simple_glob_match("target/", "target"));
    }

    // --- split_raw_frontmatter ---

    #[test]
    fn split_frontmatter_basic() {
        let content = "---\nname: test\ndescription: desc\n---\nbody text";
        let (fm, body) = split_raw_frontmatter(content);
        assert!(fm.is_some());
        assert!(fm.unwrap().contains("name: test"));
        assert_eq!(body, "body text");
    }

    #[test]
    fn split_frontmatter_no_frontmatter() {
        let content = "just body text";
        let (fm, body) = split_raw_frontmatter(content);
        assert!(fm.is_none());
        assert_eq!(body, "just body text");
    }

    // --- WorkingMemory ---

    #[test]
    fn working_memory_add_and_get() {
        let mut wm = WorkingMemory::new();
        wm.add_message(Message {
            role: Role::User,
            content: "hello".into(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            reasoning_signature: None,
        });
        wm.add_message(Message {
            role: Role::Assistant,
            content: "hi".into(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            reasoning_signature: None,
        });
        assert_eq!(wm.get_all().len(), 2);
    }

    #[test]
    fn working_memory_clear() {
        let mut wm = WorkingMemory::new();
        wm.add_message(Message {
            role: Role::User,
            content: "test".into(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            reasoning_signature: None,
        });
        assert_eq!(wm.get_all().len(), 1);
        wm.clear();
        assert!(wm.get_all().is_empty());
        assert!(wm.compaction_digest.is_none());
    }

    #[test]
    fn working_memory_rewind() {
        let mut wm = WorkingMemory::new();
        for i in 0..5 {
            wm.add_message(Message {
                role: Role::User,
                content: format!("msg{i}"),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                reasoning_signature: None,
            });
        }
        assert_eq!(wm.get_all().len(), 5);
        wm.rewind(2);
        assert_eq!(wm.get_all().len(), 3);
    }

    #[test]
    fn working_memory_pin_survives_clear() {
        let mut wm = WorkingMemory::new();
        wm.pin(Message {
            role: Role::System,
            content: "system prompt".into(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            reasoning_signature: None,
        });
        assert_eq!(wm.pinned.len(), 1);
        wm.clear();
        assert_eq!(wm.pinned.len(), 1);
        assert_eq!(wm.pinned[0].content, "system prompt");
    }

    // --- PromptBuilder ---

    #[test]
    fn prompt_builder_injects_tools() {
        let tools = vec![ToolSchema {
            name: "grep".into(),
            description: "search files".into(),
            parameters: serde_json::json!({}),
        }];
        let messages = PromptBuilder::build(
            "You are helpful.",
            &tools,
            &WorkingMemory::new(),
            &ProjectMemory::new(),
            None,
        );
        assert_eq!(messages.len(), 1);
        assert!(messages[0].content.contains("## Available Tools"));
        assert!(messages[0].content.contains("grep"));
    }

    #[test]
    fn prompt_builder_injects_project_memory() {
        let mut pm = ProjectMemory::new();
        pm.deepseeknova_md = Some("This is a Rust project.".into());

        let messages =
            PromptBuilder::build("You are helpful.", &[], &WorkingMemory::new(), &pm, None);
        assert!(messages[0].content.contains("## Project Context"));
        assert!(messages[0].content.contains("Rust project"));
    }

    /// B6：缓存感知前缀按 global（AGENTS.md）→ project（DEEPSEEKNOVA.md）
    /// 分层注入，段序 static-first（global 先于 project），保证 AGENTS.md
    /// 变化只 invalidate 前缀的 global 段。
    #[test]
    fn cache_aware_prefix_layers_agents_then_project() {
        let mut pm = ProjectMemory::new();
        pm.agents_md = Some("AGENTS_RULES".into());
        pm.deepseeknova_md = Some("PROJECT_CTX".into());

        let mut builder = CacheAwarePromptBuilder::new(false);
        let (prefix, _hash) = builder.build_prefix("SYS", &[], Some(&pm), None);
        let sys_pos = prefix.find("SYS").unwrap();
        let agents_pos = prefix.find("## Project Rules").unwrap();
        let ctx_pos = prefix.find("## Project Context").unwrap();
        assert!(
            sys_pos < agents_pos && agents_pos < ctx_pos,
            "segment order must be system → AGENTS.md → DEEPSEEKNOVA.md:\n{prefix}"
        );
        assert!(prefix.contains("AGENTS_RULES"));
        assert!(prefix.contains("PROJECT_CTX"));
    }

    #[test]
    fn prompt_builder_inserts_compaction_digest() {
        let mut wm = WorkingMemory::new();
        wm.add_message(Message {
            role: Role::User,
            content: "hi".into(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            reasoning_signature: None,
        });
        wm.compaction_digest = Some("summary of earlier conversation".into());

        let messages = PromptBuilder::build("system", &[], &wm, &ProjectMemory::new(), None);
        // system msg + digest + conversation (1 user msg)
        assert_eq!(messages.len(), 3);
        assert!(messages[1].content.contains("conversation-summary"));
    }

    #[test]
    fn prompt_builder_injects_repo_map_after_project_context() {
        let mut pm = ProjectMemory::new();
        pm.deepseeknova_md = Some("PROJECT_CTX".into());
        let map = "crates/x/src/a.rs:\n│ pub fn foo()\n⋮";
        let msgs = PromptBuilder::build("SYS", &[], &WorkingMemory::new(), &pm, Some(map));
        let sys = &msgs[0].content;
        assert!(sys.contains("PROJECT_CTX"));
        assert!(sys.contains("Repo Map"));
        assert!(sys.contains("pub fn foo()"));
        assert!(sys.find("PROJECT_CTX").unwrap() < sys.find("pub fn foo()").unwrap());
    }

    #[test]
    fn prompt_builder_none_map_is_noop() {
        let msgs = PromptBuilder::build(
            "SYS",
            &[],
            &WorkingMemory::new(),
            &ProjectMemory::new(),
            None,
        );
        assert!(!msgs[0].content.contains("Repo Map"));
    }

    #[test]
    fn prompt_builder_no_compaction_when_only_system() {
        let mut wm = WorkingMemory::new();
        wm.compaction_digest = Some("summary".into());
        // No conversation messages -> no digest injection

        let messages = PromptBuilder::build("system", &[], &wm, &ProjectMemory::new(), None);
        // Only the system message, no digest inserted
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::System);
    }

    // --- WorkspaceIndex ---

    #[test]
    fn workspace_scan_temp_dir() {
        let dir =
            std::env::temp_dir().join(format!("deepseeknova-ctx-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Create a test file
        std::fs::write(dir.join("test.rs"), "fn main() {}").unwrap();
        // Create a subdirectory with a file
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src").join("lib.rs"), "pub fn hello() {}").unwrap();

        let ws = WorkspaceIndex::scan(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(ws.root, dir);
        let paths: Vec<&str> = ws
            .file_tree
            .entries
            .iter()
            .filter(|e| !e.is_dir)
            .map(|e| e.path.to_str().unwrap())
            .collect();
        assert!(paths.iter().any(|p| p.ends_with("test.rs")));
        assert!(paths.iter().any(|p| p.ends_with("lib.rs")));
    }

    #[test]
    fn workspace_scan_respects_gitignore() {
        let dir = std::env::temp_dir().join(format!("deepseeknova-ctx-gi-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(dir.join(".gitignore"), "*.log\ntarget/\n").unwrap();
        std::fs::write(dir.join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.join("debug.log"), "log content").unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("target").join("output.o"), "binary").unwrap();

        let ws = WorkspaceIndex::scan(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);

        let file_paths: Vec<&str> = ws
            .file_tree
            .entries
            .iter()
            .filter(|e| !e.is_dir)
            .map(|e| e.path.to_str().unwrap())
            .collect();
        // main.rs and debug.log should be listed (gitignore patterns only
        // apply at the directory level in this implementation)
        assert!(file_paths.iter().any(|p| p.ends_with("main.rs")));
        assert!(file_paths.iter().any(|p| p.ends_with("debug.log")));
        // target/ directory is excluded (matched at directory level)
        assert!(!file_paths.iter().any(|p| p.contains("target/")));
    }
}

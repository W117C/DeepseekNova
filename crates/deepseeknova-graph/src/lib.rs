//! # deepseeknova-graph
//!
//! 代码图引擎：tree-sitter 解析 → SQLite 异构图（FTS5 BM25）→
//! 个性化 PageRank 排序 → 图检索 API 与 token 预算 repo map。

pub mod model;
pub mod parser;

pub use model::*;

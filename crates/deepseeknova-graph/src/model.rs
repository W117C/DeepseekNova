//! 图模型：节点/边类型、稳定 ID、错误类型。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Directory,
    File,
    Struct,
    Enum,
    Trait,
    Class,
    Function,
    Method,
}

impl NodeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Directory => "directory",
            Self::File => "file",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Class => "class",
            Self::Function => "function",
            Self::Method => "method",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "directory" => Self::Directory,
            "file" => Self::File,
            "struct" => Self::Struct,
            "enum" => Self::Enum,
            "trait" => Self::Trait,
            "class" => Self::Class,
            "function" => Self::Function,
            "method" => Self::Method,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    Contains,
    Imports,
    Calls,
    Implements,
    References,
    /// 动态分发桥：trait 方法 → 同名 impl 方法（Rust trait 多态，名称级匹配）。
    Dispatch,
}

impl EdgeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Contains => "contains",
            Self::Imports => "imports",
            Self::Calls => "calls",
            Self::Implements => "implements",
            Self::References => "references",
            Self::Dispatch => "dispatch",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "contains" => Self::Contains,
            "imports" => Self::Imports,
            "calls" => Self::Calls,
            "implements" => Self::Implements,
            "references" => Self::References,
            "dispatch" => Self::Dispatch,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub kind: NodeKind,
    pub name: String,
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub signature: String,
    pub doc: String,
    pub score: f64,
}

#[derive(Debug, Clone)]
pub struct EdgeRec {
    pub src: String,
    pub dst: String,
    pub kind: EdgeKind,
}

pub fn node_id(path: &str, name: &str, start_line: u32) -> String {
    format!("{path}#{name}#{start_line}")
}

#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error("parse error in {path} ({lang})")]
    Parse { path: String, lang: &'static str },
    #[error("storage error: {0}")]
    Storage(#[from] rusqlite::Error),
    #[error("index is busy (refresh in progress)")]
    IndexBusy,
    #[error("entity not found: {0}")]
    EntityNotFound(String),
}

/// 把 [`GraphError`] 转换为 [`deepseeknova_core::DeepseeknovaError`]。
///
/// 本 impl 利用 orphan rule 放在拥有 `GraphError` 的本 crate 中
/// （`DeepseeknovaError` 来自 `deepseeknova-core`，`From` 来自 std）。这使 `?`
/// 运算符能把 `Result<_, GraphError>` 直接用于返回 `Result<_, DeepseeknovaError>`
/// 的函数，无需显式 `.map_err`。
///
/// 当前映射保留人可读消息（`to_string()`），丢失变体级别的类型信息；未来若
/// 需在 `DeepseeknovaError` 上做按变体 dispatch，可在 core 加 `Graph(Box<dyn
/// std::error::Error>)` 之类的富类型变体（additive，不破坏下游 match）。
impl From<GraphError> for deepseeknova_core::DeepseeknovaError {
    fn from(err: GraphError) -> Self {
        deepseeknova_core::DeepseeknovaError::Graph(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_id_is_stable_and_readable() {
        assert_eq!(node_id("src/a.rs", "foo", 10), "src/a.rs#foo#10");
    }

    #[test]
    fn kind_roundtrip() {
        for k in [
            NodeKind::Directory,
            NodeKind::File,
            NodeKind::Struct,
            NodeKind::Enum,
            NodeKind::Trait,
            NodeKind::Class,
            NodeKind::Function,
            NodeKind::Method,
        ] {
            assert_eq!(NodeKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(NodeKind::parse("nope"), None);
        for e in [
            EdgeKind::Contains,
            EdgeKind::Imports,
            EdgeKind::Calls,
            EdgeKind::Implements,
            EdgeKind::References,
        ] {
            assert_eq!(EdgeKind::parse(e.as_str()), Some(e));
        }
    }

    /// 验证 `From<GraphError> for DeepseeknovaError` 让 `?` 直接把
    /// `Result<_, GraphError>` 用于返回 `Result<_, DeepseeknovaError>` 的函数。
    /// 这是 P1-3 Phase 2 在 graph crate 的 pilot：orphan rule impl 落地。
    #[test]
    fn graph_error_converts_to_deepseeknova_error_via_question_mark() {
        fn inner() -> Result<(), GraphError> {
            Err(GraphError::EntityNotFound("missing".into()))
        }
        fn outer() -> Result<(), deepseeknova_core::DeepseeknovaError> {
            inner()?;
            Ok(())
        }
        let err = outer().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("entity not found"),
            "应保留 GraphError 的消息: {msg}"
        );
        assert!(
            msg.contains("graph error"),
            "应通过 DeepseeknovaError::Graph 变体渲染: {msg}"
        );
        assert!(!err.is_retryable(), "Graph 错误默认不可重试");
    }

    /// 验证 `IndexBusy` 与 `Storage` 变体也走同一映射路径。
    #[test]
    fn graph_error_variants_all_map_to_graph_category() {
        let cases: Vec<GraphError> = vec![
            GraphError::IndexBusy,
            GraphError::Parse {
                path: "src/x.rs".into(),
                lang: "rust",
            },
            GraphError::Storage(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(1),
                Some("database locked".into()),
            )),
        ];
        for ge in cases {
            let de: deepseeknova_core::DeepseeknovaError = ge.into();
            assert!(
                matches!(de, deepseeknova_core::DeepseeknovaError::Graph(_)),
                "所有 GraphError 变体应映射到 Graph 类别"
            );
        }
    }
}

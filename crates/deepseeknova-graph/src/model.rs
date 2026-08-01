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
}

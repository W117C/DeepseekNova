//! 图模型：节点/边类型、稳定 ID、错误类型。

use serde::{Deserialize, Serialize};

/// 节点种类（图检索与 repo map 分类用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// 源文件节点。
    File,
    /// Rust/Go 的 struct 类型。
    Struct,
    /// Rust/Go 的 enum 类型。
    Enum,
    /// Rust trait 类型。
    Trait,
    /// Python/JS/TS 的 class 类型。
    Class,
    /// 顶层函数。
    Function,
    /// 类 / impl 内方法。
    Method,
}

impl NodeKind {
    /// 序列化为存储与 JSON 使用的 snake_case 字符串（`parse` 的反函数）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Class => "class",
            Self::Function => "function",
            Self::Method => "method",
        }
    }
    /// 反序列化：字符串 → [`NodeKind`]；无法识别返回 None。
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
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

/// 有向边类型（代码图的关系维度）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// 层级归属：目录/文件/类型包含子符号。
    Contains,
    /// 跨文件依赖：文件导入符号或文件。
    Imports,
    /// 调用关系（callee 名级匹配）。
    Calls,
    /// 引用关系：定义体引用的标识符（名称级，call callee 不在内）。
    References,
    /// 动态分发桥：trait 方法 → 同名 impl 方法（Rust trait 多态，名称级匹配）。
    Dispatch,
}

impl EdgeKind {
    /// 序列化为存储使用的 snake_case 字符串（`parse` 的反函数）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Contains => "contains",
            Self::Imports => "imports",
            Self::Calls => "calls",
            Self::References => "references",
            Self::Dispatch => "dispatch",
        }
    }
    /// 反序列化：字符串 → [`EdgeKind`]；无法识别返回 None。
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "contains" => Self::Contains,
            "imports" => Self::Imports,
            "calls" => Self::Calls,
            "references" => Self::References,
            "dispatch" => Self::Dispatch,
            _ => return None,
        })
    }
}

/// 代码图节点：一个符号（文件/函数/类型…）的稳定表示。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    /// 稳定节点 ID（`path#name#start_line`），全库唯一。
    pub id: String,
    /// 节点种类。
    pub kind: NodeKind,
    /// 符号名（已去限定符）。
    pub name: String,
    /// 相对 workspace 根的文件路径。
    pub path: String,
    /// 定义起始行（1-based）。
    pub start_line: u32,
    /// 定义结束行（1-based，含函数体）。
    pub end_line: u32,
    /// 提取的签名（如 `pub fn foo(a: i32) -> bool`）。
    pub signature: String,
    /// 定义上方提取的文档注释（可为空串）。
    pub doc: String,
    /// PageRank 分数（`refresh` 与 `repo_map` 写入）。
    pub score: f64,
}

/// 一条有向边记录（源节点 id → 目标节点 id）。
#[derive(Debug, Clone)]
pub struct EdgeRec {
    /// 源节点 id。
    pub src: String,
    /// 目标节点 id。
    pub dst: String,
    /// 边类型。
    pub kind: EdgeKind,
}

/// 生成稳定节点 ID：`{path}#{name}#{start_line}`。
pub fn node_id(path: &str, name: &str, start_line: u32) -> String {
    format!("{path}#{name}#{start_line}")
}

/// 图索引错误类型。
#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    /// 源文件解析失败（tree-sitter 语法错误或语言不支持）。
    #[error("parse error in {path} ({lang})")]
    Parse {
        /// 出错文件路径。
        path: String,
        /// 源语言标识（如 "rust"）。
        lang: &'static str,
    },
    /// 底层 SQLite 存储错误。
    #[error("storage error: {0}")]
    Storage(#[from] rusqlite::Error),
    /// 索引忙（refresh 进行中，数据库被锁）。
    #[error("index is busy (refresh in progress)")]
    IndexBusy,
    /// 实体未找到（`resolve`/`get` 无法定位）。
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
/// 映射保留原始 `GraphError` 实例（装箱为 `Box<dyn Error>`），调用方可通过
/// `err.source().downcast_ref::<GraphError>()` 恢复 `Parse` / `Storage` /
/// `IndexBusy` / `EntityNotFound` 等具体变体，不再丢失类型信息与 source 链。
impl From<GraphError> for deepseeknova_core::DeepseeknovaError {
    fn from(err: GraphError) -> Self {
        deepseeknova_core::DeepseeknovaError::Graph(Box::new(err))
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

    /// 验证 `From<GraphError>` 保留原始错误实例与 source 链：调用方可通过
    /// `source().downcast_ref::<GraphError>()` 恢复具体变体，不再丢失类型信息。
    #[test]
    fn graph_error_source_preserves_variant_for_downcast() {
        let ge = GraphError::IndexBusy;
        let de: deepseeknova_core::DeepseeknovaError = ge.into();
        use std::error::Error as _;
        let src = de
            .source()
            .expect("Graph 变体应持有 source")
            .downcast_ref::<GraphError>()
            .expect("source 应可 downcast 回 GraphError");
        assert!(
            matches!(src, GraphError::IndexBusy),
            "downcast 后应保留具体变体 IndexBusy"
        );
    }

    /// 验证 `From<GraphError::EntityNotFound>` 保留 EntityNotFound 变体。
    #[test]
    fn graph_error_entity_not_found_preserves_through_source() {
        let ge = GraphError::EntityNotFound("node-42".into());
        let de: deepseeknova_core::DeepseeknovaError = ge.into();
        use std::error::Error as _;
        let src = de
            .source()
            .expect("Graph 变体应持有 source")
            .downcast_ref::<GraphError>()
            .expect("source 应可 downcast 回 GraphError");
        match src {
            GraphError::EntityNotFound(id) => assert_eq!(id, "node-42"),
            other => panic!("期望 EntityNotFound，得到 {other:?}"),
        }
    }
}

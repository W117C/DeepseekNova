//! tree-sitter 五语言解析：实体提取 + 名称级 calls/imports。

use crate::model::{node_id, GraphError, Node, NodeKind};
use tree_sitter::{Language, Node as TsNode, Parser};

/// 支持的源语言。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Go,
}

impl Lang {
    /// 按文件扩展名判定语言；不识别的扩展返回 None。
    pub fn from_path(path: &str) -> Option<Lang> {
        let ext = std::path::Path::new(path).extension()?.to_str()?;
        match ext {
            "rs" => Some(Lang::Rust),
            "py" => Some(Lang::Python),
            "ts" | "tsx" => Some(Lang::TypeScript),
            "js" | "jsx" | "mjs" | "cjs" => Some(Lang::JavaScript),
            "go" => Some(Lang::Go),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Lang::Rust => "rust",
            Lang::Python => "python",
            Lang::JavaScript => "javascript",
            Lang::TypeScript => "typescript",
            Lang::Go => "go",
        }
    }

    fn language(&self) -> Language {
        match self {
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Lang::Go => tree_sitter_go::LANGUAGE.into(),
        }
    }
}

/// 结构化 import 链接类型：本地符号（按名匹配）/ 本地文件（相对路径）/ 外部依赖。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportKind {
    Symbol,
    File,
    External,
}

impl ImportKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Symbol => "symbol",
            Self::File => "file",
            Self::External => "external",
        }
    }
}

/// 一条结构化 import 事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportLink {
    pub kind: ImportKind,
    pub target: String,
}

/// 单文件解析结果：实体节点 + 名称级调用对 + import 事实 + 符号引用。
pub struct FileParse {
    pub nodes: Vec<Node>,
    pub calls: Vec<(String, String)>,
    pub imports: Vec<String>,
    /// 结构化 import 事实（Rust/Python 路径段、JS/TS specifier）。
    pub import_links: Vec<ImportLink>,
    /// Rust：trait 声明的方法 (trait 名, 方法名, 起始行)，用于构建动态分发桥。
    pub trait_methods: Vec<(String, String, u32)>,
    /// Rust：`impl Trait for Type` 内的方法 (trait 名, 实现类型名, 方法名, 起始行)。
    pub impl_trait_methods: Vec<(String, String, String, u32)>,
    /// 定义体引用的标识符（from, ref_name），名称级；call callee 不在内。
    pub refs: Vec<(String, String)>,
}

/// 每个定义体最多采集的引用名数量（防 raw_refs 膨胀）。
const MAX_REFS_PER_DEF: usize = 64;

fn parse_err(path: &str, lang: Lang) -> GraphError {
    GraphError::Parse {
        path: path.into(),
        lang: lang.as_str(),
    }
}

/// 判定定义实体的 NodeKind；非实体返回 None。
/// Go 的 type_declaration 不在此判定（分组声明需遍历子节点，见 parse_source）。
fn entity_kind(lang: Lang, kind: &str, ancestors: &[&str]) -> Option<NodeKind> {
    match lang {
        Lang::Rust => match kind {
            "struct_item" => Some(NodeKind::Struct),
            "enum_item" => Some(NodeKind::Enum),
            "trait_item" => Some(NodeKind::Trait),
            "function_item" | "function_signature_item" => {
                Some(if ancestors.contains(&"impl_item") {
                    NodeKind::Method
                } else {
                    NodeKind::Function
                })
            }
            _ => None,
        },
        Lang::Python => match kind {
            "class_definition" => Some(NodeKind::Class),
            "function_definition" => Some(if ancestors.contains(&"class_definition") {
                NodeKind::Method
            } else {
                NodeKind::Function
            }),
            _ => None,
        },
        Lang::JavaScript | Lang::TypeScript => match kind {
            "class_declaration" => Some(NodeKind::Class),
            "function_declaration" => Some(NodeKind::Function),
            "method_definition" => Some(NodeKind::Method),
            _ => None,
        },
        Lang::Go => match kind {
            "function_declaration" => Some(NodeKind::Function),
            "method_declaration" => Some(NodeKind::Method),
            // type_spec/type_alias 不在此判定实体（单声明与分组 `type ( ... )`
            // 均由 parse_source 的 Go 分支在成员节点 Enter 时逐个产出，见 parse_source）。
            _ => None,
        },
    }
}

fn is_import(lang: Lang, kind: &str) -> bool {
    match lang {
        Lang::Rust => kind == "use_declaration",
        Lang::Python => kind == "import_statement" || kind == "import_from_statement",
        Lang::JavaScript | Lang::TypeScript => kind == "import_statement",
        Lang::Go => kind == "import_declaration" || kind == "import_spec",
    }
}

fn is_call(lang: Lang, kind: &str) -> bool {
    match lang {
        Lang::Python => kind == "call",
        _ => kind == "call_expression",
    }
}

/// 从 import 文本提取标识符 token（过滤语言关键字）。
fn identifier_tokens(text: &str) -> Vec<String> {
    const KEYWORDS: &[&str] = &[
        "use", "as", "import", "from", "pub", "crate", "self", "super", "mod", "extern",
    ];
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|t| !t.is_empty())
        .filter(|t| !KEYWORDS.contains(t))
        .map(str::to_string)
        .collect()
}

fn strip_quotes(text: &str) -> &str {
    text.trim_matches(|c| c == '"' || c == '\'')
}

/// 取定义实体的名称：优先 `name` 字段，退回扫描 identifier 类子节点。
fn entity_name(node: TsNode, src: &str) -> Option<String> {
    if let Some(n) = node.child_by_field_name("name") {
        return n.utf8_text(src.as_bytes()).ok().map(str::to_string);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if matches!(
            child.kind(),
            "identifier" | "type_identifier" | "property_identifier"
        ) {
            return child.utf8_text(src.as_bytes()).ok().map(str::to_string);
        }
    }
    None
}

/// 签名：定义起始字节到「该行末尾或首个语言体分隔符（Rust/JS/TS 为 `{`，Python 为 `:`）」
/// 之前的子串，压平空白为单空格。
fn extract_signature(lang: Lang, node: TsNode, src: &str) -> String {
    let bytes = src.as_bytes();
    let start = node.start_byte().min(bytes.len());
    let end = node.end_byte().min(bytes.len());
    let slice = &bytes[start..end];
    let stop = match lang {
        Lang::Python => b':',
        _ => b'{',
    };
    let cut = slice
        .iter()
        .position(|&b| b == b'\n' || b == stop)
        .unwrap_or(slice.len());
    // cut 落在 ASCII 字节上，切片必然是合法 UTF-8 边界。
    std::str::from_utf8(&slice[..cut])
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// 定义前紧邻注释的首行，去掉注释标记后 trim；无则空串。
fn extract_doc(node: TsNode, src: &str) -> String {
    let Some(prev) = node.prev_sibling() else {
        return String::new();
    };
    if !prev.kind().contains("comment") {
        return String::new();
    }
    let end_row = prev.end_position().row;
    let start_row = node.start_position().row;
    if start_row != end_row && start_row != end_row + 1 {
        return String::new();
    }
    let text = prev.utf8_text(src.as_bytes()).unwrap_or("");
    let first = text.lines().next().unwrap_or("").trim();
    let stripped = first
        .strip_prefix("///")
        .or_else(|| first.strip_prefix("//!"))
        .or_else(|| first.strip_prefix("/**"))
        .or_else(|| first.strip_prefix("/*"))
        .or_else(|| first.strip_prefix("//"))
        .or_else(|| first.strip_prefix('#'))
        .unwrap_or(first);
    stripped.trim_end_matches("*/").trim().to_string()
}

/// 取被调用名：identifier 直接用；scoped/member/attribute/selector 等取末段。
fn callee_name(call: TsNode, src: &str) -> Option<String> {
    let func = call.child_by_field_name("function")?;
    let target = match func.kind() {
        "identifier" => func,
        "scoped_identifier" => func.child_by_field_name("name")?,
        "field_expression" => func.child_by_field_name("field")?,
        "member_expression" => func.child_by_field_name("property")?,
        "attribute" => func.child_by_field_name("attribute")?,
        // Go：pkg.Func / recv.Method → 取末段字段名
        "selector_expression" => func.child_by_field_name("field")?,
        _ => return None,
    };
    target.utf8_text(src.as_bytes()).ok().map(str::to_string)
}

enum Step<'t> {
    Enter(TsNode<'t>),
    Exit {
        /// 本节点产出的实体数（Go 分组 type 声明可为多个），Exit 时逐个出栈。
        pop_def: usize,
        pop_trait: bool,
        pop_impl: bool,
    },
}

/// 解析单个源文件，提取定义实体、名称级调用对与 import 语句。
pub fn parse_source(lang: Lang, path: &str, src: &str) -> Result<FileParse, GraphError> {
    let mut parser = Parser::new();
    parser
        .set_language(&lang.language())
        .map_err(|_| parse_err(path, lang))?;
    let tree = parser
        .parse(src, None)
        .ok_or_else(|| parse_err(path, lang))?;

    let mut nodes: Vec<Node> = Vec::new();
    let mut calls: Vec<(String, String)> = Vec::new();
    let mut imports: Vec<String> = Vec::new();
    let mut import_links: Vec<ImportLink> = Vec::new();
    let mut trait_methods: Vec<(String, String, u32)> = Vec::new();
    let mut impl_trait_methods: Vec<(String, String, String, u32)> = Vec::new();
    let mut refs: Vec<(String, String)> = Vec::new();
    // 最近的命名定义栈（caller 归属）与祖先 kind 栈（Method 判定）。
    let mut def_stack: Vec<String> = Vec::new();
    // 与 def_stack 对齐的引用名集合栈（每个定义体一个，去重 + 上限）。
    let mut refs_stack: Vec<std::collections::HashSet<String>> = Vec::new();
    let mut ancestor_kinds: Vec<&str> = Vec::new();
    // Rust trait / impl 上下文栈（名称级动态分发事实）。
    let mut trait_stack: Vec<String> = Vec::new();
    let mut impl_trait_stack: Vec<Option<(String, String)>> = Vec::new();
    let mut work: Vec<Step> = vec![Step::Enter(tree.root_node())];

    while let Some(step) = work.pop() {
        match step {
            Step::Exit {
                pop_def,
                pop_trait,
                pop_impl,
            } => {
                ancestor_kinds.pop();
                if pop_trait {
                    trait_stack.pop();
                }
                if pop_impl {
                    impl_trait_stack.pop();
                }
                for _ in 0..pop_def {
                    if let (Some(name), Some(set)) = (def_stack.last(), refs_stack.pop()) {
                        for ref_name in set {
                            if ref_name != *name {
                                refs.push((name.clone(), ref_name));
                            }
                        }
                    }
                    def_stack.pop();
                }
            }
            Step::Enter(node) => {
                let kind = node.kind();
                let mut pop_trait = false;
                let mut pop_impl = false;
                if lang == Lang::Rust {
                    if kind == "trait_item" {
                        if let Some(name) = entity_name(node, src) {
                            trait_stack.push(name);
                            pop_trait = true;
                        }
                    } else if kind == "impl_item" {
                        let trait_name = node
                            .child_by_field_name("trait")
                            .and_then(|n| n.utf8_text(src.as_bytes()).ok())
                            .map(str::trim)
                            .map(str::to_string);
                        let impl_type = node
                            .child_by_field_name("type")
                            .and_then(|n| n.utf8_text(src.as_bytes()).ok())
                            .map(str::trim)
                            .map(str::to_string);
                        impl_trait_stack.push(trait_name.zip(impl_type));
                        pop_impl = true;
                    }
                }
                if is_import(lang, kind) {
                    if let Ok(text) = node.utf8_text(src.as_bytes()) {
                        if lang == Lang::Go {
                            // Go：仅 import_spec 逐条采集（import_declaration 本身跳过，
                            // 避免整段重复）；path 三态：相对路径=File，其余=External。
                            if kind == "import_spec" {
                                if let Some(path) = node
                                    .child_by_field_name("path")
                                    .and_then(|n| n.utf8_text(src.as_bytes()).ok())
                                {
                                    let spec = strip_quotes(path).to_string();
                                    imports.push(spec.clone());
                                    let kind = if spec.starts_with("./") || spec.starts_with("../")
                                    {
                                        ImportKind::File
                                    } else {
                                        ImportKind::External
                                    };
                                    import_links.push(ImportLink { kind, target: spec });
                                }
                            }
                        } else {
                            imports.push(text.trim().to_string());
                            if matches!(lang, Lang::JavaScript | Lang::TypeScript) {
                                if let Some(source) = node
                                    .child_by_field_name("source")
                                    .and_then(|n| n.utf8_text(src.as_bytes()).ok())
                                {
                                    let spec = strip_quotes(source).to_string();
                                    let kind = if spec.starts_with("./")
                                        || spec.starts_with("../")
                                        || spec.starts_with('/')
                                    {
                                        ImportKind::File
                                    } else {
                                        ImportKind::External
                                    };
                                    import_links.push(ImportLink { kind, target: spec });
                                }
                            } else {
                                let mut seen = std::collections::HashSet::new();
                                for tok in identifier_tokens(text) {
                                    if seen.insert(tok.clone()) {
                                        import_links.push(ImportLink {
                                            kind: ImportKind::Symbol,
                                            target: tok,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
                if is_call(lang, kind) {
                    // JS/TS require('pkg') 也是依赖事实（相对路径=本地文件，裸名=外部）。
                    if matches!(lang, Lang::JavaScript | Lang::TypeScript)
                        && callee_name(node, src).as_deref() == Some("require")
                    {
                        if let Some(arg) = node
                            .child_by_field_name("arguments")
                            .and_then(|a| a.named_child(0))
                            .and_then(|a| a.utf8_text(src.as_bytes()).ok())
                        {
                            let spec = strip_quotes(arg).to_string();
                            let kind = if spec.starts_with("./")
                                || spec.starts_with("../")
                                || spec.starts_with('/')
                            {
                                ImportKind::File
                            } else {
                                ImportKind::External
                            };
                            import_links.push(ImportLink { kind, target: spec });
                        }
                    }
                    if let (Some(caller), Some(callee)) = (def_stack.last(), callee_name(node, src))
                    {
                        calls.push((caller.clone(), callee));
                    }
                }
                // 定义体内的标识符引用采集：跳过 call 的 callee（已走 Calls 边）。
                if matches!(kind, "identifier" | "type_identifier") {
                    if let Some(set) = refs_stack.last_mut() {
                        let is_callee = node.parent().is_some_and(|p| {
                            p.kind().contains("call")
                                && p.child_by_field_name("function")
                                    .is_some_and(|f| f.id() == node.id())
                        });
                        if !is_callee {
                            if let Ok(name) = node.utf8_text(src.as_bytes()) {
                                if set.len() < MAX_REFS_PER_DEF {
                                    set.insert(name.to_string());
                                }
                            }
                        }
                    }
                }
                let mut pushed_def: usize = 0;
                // 实体产出：Go 的 type_spec/type_alias 成员（单声明与分组
                // `type ( A struct{}; B ... )` 的 tree-sitter-go 0.25 形态一致，
                // type_declaration 下 children 为 multiple）在成员节点 Enter 时
                // 逐个产出实体、成员子树 Exit 时出栈（pop_def 计数）——与单实体
                // 路径一致，成员体内引用与自身 name 节点归属本成员（R1）；
                // 其余语言经 entity_kind/entity_name 判定单一实体。
                let mut push_entity =
                    |nk: NodeKind, name: String, ent: TsNode, sig: String, doc: String| {
                        let start_line = ent.start_position().row as u32 + 1;
                        nodes.push(Node {
                            id: node_id(path, &name, start_line),
                            kind: nk,
                            name: name.clone(),
                            path: path.to_string(),
                            start_line,
                            end_line: ent.end_position().row as u32 + 1,
                            signature: sig,
                            doc,
                            score: 0.0,
                        });
                        refs_stack.push(std::collections::HashSet::new());
                        if lang == Lang::Rust && nk == NodeKind::Method {
                            if let Some(tname) = trait_stack.last() {
                                trait_methods.push((tname.clone(), name.clone(), start_line));
                            }
                            if let Some(Some((tname, itype))) = impl_trait_stack.last() {
                                impl_trait_methods.push((
                                    tname.clone(),
                                    itype.clone(),
                                    name.clone(),
                                    start_line,
                                ));
                            }
                        }
                        if lang == Lang::Rust && nk == NodeKind::Function {
                            if let Some(tname) = trait_stack.last() {
                                trait_methods.push((tname.clone(), name.clone(), start_line));
                            }
                        }
                        def_stack.push(name);
                        pushed_def += 1;
                    };
                if lang == Lang::Go && matches!(kind, "type_spec" | "type_alias") {
                    // 单声明 `type A struct{}` 与分组 `type ( A ...; B ... )`
                    // 均落到此：成员 Enter 时建实体、成员子树 Exit 时 pop。
                    // type_spec/type_alias 的 type 字段区分 struct/interface。
                    let nk = match node.child_by_field_name("type").map(|t| t.kind()) {
                        Some("struct_type") => Some(NodeKind::Struct),
                        Some("interface_type") => Some(NodeKind::Trait),
                        _ => None,
                    };
                    if let Some(nk) = nk {
                        if let Some(name) = node
                            .child_by_field_name("name")
                            .and_then(|n| n.utf8_text(src.as_bytes()).ok())
                            .map(str::to_string)
                        {
                            // R2 doc：成员自身紧邻注释优先（分组内逐成员注释），
                            // 取不到时回退父节点 type_declaration——单声明注释在
                            // `type` 关键字之前，type_spec 的 prev_sibling 是
                            // "type" 匿名关键字节点；signature 保留 "type " 前缀
                            // 与 G-M1 前旧行为等价。
                            let direct_doc = extract_doc(node, src);
                            let doc = if direct_doc.is_empty() {
                                node.parent()
                                    .map(|p| extract_doc(p, src))
                                    .unwrap_or_default()
                            } else {
                                direct_doc
                            };
                            let sig = format!("type {}", extract_signature(lang, node, src));
                            push_entity(nk, name, node, sig, doc);
                        }
                    }
                } else if let Some(nk) = entity_kind(lang, kind, &ancestor_kinds) {
                    if let Some(name) = entity_name(node, src) {
                        push_entity(
                            nk,
                            name,
                            node,
                            extract_signature(lang, node, src),
                            extract_doc(node, src),
                        );
                    }
                }
                ancestor_kinds.push(kind);
                work.push(Step::Exit {
                    pop_def: pushed_def,
                    pop_trait,
                    pop_impl,
                });
                for i in (0..node.child_count()).rev() {
                    if let Some(child) = node.child(i) {
                        work.push(Step::Enter(child));
                    }
                }
            }
        }
    }

    Ok(FileParse {
        nodes,
        calls,
        imports,
        import_links,
        trait_methods,
        impl_trait_methods,
        refs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::NodeKind;

    const RUST_SRC: &str = "use std::collections::HashMap;\n\n\
/// A widget.\npub struct Widget { pub id: u32 }\n\n\
pub fn make() -> Widget {
    let w = Widget { id: 1 };
    helper(w)
}

\
fn helper(w: Widget) -> Widget { w }\n";

    const GO_SRC: &str = r#"package main

import (
    "fmt"
    "os"
    "example.com/lib"
    "./localpkg"
)

type User struct {
    Name string
}

type Greeter interface {
    Greet() string
}

func (u User) Greet() string {
    return "hi " + u.Name
}

func MakeUser(name string) *User {
    u := User{Name: name}
    u.Greet()
    helper()
    return &u
}

func helper() {
    internal()
}

func internal() {
    fmt.Println("x")
}
"#;

    #[test]
    fn parses_rust_entities() {
        let fp = parse_source(Lang::Rust, "src/w.rs", RUST_SRC).unwrap();
        let names: Vec<_> = fp.nodes.iter().map(|n| (n.kind, n.name.as_str())).collect();
        assert!(names.contains(&(NodeKind::Struct, "Widget")));
        assert!(names.contains(&(NodeKind::Function, "make")));
        assert!(names.contains(&(NodeKind::Function, "helper")));
        let make = fp.nodes.iter().find(|n| n.name == "make").unwrap();
        assert!(make.signature.contains("pub fn make"));
        assert!(!make.signature.contains('{'));
        let widget = fp.nodes.iter().find(|n| n.name == "Widget").unwrap();
        assert_eq!(widget.doc, "A widget.");
    }

    #[test]
    fn extracts_rust_calls_and_imports() {
        let fp = parse_source(Lang::Rust, "src/w.rs", RUST_SRC).unwrap();
        assert!(fp
            .calls
            .iter()
            .any(|(caller, callee)| caller == "make" && callee == "helper"));
        assert!(fp.imports.iter().any(|i| i.contains("HashMap")));
    }

    #[test]
    fn lang_from_extension() {
        assert_eq!(Lang::from_path("a.rs"), Some(Lang::Rust));
        assert_eq!(Lang::from_path("a.py"), Some(Lang::Python));
        assert_eq!(Lang::from_path("a.js"), Some(Lang::JavaScript));
        assert_eq!(Lang::from_path("a.ts"), Some(Lang::TypeScript));
        assert_eq!(Lang::from_path("a.tsx"), Some(Lang::TypeScript));
        assert_eq!(Lang::from_path("a.go"), Some(Lang::Go));
        assert_eq!(Lang::from_path("a.md"), None);
    }

    #[test]
    fn parses_go_grouped_type_declarations() {
        // 分组类型声明：一个 type_declaration 含多个 type_spec（tree-sitter-go
        // 0.25 children 为 multiple），组内每个类型都要产出实体（G-M1）。
        // 单行分号分隔与多行两个形态都覆盖。
        for src in [
            "package main\n\ntype ( A struct{}; B interface{} )\n",
            "package main\n\ntype (\n\tA struct{}\n\tB interface{}\n)\n",
        ] {
            let fp = parse_source(Lang::Go, "src/grouped.go", src).unwrap();
            let names: Vec<_> = fp.nodes.iter().map(|n| (n.kind, n.name.as_str())).collect();
            assert!(
                names.contains(&(NodeKind::Struct, "A")),
                "分组内 A 应产出 Struct 实体：{names:?}"
            );
            assert!(
                names.contains(&(NodeKind::Trait, "B")),
                "分组内 B 应产出 Trait 实体：{names:?}"
            );
            assert_eq!(fp.nodes.len(), 2, "分组声明应恰好 2 个实体：{names:?}");
        }
        // 分组内不含实体的 type_spec（如 type 别名到非 struct/interface）不产出。
        let src = "package main\n\ntype (\n\tA = int\n\tB struct{}\n)\n";
        let fp = parse_source(Lang::Go, "src/grouped2.go", src).unwrap();
        let names: Vec<_> = fp.nodes.iter().map(|n| (n.kind, n.name.as_str())).collect();
        assert_eq!(names, vec![(NodeKind::Struct, "B")], "{names:?}");
    }

    #[test]
    fn go_grouped_type_refs_attribution() {
        // R1：分组 type_declaration 成员体内引用归属各自成员。修复前全部成员在
        // 父节点 Enter 时一次性 push（栈顶=最后成员），成员 1..n-1 的体内引用与
        // 自身 name 全部落入最后成员 set：refs=[(B,"A"),(B,"C")] 伪边、A 无出边。
        for src in [
            // 单行分号分隔（第二轮审查 /tmp 实测形态）
            "package main\n\ntype ( A struct{ next *B; ext *C }; B struct{} )\n",
            // 多行形态
            "package main\n\ntype (\n\tA struct{ next *B; ext *C }\n\tB struct{}\n)\n",
        ] {
            let fp = parse_source(Lang::Go, "src/grouped_refs.go", src).unwrap();
            // A 的体内引用：B、C
            assert!(
                fp.refs.contains(&("A".into(), "B".into())),
                "A 应引用 B：{:?}",
                fp.refs
            );
            assert!(
                fp.refs.contains(&("A".into(), "C".into())),
                "A 应引用 C：{:?}",
                fp.refs
            );
            // B 无出边（不得有伪边 B→A，也不得收走 A 的引用）
            assert!(
                !fp.refs.iter().any(|(from, _)| from == "B"),
                "B 不应有出边（含伪边 B→A）：{:?}",
                fp.refs
            );
        }
        // 分组后的方法调用归属：M→helper；组内成员不残留为 caller。
        let src = "package main\n\ntype (\n\tA struct{ next *B; ext *C }\n\tB struct{}\n)\n\nfunc (a A) M() { helper() }\n";
        let fp = parse_source(Lang::Go, "src/grouped_refs.go", src).unwrap();
        assert!(
            fp.calls.contains(&("M".into(), "helper".into())),
            "方法调用应归属 M→helper：{:?}",
            fp.calls
        );
        assert!(
            !fp.calls.iter().any(|(c, _)| c == "A" || c == "B"),
            "组内类型不得残留为 caller：{:?}",
            fp.calls
        );
    }

    #[test]
    fn go_type_doc_and_signature_restored() {
        // R2：实体从 type_declaration 改为 type_spec 后，doc 需回退父节点提取、
        // signature 保留 "type " 前缀（与 G-M1 前旧行为等价）。
        // 单声明：
        let fp = parse_source(
            Lang::Go,
            "src/user.go",
            "package main\n\n// User is a user.\ntype User struct {\n\tName string\n}\n",
        )
        .unwrap();
        let user = fp.nodes.iter().find(|n| n.name == "User").unwrap();
        assert_eq!(user.doc, "User is a user.", "doc 应恢复：{:?}", user.doc);
        assert_eq!(
            user.signature, "type User struct",
            "signature 应保留 type 前缀"
        );
        // 分组声明：组注释归属首成员；每成员 signature 均带 "type " 前缀。
        let fp = parse_source(
            Lang::Go,
            "src/grouped.go",
            "package main\n\n// Group types.\ntype (\n\tA struct{}\n\tB interface{}\n)\n",
        )
        .unwrap();
        let a = fp.nodes.iter().find(|n| n.name == "A").unwrap();
        let b = fp.nodes.iter().find(|n| n.name == "B").unwrap();
        assert_eq!(a.doc, "Group types.", "分组首成员应取组注释：{:?}", a.doc);
        assert_eq!(a.signature, "type A struct", "A signature: {}", a.signature);
        assert_eq!(
            b.signature, "type B interface",
            "B signature: {}",
            b.signature
        );
    }

    #[test]
    fn parses_go_entities() {
        let fp = parse_source(Lang::Go, "src/main.go", GO_SRC).unwrap();
        let names: Vec<_> = fp.nodes.iter().map(|n| (n.kind, n.name.as_str())).collect();
        assert!(names.contains(&(NodeKind::Struct, "User")), "{names:?}");
        assert!(names.contains(&(NodeKind::Trait, "Greeter")), "{names:?}");
        assert!(
            names.contains(&(NodeKind::Function, "MakeUser")),
            "{names:?}"
        );
        assert!(names.contains(&(NodeKind::Function, "helper")), "{names:?}");
        assert!(names.contains(&(NodeKind::Method, "Greet")), "{names:?}");
        let make_user = fp.nodes.iter().find(|n| n.name == "MakeUser").unwrap();
        assert!(
            make_user
                .signature
                .contains("func MakeUser(name string) *User"),
            "Go 签名应从 func 到左花括号前：{}",
            make_user.signature
        );
        assert!(!make_user.signature.contains('{'));
    }

    #[test]
    fn extracts_go_calls_and_imports() {
        let fp = parse_source(Lang::Go, "src/main.go", GO_SRC).unwrap();
        // 调用链 a→b→c：MakeUser → helper → internal
        assert!(
            fp.calls
                .iter()
                .any(|(c, e)| c == "MakeUser" && e == "helper"),
            "{:?}",
            fp.calls
        );
        assert!(
            fp.calls
                .iter()
                .any(|(c, e)| c == "helper" && e == "internal"),
            "{:?}",
            fp.calls
        );
        // 方法调用取末段：u.Greet() → Greet
        assert!(
            fp.calls
                .iter()
                .any(|(c, e)| c == "MakeUser" && e == "Greet"),
            "{:?}",
            fp.calls
        );
        // 包级调用取末段：fmt.Println → Println
        assert!(
            fp.calls
                .iter()
                .any(|(c, e)| c == "internal" && e == "Println"),
            "{:?}",
            fp.calls
        );
        assert!(fp.imports.iter().any(|i| i == "fmt"), "{:?}", fp.imports);
        assert!(
            fp.imports.iter().any(|i| i == "example.com/lib"),
            "{:?}",
            fp.imports
        );
    }

    #[test]
    fn go_import_three_states() {
        let fp = parse_source(Lang::Go, "src/main.go", GO_SRC).unwrap();
        // 本地相对路径 → File
        assert!(fp.import_links.contains(&ImportLink {
            kind: ImportKind::File,
            target: "./localpkg".into()
        }));
        // stdlib / 第三方裸路径 → External
        assert!(fp.import_links.contains(&ImportLink {
            kind: ImportKind::External,
            target: "fmt".into()
        }));
        assert!(fp.import_links.contains(&ImportLink {
            kind: ImportKind::External,
            target: "example.com/lib".into()
        }));
        // 无 Symbol 态（Go import 不产生符号级链接）
        assert!(fp.import_links.iter().all(|l| l.kind != ImportKind::Symbol));
    }

    #[test]
    fn go_collects_type_references_without_callees() {
        let fp = parse_source(Lang::Go, "src/main.go", GO_SRC).unwrap();
        // MakeUser 引用 User 类型
        assert!(
            fp.refs.iter().any(|(f, r)| f == "MakeUser" && r == "User"),
            "{:?}",
            fp.refs
        );
        // 递归/自调用不进引用
        assert!(!fp
            .refs
            .iter()
            .any(|(f, r)| f == "internal" && r == "internal"));
    }

    #[test]
    fn parses_python_and_js() {
        let py = parse_source(
            Lang::Python,
            "a.py",
            "class Foo:\n    def bar(self):\n        return baz()\n\ndef baz():\n    return 1\n",
        )
        .unwrap();
        assert!(py
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::Class && n.name == "Foo"));
        assert!(py.nodes.iter().any(|n| n.name == "bar"));
        let js = parse_source(
            Lang::JavaScript,
            "a.js",
            "function greet() { return hi(); }\nfunction hi() { return 1; }\n",
        )
        .unwrap();
        assert!(js.nodes.iter().any(|n| n.name == "greet"));
        assert!(js.calls.iter().any(|(c, e)| c == "greet" && e == "hi"));
    }

    #[test]
    fn extracts_rust_trait_dispatch_facts() {
        let fp = parse_source(
            Lang::Rust,
            "src/animals.rs",
            "trait Animal {\n    fn speak(&self);\n}\n\n\
             struct Dog;\nimpl Animal for Dog {\n    fn speak(&self) {}\n}\n\n\
             struct Cat;\nimpl Animal for Cat {\n    fn speak(&self) {}\n}\n\n\
             fn make_noise(a: &dyn Animal) {\n    a.speak();\n}\n",
        )
        .unwrap();
        // 两条 trait 方法事实（trait 声明 + impl 内同名方法不重复计入 trait 声明表）
        let tm: Vec<_> = fp
            .trait_methods
            .iter()
            .filter(|(t, m, _)| t == "Animal" && m == "speak")
            .collect();
        assert_eq!(tm.len(), 1, "trait 声明的方法应恰好一条");
        // impl Animal for Dog/Cat 两条事实
        let im: Vec<_> = fp
            .impl_trait_methods
            .iter()
            .filter(|(t, _, m, _)| t == "Animal" && m == "speak")
            .collect();
        assert_eq!(im.len(), 2, "两个 impl 的方法应各记一条");
        let types: Vec<_> = im.iter().map(|(_, ty, _, _)| ty.as_str()).collect();
        assert!(types.contains(&"Dog") && types.contains(&"Cat"));
        // 普通 impl（无 trait）不产生 impl_trait_methods
        let fp2 = parse_source(
            Lang::Rust,
            "src/plain.rs",
            "struct S;\nimpl S {\n    fn run(&self) {}\n}\n",
        )
        .unwrap();
        assert!(fp2.impl_trait_methods.is_empty());
    }

    #[test]
    fn records_dyn_call_site_as_regular_call() {
        let fp = parse_source(
            Lang::Rust,
            "src/call.rs",
            "trait T { fn go(&self); }\nfn driver(x: &dyn T) { x.go(); }\n",
        )
        .unwrap();
        assert!(fp.calls.iter().any(|(c, m)| c == "driver" && m == "go"));
    }

    #[test]
    fn extracts_structured_import_links_per_language() {
        // Rust：use 路径段 → Symbol 链接
        let rust =
            parse_source(Lang::Rust, "src/a.rs", "use std::collections::HashMap;\n").unwrap();
        let targets: Vec<&str> = rust
            .import_links
            .iter()
            .map(|l| l.target.as_str())
            .collect();
        assert!(targets.contains(&"HashMap"), "{targets:?}");
        assert!(targets.contains(&"std"), "{targets:?}");
        assert!(
            rust.import_links
                .iter()
                .all(|l| l.kind == ImportKind::Symbol),
            "Rust use 全部记 Symbol"
        );

        // Python：from a.b import c → Symbol
        let py = parse_source(Lang::Python, "src/m.py", "from pkg import Thing\n").unwrap();
        let py_targets: Vec<&str> = py.import_links.iter().map(|l| l.target.as_str()).collect();
        assert!(py_targets.contains(&"Thing"), "{py_targets:?}");

        // JS：相对路径 → File；裸包名 → External
        let js = parse_source(
            Lang::JavaScript,
            "src/main.js",
            "import x from './util.js';\nimport y from 'react';\n",
        )
        .unwrap();
        assert!(js.import_links.contains(&ImportLink {
            kind: ImportKind::File,
            target: "./util.js".into()
        }));
        assert!(js.import_links.contains(&ImportLink {
            kind: ImportKind::External,
            target: "react".into()
        }));

        // require() 也产生依赖事实
        let req = parse_source(
            Lang::JavaScript,
            "src/r.js",
            "const x = require('./mod');\n",
        )
        .unwrap();
        assert!(req.import_links.contains(&ImportLink {
            kind: ImportKind::File,
            target: "./mod".into()
        }));
    }

    #[test]
    fn collects_definition_references_without_self_or_callees() {
        let fp = parse_source(
            Lang::Rust,
            "src/refs.rs",
            "struct Foo {}\n\
             pub fn use_foo() -> Foo { Foo {} }\n\
             pub fn recur(n: u32) -> u32 {\n    if n == 0 { 0 } else { recur(n - 1) }\n}\n",
        )
        .unwrap();
        assert!(
            fp.refs.iter().any(|(f, r)| f == "use_foo" && r == "Foo"),
            "use_foo 应引用 Foo：{:?}",
            fp.refs
        );
        assert!(
            !fp.refs
                .iter()
                .any(|(f, r)| f == "use_foo" && r == "use_foo"),
            "自身名不进引用"
        );
        assert!(
            !fp.refs.iter().any(|(f, r)| f == "recur" && r == "recur"),
            "递归调用（callee）不进引用"
        );
        // 上限防膨胀
        let big = parse_source(
            Lang::Rust,
            "src/big.rs",
            &format!(
                "pub fn huge() {{\n{}\n}}\n",
                (0..200)
                    .map(|i| format!("    let v{i} = {i};"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        )
        .unwrap();
        let huge_refs = big.refs.iter().filter(|(f, _)| f == "huge").count();
        assert!(
            huge_refs <= MAX_REFS_PER_DEF,
            "引用采集必须受限：{huge_refs}"
        );
    }
}

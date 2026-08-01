//! tree-sitter 四语言解析：实体提取 + 名称级 calls/imports。

use crate::model::{node_id, GraphError, Node, NodeKind};
use tree_sitter::{Language, Node as TsNode, Parser};

/// 支持的源语言。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Rust,
    Python,
    JavaScript,
    TypeScript,
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
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Lang::Rust => "rust",
            Lang::Python => "python",
            Lang::JavaScript => "javascript",
            Lang::TypeScript => "typescript",
        }
    }

    fn language(&self) -> Language {
        match self {
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        }
    }
}

/// 单文件解析结果：实体节点 + 名称级调用对 + import 原文本。
pub struct FileParse {
    pub nodes: Vec<Node>,
    pub calls: Vec<(String, String)>,
    pub imports: Vec<String>,
    /// Rust：trait 声明的方法 (trait 名, 方法名, 起始行)，用于构建动态分发桥。
    pub trait_methods: Vec<(String, String, u32)>,
    /// Rust：`impl Trait for Type` 内的方法 (trait 名, 实现类型名, 方法名, 起始行)。
    pub impl_trait_methods: Vec<(String, String, String, u32)>,
}

fn parse_err(path: &str, lang: Lang) -> GraphError {
    GraphError::Parse {
        path: path.into(),
        lang: lang.as_str(),
    }
}

/// 判定定义实体的 NodeKind；非实体返回 None。
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
    }
}

fn is_import(lang: Lang, kind: &str) -> bool {
    match lang {
        Lang::Rust => kind == "use_declaration",
        Lang::Python => kind == "import_statement" || kind == "import_from_statement",
        Lang::JavaScript | Lang::TypeScript => kind == "import_statement",
    }
}

fn is_call(lang: Lang, kind: &str) -> bool {
    match lang {
        Lang::Python => kind == "call",
        _ => kind == "call_expression",
    }
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

/// 取被调用名：identifier 直接用；scoped/member/attribute 等取末段。
fn callee_name(call: TsNode, src: &str) -> Option<String> {
    let func = call.child_by_field_name("function")?;
    let target = match func.kind() {
        "identifier" => func,
        "scoped_identifier" => func.child_by_field_name("name")?,
        "field_expression" => func.child_by_field_name("field")?,
        "member_expression" => func.child_by_field_name("property")?,
        "attribute" => func.child_by_field_name("attribute")?,
        _ => return None,
    };
    target.utf8_text(src.as_bytes()).ok().map(str::to_string)
}

enum Step<'t> {
    Enter(TsNode<'t>),
    Exit {
        pop_def: bool,
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
    let mut trait_methods: Vec<(String, String, u32)> = Vec::new();
    let mut impl_trait_methods: Vec<(String, String, String, u32)> = Vec::new();
    // 最近的命名定义栈（caller 归属）与祖先 kind 栈（Method 判定）。
    let mut def_stack: Vec<String> = Vec::new();
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
                if pop_def {
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
                        imports.push(text.trim().to_string());
                    }
                }
                if is_call(lang, kind) {
                    if let (Some(caller), Some(callee)) = (def_stack.last(), callee_name(node, src))
                    {
                        calls.push((caller.clone(), callee));
                    }
                }
                let mut pushed_def = false;
                if let Some(nk) = entity_kind(lang, kind, &ancestor_kinds) {
                    if let Some(name) = entity_name(node, src) {
                        let start_line = node.start_position().row as u32 + 1;
                        nodes.push(Node {
                            id: node_id(path, &name, start_line),
                            kind: nk,
                            name: name.clone(),
                            path: path.to_string(),
                            start_line,
                            end_line: node.end_position().row as u32 + 1,
                            signature: extract_signature(lang, node, src),
                            doc: extract_doc(node, src),
                            score: 0.0,
                        });
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
                        pushed_def = true;
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
        trait_methods,
        impl_trait_methods,
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
        assert_eq!(Lang::from_path("a.md"), None);
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
}

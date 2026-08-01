# Graph Engineering 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 新建 `deepseeknova-graph` crate（tree-sitter 四语言 → SQLite 异构代码图 + FTS5 BM25 + 个性化 PageRank），为 agent 提供 search_code / traverse_graph / retrieve_entity 三个图检索工具与 token 预算内的自动 repo map，替代 grep+整文件读取的高耗 token 检索。

**Architecture:** 索引器把源码解析为节点（Directory/File/Struct/Enum/Trait/Class/Function/Method）与边（Contains/Imports/Calls/Implements/References），持久化到 `.deepseeknova/graph.db`（mtime+hash 增量刷新）；PageRank 分数写回节点用于排序；tools crate 加三个薄工具经 `ToolContext.extensions` 取用索引；runtime `build_agent` 负责装配（后台构建、系统提示注入 repo map 与检索策略）。

**Tech Stack:** Rust, tree-sitter (+rust/python/javascript/typescript grammars), rusqlite(bundled FTS5), tokio, thiserror。PageRank 幂迭代自实现。

**Spec:** `docs/superpowers/specs/2026-07-26-graph-engineering-design.md`

**验收惯例（每个 Task 末尾执行）:** `cargo test -p deepseeknova-graph`（或相应 crate）；整计划收尾 `make check`。注意 nvm 的 node 需完整路径（前端不涉及本计划）。

---

## 文件结构

```
crates/deepseeknova-graph/
├── Cargo.toml
├── README.md
└── src/
    ├── lib.rs       GraphIndex 门面（open/refresh/search/neighbors/skeleton/repo_map）
    ├── model.rs     NodeKind/EdgeKind/Node/EdgeRec/GraphError/node_id
    ├── parser.rs    tree-sitter 解析：LangSpec + 每语言 query + FileParse 输出
    ├── store.rs     SQLite schema、增量刷新、FTS5 检索、边解析
    ├── rank.rs      个性化 PageRank，分数写回 nodes.score
    └── repomap.rs   token 预算骨架地图
crates/deepseeknova-tools/src/graph_tools.rs   三工具
crates/deepseeknova-config/src/lib.rs          [graph] 配置节
crates/deepseeknova-agent/src/agent.rs         Agent.extensions 通用注入
crates/deepseeknova-runtime/src/lib.rs         build_agent 装配
crates/deepseeknova-context/src/lib.rs         PromptBuilder repo_map 参数
```

依赖顺序：Task 1→2→3→4→5→6（graph crate 自足）；Task 7（config）独立；Task 8（agent 注入点）→9（tools）→10（runtime 装配）→11（context repo map 注入）→12（收尾回归）。

---

### Task 1: crate 脚手架与依赖

**Files:**
- Create: `crates/deepseeknova-graph/Cargo.toml`, `crates/deepseeknova-graph/src/lib.rs`, `crates/deepseeknova-graph/README.md`
- Modify: `Cargo.toml`（workspace members / default-members / workspace.dependencies 三处）

- [ ] **Step 1: 建 Cargo.toml**

```toml
[package]
name = "deepseeknova-graph"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true
description = "Code graph engine: tree-sitter parsing, SQLite-persisted heterogeneous graph, BM25 + personalized PageRank retrieval, token-budgeted repo maps."
readme = "README.md"

[dependencies]
anyhow = { workspace = true }
thiserror = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
sha2 = { workspace = true }
hex = { workspace = true }
tracing = { workspace = true }
rusqlite = { version = "0.37", features = ["bundled"] }
tree-sitter = "0.24"
tree-sitter-rust = "0.23"
tree-sitter-python = "0.23"
tree-sitter-javascript = "0.23"
tree-sitter-typescript = "0.23"

[dev-dependencies]
tempfile = { workspace = true }
```

- [ ] **Step 2: 最小 lib.rs**

```rust
//! # deepseeknova-graph
//!
//! 代码图引擎：tree-sitter 解析 → SQLite 异构图（FTS5 BM25）→
//! 个性化 PageRank 排序 → 图检索 API 与 token 预算 repo map。
```

- [ ] **Step 3: 根 Cargo.toml 三处登记**

`[workspace] members` 与 `default-members` 各加 `"crates/deepseeknova-graph",`；`[workspace.dependencies]` 加 `deepseeknova-graph = { version = "0.4.0", path = "crates/deepseeknova-graph" }`。

- [ ] **Step 4: README.md**

```markdown
# deepseeknova-graph

Code graph engine for token-efficient retrieval: tree-sitter parsing (Rust/Python/JS/TS),
SQLite-persisted heterogeneous graph with FTS5 BM25 and personalized PageRank, powering
graph search tools and token-budgeted repo maps.
See `docs/superpowers/specs/2026-07-26-graph-engineering-design.md`.
```

- [ ] **Step 5: 验证编译**

Run: `cargo build -p deepseeknova-graph`
Expected: `Finished` 无 error。若 grammar 版本求解失败，按 cargo 报错把对应 `tree-sitter-*` minor 版本对齐到与 `tree-sitter = 0.24` 兼容的最新 0.23.x 后重跑。

- [ ] **Step 6: Commit**

```bash
git add crates/deepseeknova-graph Cargo.toml Cargo.lock
git commit -m "feat(graph): scaffold deepseeknova-graph crate with tree-sitter deps"
```

---

### Task 2: model.rs — 图模型与错误类型

**Files:**
- Create: `crates/deepseeknova-graph/src/model.rs`
- Modify: `crates/deepseeknova-graph/src/lib.rs`（加 `pub mod model; pub use model::*;`）

- [ ] **Step 1: 写失败测试（model.rs 尾部 `#[cfg(test)]`）**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_id_is_stable_and_readable() {
        assert_eq!(node_id("src/a.rs", "foo", 10), "src/a.rs#foo#10");
    }

    #[test]
    fn kind_roundtrip() {
        for k in [NodeKind::Directory, NodeKind::File, NodeKind::Struct, NodeKind::Enum,
                  NodeKind::Trait, NodeKind::Class, NodeKind::Function, NodeKind::Method] {
            assert_eq!(NodeKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(NodeKind::parse("nope"), None);
        for e in [EdgeKind::Contains, EdgeKind::Imports, EdgeKind::Calls,
                  EdgeKind::Implements, EdgeKind::References] {
            assert_eq!(EdgeKind::parse(e.as_str()), Some(e));
        }
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p deepseeknova-graph`
Expected: FAIL（类型未定义，编译错误即视为失败）

- [ ] **Step 3: 实现模型（model.rs 顶部）**

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind { Directory, File, Struct, Enum, Trait, Class, Function, Method }

impl NodeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Directory => "directory", Self::File => "file",
            Self::Struct => "struct", Self::Enum => "enum", Self::Trait => "trait",
            Self::Class => "class", Self::Function => "function", Self::Method => "method",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "directory" => Self::Directory, "file" => Self::File,
            "struct" => Self::Struct, "enum" => Self::Enum, "trait" => Self::Trait,
            "class" => Self::Class, "function" => Self::Function, "method" => Self::Method,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind { Contains, Imports, Calls, Implements, References }

impl EdgeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Contains => "contains", Self::Imports => "imports",
            Self::Calls => "calls", Self::Implements => "implements",
            Self::References => "references",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "contains" => Self::Contains, "imports" => Self::Imports,
            "calls" => Self::Calls, "implements" => Self::Implements,
            "references" => Self::References,
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
pub struct EdgeRec { pub src: String, pub dst: String, pub kind: EdgeKind }

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
```

- [ ] **Step 4: lib.rs 挂模块 → 跑测试通过**

Run: `cargo test -p deepseeknova-graph`
Expected: `2 passed`

- [ ] **Step 5: Commit**

```bash
git add crates/deepseeknova-graph
git commit -m "feat(graph): node/edge model, stable ids, GraphError"
```

---

### Task 3: parser.rs — tree-sitter 解析（四语言实体 + 名称级 calls/imports）

**Files:**
- Create: `crates/deepseeknova-graph/src/parser.rs`
- Modify: `crates/deepseeknova-graph/src/lib.rs`（加 `pub mod parser;`）

设计要点：每语言一个 `LangSpec`（`tree_sitter::Language` + def-query + call-query + import-query）。解析产出 `FileParse { nodes, calls, imports }`，其中 `calls`/`imports` 是**待解析的名称**（字符串），真实边在 store 层用「名称→定义节点」映射连接（名称级近似，spec §3.1）。

- [ ] **Step 1: 写失败测试（parser.rs 尾部）**

```rust
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
        assert!(fp.calls.iter().any(|(caller, callee)| caller == "make" && callee == "helper"));
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
        let py = parse_source(Lang::Python, "a.py",
            "class Foo:\n    def bar(self):\n        return baz()\n\ndef baz():\n    return 1\n").unwrap();
        assert!(py.nodes.iter().any(|n| n.kind == NodeKind::Class && n.name == "Foo"));
        assert!(py.nodes.iter().any(|n| n.name == "bar"));
        let js = parse_source(Lang::JavaScript, "a.js",
            "function greet() { return hi(); }\nfunction hi() { return 1; }\n").unwrap();
        assert!(js.nodes.iter().any(|n| n.name == "greet"));
        assert!(js.calls.iter().any(|(c, e)| c == "greet" && e == "hi"));
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p deepseeknova-graph parser`
Expected: FAIL（`parse_source`/`Lang` 未定义）

- [ ] **Step 3: 实现 parser.rs**

关键实现说明（写代码时遵循）：
- `Lang` 枚举 + `from_path` 按扩展名；`.tsx/.ts` → TypeScript，`.jsx/.js/.mjs/.cjs` → JavaScript。
- `language()` 返回对应 `tree_sitter::Language`（tree-sitter-typescript 用 `LANGUAGE_TYPESCRIPT`）。
- 用 `tree_sitter::Query` 抓定义：Rust 抓 `struct_item/enum_item/trait_item/function_item`；Python 抓 `class_definition/function_definition`；JS/TS 抓 `function_declaration/class_declaration/method_definition`。方法（在 impl/class 内的函数）标 `NodeKind::Method`，顶层函数标 `Function`。
- 每个定义节点：`name` 取 name 子节点文本；`start_line/end_line` = node 行 +1（1-based）；`signature` = 定义首行截到 `{`/`:` 前并 `trim`；`doc` = 定义前紧邻的 `///`/`#`/`//` 注释首行去标记，无则空串。
- calls：遍历 `call_expression`（Rust/JS/TS）/`call`（Python），取被调用标识符名，归属到包含它的最近命名定义（caller name）；产出 `(caller_name, callee_name)`。
- imports：Rust `use_declaration`、Python `import_statement`/`import_from_statement`、JS/TS `import_statement` 的原文本 trim。
- 解析失败（`Parser::set_language` 或 `parse` 返回 None）→ `Err(GraphError::Parse{..})`。

```rust
use crate::model::{node_id, GraphError, Node, NodeKind};
use tree_sitter::{Node as TsNode, Parser, Query, QueryCursor};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang { Rust, Python, JavaScript, TypeScript }

impl Lang {
    pub fn from_path(path: &str) -> Option<Self> {
        let ext = path.rsplit('.').next()?;
        Some(match ext {
            "rs" => Self::Rust,
            "py" => Self::Python,
            "ts" | "tsx" => Self::TypeScript,
            "js" | "jsx" | "mjs" | "cjs" => Self::JavaScript,
            _ => return None,
        })
    }
    pub fn as_str(&self) -> &'static str {
        match self { Self::Rust=>"rust", Self::Python=>"python",
                     Self::JavaScript=>"javascript", Self::TypeScript=>"typescript" }
    }
    fn language(&self) -> tree_sitter::Language {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        }
    }
}

/// 单文件解析结果。calls/imports 是待连接的名称，边在 store 层解析。
pub struct FileParse {
    pub nodes: Vec<Node>,
    pub calls: Vec<(String, String)>, // (caller_name, callee_name)
    pub imports: Vec<String>,
}

pub fn parse_source(lang: Lang, path: &str, src: &str) -> Result<FileParse, GraphError> {
    let mut parser = Parser::new();
    parser.set_language(&lang.language())
        .map_err(|_| GraphError::Parse { path: path.into(), lang: lang.as_str() })?;
    let tree = parser.parse(src, None)
        .ok_or(GraphError::Parse { path: path.into(), lang: lang.as_str() })?;
    // 实现：递归遍历 tree.root_node()，按 node.kind() 分派：
    //   - 命名定义 → 构造 Node（含 signature/doc/行区间），入 nodes
    //   - call → 记录 (当前包含定义名, 被调名) 入 calls
    //   - import → 记录原文本入 imports
    // 具体节点 kind 名见各 grammar；signature 取 src[def.start_byte..行末或'{'前]。
    // 省略处按上方"关键实现说明"填充；返回 FileParse。
    todo!("按关键实现说明填充遍历逻辑")
}
```

> 执行者注意：`todo!` 必须在本 Step 内用「关键实现说明」列出的规则完整实现，不得留 `todo!` 提交。遍历建议用显式栈 DFS，`doc` 用 `node.prev_sibling()` 找注释。

- [ ] **Step 4: 跑测试通过**

Run: `cargo test -p deepseeknova-graph parser`
Expected: 4 passed（parses_rust_entities / extracts_rust_calls_and_imports / lang_from_extension / parses_python_and_js）

- [ ] **Step 5: Commit**

```bash
git add crates/deepseeknova-graph
git commit -m "feat(graph): tree-sitter parser for rust/python/js/ts entities, calls, imports"
```

---

### Task 4: store.rs — SQLite 持久化、增量刷新、FTS5 检索

**Files:**
- Create: `crates/deepseeknova-graph/src/store.rs`
- Modify: `crates/deepseeknova-graph/src/lib.rs`（加 `pub mod store;`）

- [ ] **Step 1: 写失败测试（store.rs 尾部）**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EdgeKind, NodeKind};
    use tempfile::tempdir;

    fn write(root: &std::path::Path, rel: &str, body: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn full_then_incremental_refresh() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(root, "src/a.rs", "pub fn alpha() { beta(); }\npub fn beta() {}\n");
        write(root, "src/b.rs", "pub fn gamma() {}\n");
        let mut store = Store::open(&root.join(".deepseeknova/graph.db")).unwrap();

        let n1 = store.refresh(root, 1_048_576).unwrap();
        assert!(n1.files_indexed >= 2);
        let beta_id = store.find_by_name("beta").unwrap()[0].id.clone();

        // calls 边：alpha → beta
        let callers = store.neighbors(&beta_id, &[EdgeKind::Calls], Direction::Callers, 1).unwrap();
        assert!(callers.iter().any(|n| n.name == "alpha"));

        // 增量：只改 b.rs，beta 节点 id 不变
        std::thread::sleep(std::time::Duration::from_millis(10));
        write(root, "src/b.rs", "pub fn gamma() {}\npub fn delta() {}\n");
        let n2 = store.refresh(root, 1_048_576).unwrap();
        assert_eq!(n2.files_reparsed, 1);
        assert_eq!(store.find_by_name("beta").unwrap()[0].id, beta_id);
        assert!(!store.find_by_name("delta").unwrap().is_empty());
    }

    #[test]
    fn fts_search_ranks_name_match() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        write(root, "src/a.rs", "/// permission gate\npub struct PermissionGate {}\npub fn unrelated() {}\n");
        let mut store = Store::open(&root.join(".deepseeknova/graph.db")).unwrap();
        store.refresh(root, 1_048_576).unwrap();
        let hits = store.search("PermissionGate", None, 10).unwrap();
        assert_eq!(hits[0].name, "PermissionGate");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p deepseeknova-graph store`
Expected: FAIL（`Store`/`Direction` 未定义）

- [ ] **Step 3: 实现 store.rs**

关键实现说明：
- schema（`open` 时 `CREATE TABLE IF NOT EXISTS`）：
  ```sql
  files(path TEXT PRIMARY KEY, mtime INTEGER, hash TEXT);
  nodes(id TEXT PRIMARY KEY, kind TEXT, name TEXT, path TEXT, start_line INTEGER,
        end_line INTEGER, signature TEXT, doc TEXT, score REAL DEFAULT 0);
  edges(src TEXT, dst TEXT, kind TEXT);
  CREATE INDEX IF NOT EXISTS idx_nodes_name ON nodes(name);
  CREATE INDEX IF NOT EXISTS idx_edges_src ON edges(src);
  CREATE INDEX IF NOT EXISTS idx_edges_dst ON edges(dst);
  CREATE VIRTUAL TABLE IF NOT EXISTS symbol_fts USING fts5(
    name, signature, doc, id UNINDEXED, path UNINDEXED, tokenize='porter unicode61');
  ```
- `Direction { Callers, Callees, Both }`；`RefreshReport { files_indexed, files_reparsed, nodes, edges }`。
- `refresh(root, max_file_size)`：
  1. 递归遍历 root（复用 .gitignore：读 root/.gitignore 行做前缀/后缀匹配；硬排除 `target/ node_modules/ .git/ dist/`）。
  2. 对每个 `Lang::from_path` 命中的文件：`fs::metadata` 取 mtime，超 `max_file_size` 跳过；查 `files` 表，mtime 相同则跳过；否则读内容算 sha256，与库中 hash 比，相同则更 mtime 跳过，不同则「删除该 path 的 nodes+edges+fts+file 行 → parse_source → 插入」。单文件 parse Err 时 `tracing::warn!` 跳过、计入 files_indexed 但不算 reparsed。
  3. 每文件额外插一个 `NodeKind::File` 节点（id=`path#<file>#0`）并对其定义子节点建 `Contains` 边。
  4. 全量重建阶段结束后**解析名称边**：建 `name → Vec<node_id>` 映射；对每个文件的 `calls`：caller_name 定位到该文件内同名定义节点、callee_name 定位到全库同名定义节点（取第一个），插 `Calls` 边；`imports` 文本里若含某定义名则插 `Imports` 边（近似）。删文件时其出边一并删。
  5. 返回 RefreshReport。
- `find_by_name(name) -> Vec<Node>`：`SELECT ... WHERE name=?`。
- `search(query, kind: Option<NodeKind>, limit)`：FTS5 `MATCH`（query 分词 OR，转义引号，同 memory/store.rs 模式），`bm25(symbol_fts)` 排序；名称精确匹配的结果分数加权（把 name==query 的排到最前）；kind 过滤在 SQL `WHERE kind=?`。返回 `Vec<Node>`。
- `neighbors(id, edge_kinds, direction, hops)`：BFS，Callers 查 `edges.dst=?`、Callees 查 `edges.src=?`，Both 双向；限定 edge_kinds；去重；返回按 score 降序 `Vec<Node>`。
- `get(id) -> Option<Node>`、`children(id) -> Vec<Node>`（Contains 出边）、`all_edges() -> Vec<EdgeRec>`、`all_nodes() -> Vec<Node>`、`set_scores(&[(id, score)])`（供 rank 写回）。

（完整 SQL 与遍历代码按上述说明实现；不得留 `todo!` 提交。）

- [ ] **Step 4: 跑测试通过**

Run: `cargo test -p deepseeknova-graph store`
Expected: 2 passed

- [ ] **Step 5: Commit**

```bash
git add crates/deepseeknova-graph
git commit -m "feat(graph): SQLite store with incremental refresh, FTS5 search, graph neighbors"
```

---

### Task 5: rank.rs — 个性化 PageRank

**Files:**
- Create: `crates/deepseeknova-graph/src/rank.rs`
- Modify: `crates/deepseeknova-graph/src/lib.rs`（加 `pub mod rank;`）

- [ ] **Step 1: 写失败测试（rank.rs 尾部）**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_sum_to_one_and_hub_wins() {
        // a→c, b→c：c 是 hub，应得分最高
        let edges = vec![("a".to_string(),"c".to_string()), ("b".to_string(),"c".to_string())];
        let nodes = vec!["a".to_string(),"b".to_string(),"c".to_string()];
        let scores = pagerank(&nodes, &edges, &[], 0.85, 50);
        let sum: f64 = scores.values().sum();
        assert!((sum - 1.0).abs() < 1e-6, "sum={sum}");
        assert!(scores["c"] > scores["a"]);
        assert!(scores["c"] > scores["b"]);
    }

    #[test]
    fn personalization_boosts_seed() {
        let edges = vec![("a".to_string(),"b".to_string())];
        let nodes = vec!["a".to_string(),"b".to_string()];
        let base = pagerank(&nodes, &edges, &[], 0.85, 50);
        let seeded = pagerank(&nodes, &edges, &["a".to_string()], 0.85, 50);
        assert!(seeded["a"] > base["a"], "seed should raise its own score");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p deepseeknova-graph rank`
Expected: FAIL（`pagerank` 未定义）

- [ ] **Step 3: 实现 rank.rs**

```rust
use std::collections::HashMap;

/// 个性化 PageRank（幂迭代）。
///
/// - `nodes`：全部节点 id。
/// - `edges`：有向边 (src, dst)。
/// - `personalization`：种子 id 列表；非空时 teleport 只落到种子（个性化），
///   空时均匀 teleport（标准 PageRank）。
/// - `damping`：阻尼系数（典型 0.85）。
/// - `iters`：迭代次数（50 足够收敛）。
///
/// 返回 id → score，所有分数之和为 1。
pub fn pagerank(
    nodes: &[String],
    edges: &[(String, String)],
    personalization: &[String],
    damping: f64,
    iters: usize,
) -> HashMap<String, f64> {
    let n = nodes.len();
    if n == 0 { return HashMap::new(); }
    let idx: HashMap<&str, usize> = nodes.iter().enumerate().map(|(i, s)| (s.as_str(), i)).collect();

    // 出边邻接 + 出度
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (s, d) in edges {
        if let (Some(&si), Some(&di)) = (idx.get(s.as_str()), idx.get(d.as_str())) {
            out[si].push(di);
        }
    }

    // teleport 分布
    let mut tele = vec![0.0; n];
    let seeds: Vec<usize> = personalization.iter().filter_map(|s| idx.get(s.as_str()).copied()).collect();
    if seeds.is_empty() {
        for t in tele.iter_mut() { *t = 1.0 / n as f64; }
    } else {
        for &s in &seeds { tele[s] = 1.0 / seeds.len() as f64; }
    }

    let mut rank = vec![1.0 / n as f64; n];
    for _ in 0..iters {
        let mut next = vec![0.0; n];
        let mut dangling = 0.0;
        for i in 0..n {
            if out[i].is_empty() {
                dangling += rank[i];
            } else {
                let share = rank[i] / out[i].len() as f64;
                for &j in &out[i] { next[j] += share; }
            }
        }
        for i in 0..n {
            next[i] = (1.0 - damping) * tele[i]
                    + damping * (next[i] + dangling * tele[i]);
        }
        rank = next;
    }
    nodes.iter().cloned().zip(rank).collect()
}
```

- [ ] **Step 4: 跑测试通过**

Run: `cargo test -p deepseeknova-graph rank`
Expected: 2 passed

- [ ] **Step 5: Commit**

```bash
git add crates/deepseeknova-graph
git commit -m "feat(graph): personalized PageRank power-iteration"
```

---

### Task 6: repomap.rs + lib.rs 门面 GraphIndex

**Files:**
- Create: `crates/deepseeknova-graph/src/repomap.rs`
- Modify: `crates/deepseeknova-graph/src/lib.rs`（`pub mod repomap;` + `GraphIndex` 门面）

- [ ] **Step 1: 写失败测试（repomap.rs 尾部）**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Node, NodeKind};

    fn node(name: &str, path: &str, sig: &str, score: f64) -> Node {
        Node { id: format!("{path}#{name}#1"), kind: NodeKind::Function, name: name.into(),
               path: path.into(), start_line: 1, end_line: 2, signature: sig.into(),
               doc: String::new(), score }
    }

    #[test]
    fn respects_token_budget_and_orders_by_score() {
        let nodes = vec![
            node("high", "a.rs", "pub fn high()", 0.9),
            node("mid",  "a.rs", "pub fn mid()",  0.5),
            node("low",  "b.rs", "pub fn low()",  0.1),
        ];
        let map = render_repo_map(&nodes, 40); // ~160 chars 预算
        assert!(map.contains("a.rs:"));
        assert!(map.contains("pub fn high()"));
        // high 在 mid 前
        assert!(map.find("high").unwrap() < map.find("mid").unwrap());
        // token 预算硬上限（chars/4 ≤ 40*1.1 容差）
        assert!(map.chars().count() <= 40 * 4 + 40);
    }

    #[test]
    fn empty_nodes_yield_empty_map() {
        assert_eq!(render_repo_map(&[], 100), "");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p deepseeknova-graph repomap`
Expected: FAIL（`render_repo_map` 未定义）

- [ ] **Step 3: 实现 repomap.rs**

```rust
use crate::model::Node;

/// 估算 token 数（沿用项目惯例 chars/4，与 context/history.rs 一致）。
fn est_tokens(s: &str) -> usize { s.chars().count() / 4 }

/// 在 token 预算内渲染骨架 repo map。
///
/// 入参 `nodes` 应已按 score 降序（GraphIndex::repo_map 保证）。
/// 按文件分组输出，形如：
/// ```text
/// crates/x/src/a.rs:
/// │ pub fn high()
/// ⋮
/// ```
pub fn render_repo_map(nodes: &[Node], token_budget: usize) -> String {
    if nodes.is_empty() || token_budget == 0 { return String::new(); }
    // 按 score 降序贪心装入预算；记录每个文件选中的签名（保持分数序）。
    let mut ranked: Vec<&Node> = nodes.iter().collect();
    ranked.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

    use std::collections::BTreeMap;
    let mut per_file: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut order: Vec<&str> = Vec::new(); // 文件首次出现顺序（= 该文件最高分符号的排名）
    let mut used = 0usize;

    for n in ranked {
        let line_cost = est_tokens(&n.signature) + 1;
        if used + line_cost > token_budget { continue; }
        if !per_file.contains_key(n.path.as_str()) { order.push(n.path.as_str()); }
        per_file.entry(n.path.as_str()).or_default().push(&n.signature);
        used += line_cost;
    }

    let mut out = String::new();
    for path in order {
        out.push_str(path);
        out.push_str(":\n");
        for sig in &per_file[path] {
            out.push_str("│ ");
            out.push_str(sig);
            out.push('\n');
        }
        out.push_str("⋮\n");
    }
    out.trim_end().to_string()
}
```

- [ ] **Step 4: 实现 GraphIndex 门面（lib.rs）**

```rust
pub mod model;
pub mod parser;
pub mod rank;
pub mod repomap;
pub mod store;

pub use model::{EdgeKind, GraphError, Node, NodeKind};
pub use store::Direction;

use std::path::{Path, PathBuf};

/// 代码图索引门面。线程安全由内部 Store 的连接串行化保证；
/// 通常包一层 `Arc<Mutex<GraphIndex>>` 供多处共享（见 runtime 装配）。
pub struct GraphIndex {
    store: store::Store,
    root: PathBuf,
    max_file_size: u64,
}

impl GraphIndex {
    /// 打开（或创建）workspace 的图索引。不触发解析——refresh 才解析。
    pub fn open(root: impl AsRef<Path>, max_file_size: u64) -> Result<Self, GraphError> {
        let root = root.as_ref().to_path_buf();
        let db = root.join(".deepseeknova").join("graph.db");
        let store = store::Store::open(&db)?;
        Ok(Self { store, root, max_file_size })
    }

    /// 增量刷新并重算 PageRank（分数写回 nodes.score）。
    pub fn refresh(&mut self) -> Result<store::RefreshReport, GraphError> {
        let report = self.store.refresh(&self.root, self.max_file_size)?;
        let nodes: Vec<String> = self.store.all_nodes()?.into_iter().map(|n| n.id).collect();
        let edges: Vec<(String, String)> =
            self.store.all_edges()?.into_iter().map(|e| (e.src, e.dst)).collect();
        let scores = rank::pagerank(&nodes, &edges, &[], 0.85, 50);
        self.store.set_scores(&scores.into_iter().collect::<Vec<_>>())?;
        Ok(report)
    }

    pub fn search(&self, query: &str, kind: Option<NodeKind>, limit: usize)
        -> Result<Vec<Node>, GraphError> { self.store.search(query, kind, limit) }

    pub fn neighbors(&self, entity: &str, kinds: &[EdgeKind], dir: Direction, hops: usize)
        -> Result<Vec<Node>, GraphError> {
        let id = self.resolve(entity)?;
        self.store.neighbors(&id, kinds, dir, hops)
    }

    /// 骨架视图：签名 + doc + 直接子实体签名。
    pub fn skeleton(&self, entity: &str) -> Result<String, GraphError> {
        let id = self.resolve(entity)?;
        let node = self.store.get(&id)?.ok_or_else(|| GraphError::EntityNotFound(entity.into()))?;
        let mut out = String::new();
        if !node.doc.is_empty() { out.push_str(&format!("// {}\n", node.doc)); }
        out.push_str(&node.signature); out.push('\n');
        for child in self.store.children(&id)? {
            out.push_str(&format!("  {}\n", child.signature));
        }
        Ok(out)
    }

    /// 该实体的 (path, start_line, end_line)，供 retrieve_entity(full) 精确取码。
    pub fn location(&self, entity: &str) -> Result<(String, u32, u32), GraphError> {
        let id = self.resolve(entity)?;
        let n = self.store.get(&id)?.ok_or_else(|| GraphError::EntityNotFound(entity.into()))?;
        Ok((n.path, n.start_line, n.end_line))
    }

    /// token 预算内的 repo map；`personalization` 为种子符号名/路径。
    pub fn repo_map(&self, token_budget: usize, personalization: &[String])
        -> Result<String, GraphError> {
        if token_budget == 0 { return Ok(String::new()); }
        let mut nodes = self.store.all_nodes()?;
        if !personalization.is_empty() {
            let ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
            let edges: Vec<(String, String)> =
                self.store.all_edges()?.into_iter().map(|e| (e.src, e.dst)).collect();
            // 种子名 → 命中节点 id
            let seeds: Vec<String> = nodes.iter()
                .filter(|n| personalization.iter().any(|p| &n.name == p || n.path.contains(p.as_str())))
                .map(|n| n.id.clone()).collect();
            let scores = rank::pagerank(&ids, &edges, &seeds, 0.85, 50);
            for n in nodes.iter_mut() { n.score = *scores.get(&n.id).unwrap_or(&0.0); }
        }
        // 只保留有签名的定义节点（排除 File/Directory 空签名）
        nodes.retain(|n| !n.signature.is_empty());
        Ok(repomap::render_repo_map(&nodes, token_budget))
    }

    /// entity 支持 `id`（含 `#`）或 `path:name` 或裸 `name`。
    fn resolve(&self, entity: &str) -> Result<String, GraphError> {
        if entity.contains('#') { return Ok(entity.to_string()); }
        let (name, path_hint) = match entity.split_once(':') {
            Some((p, n)) => (n, Some(p)),
            None => (entity, None),
        };
        let hits = self.store.find_by_name(name)?;
        let pick = match path_hint {
            Some(p) => hits.into_iter().find(|n| n.path.contains(p)),
            None => hits.into_iter().max_by(|a, b| a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal)),
        };
        pick.map(|n| n.id).ok_or_else(|| GraphError::EntityNotFound(entity.into()))
    }
}
```

- [ ] **Step 5: 跑全 crate 测试**

Run: `cargo test -p deepseeknova-graph`
Expected: 全部 passed（parser 4 + model 2 + store 2 + rank 2 + repomap 2）

- [ ] **Step 6: Commit**

```bash
git add crates/deepseeknova-graph
git commit -m "feat(graph): repo map renderer + GraphIndex facade (search/neighbors/skeleton/repo_map)"
```

---

### Task 7: config — `[graph]` 配置节

**Files:**
- Modify: `crates/deepseeknova-config/src/lib.rs`（新增 `GraphConfig` + `Config.graph` 字段）

- [ ] **Step 1: 写失败测试（config lib.rs 的 `#[cfg(test)] mod tests` 内新增）**

```rust
#[test]
fn graph_config_defaults() {
    let c = Config::default();
    assert!(c.graph.enabled);
    assert_eq!(c.graph.repo_map_tokens, 1024);
    assert_eq!(c.graph.max_file_size, 524_288);
}

#[test]
fn graph_config_parses_from_toml() {
    let toml = "[graph]\nenabled = false\nrepo_map_tokens = 0\n";
    let c: Config = toml::from_str(toml).unwrap();
    assert!(!c.graph.enabled);
    assert_eq!(c.graph.repo_map_tokens, 0);
    // 未写字段回落默认
    assert_eq!(c.graph.max_file_size, 524_288);
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p deepseeknova-config graph_config`
Expected: FAIL（`Config` 无 `graph` 字段）

- [ ] **Step 3: 实现（在 config lib.rs 加类型，并在 `Config` 结构体加字段）**

在 `Config` 结构体中加（紧跟 `pub tools: ToolsConfig,` 之后，保持风格）：

```rust
    /// 代码图索引配置。
    #[serde(default)]
    pub graph: GraphConfig,
```

新增类型（放在 ToolsConfig 定义附近）：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphConfig {
    /// 主开关。false 时不构建索引、不注入 repo map，行为等同现状。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// repo map 的 token 预算。0 = 不注入 map（仅保留检索工具）。
    #[serde(default = "default_repo_map_tokens")]
    pub repo_map_tokens: usize,
    /// 单文件解析大小上限（字节），超过跳过。
    #[serde(default = "default_graph_max_file_size")]
    pub max_file_size: u64,
}

fn default_repo_map_tokens() -> usize { 1024 }
fn default_graph_max_file_size() -> u64 { 524_288 }

impl Default for GraphConfig {
    fn default() -> Self {
        Self { enabled: true, repo_map_tokens: 1024, max_file_size: 524_288 }
    }
}
```

> 若 `default_true` 未存在于本 crate，则复用现有的；grep 确认 `fn default_true` 已定义（config 里 ProviderConfig 用过）。

- [ ] **Step 4: 跑测试通过**

Run: `cargo test -p deepseeknova-config graph_config`
Expected: 2 passed

- [ ] **Step 5: Commit**

```bash
git add crates/deepseeknova-config
git commit -m "feat(config): [graph] section (enabled/repo_map_tokens/max_file_size)"
```

---

### Task 8: agent — 通用 extension 注入 hook

**Files:**
- Modify: `crates/deepseeknova-agent/src/agent.rs`（加 `extensions: Vec<...>` 字段 + `with_extension` builder + 在 `ToolContext` 构造处注入）

现状：`agent.rs:632` 处构造 `ToolContext` 时只 `.with_extension(security.clone())`。需要让 runtime 能把任意扩展（如 `Arc<Mutex<GraphIndex>>`）也注入进每次工具执行的 context。

- [ ] **Step 1: 写失败测试（agent.rs 尾部 `#[cfg(test)]`；用一个读取扩展的假工具）**

```rust
#[tokio::test]
async fn injects_custom_extension_into_tool_context() {
    use deepseeknova_core::tool::{Tool, ToolContext};
    use deepseeknova_core::types::ToolSchema;

    #[derive(Clone)]
    struct Marker(u32);
    struct ProbeTool;
    #[async_trait::async_trait]
    impl Tool for ProbeTool {
        fn schema(&self) -> ToolSchema {
            ToolSchema { name: "probe".into(), description: "d".into(),
                parameters: serde_json::json!({"type":"object","properties":{}}) }
        }
        fn read_only(&self) -> bool { true }
        async fn execute(&self, ctx: &ToolContext, _args: &str) -> anyhow::Result<String> {
            let m = ctx.extensions.get::<Marker>().map(|m| m.0).unwrap_or(0);
            Ok(format!("marker={m}"))
        }
    }

    // 直接验证 build 出的 ToolContext 携带扩展：调用内部 make_tool_context 辅助。
    let agent = Agent::new(std::sync::Arc::new(crate::tests_support::NoopProvider), 3)
        .with_extension(Marker(42));
    let ctx = agent.make_tool_context("call-1", tokio_util::sync::CancellationToken::new());
    let out = ProbeTool.execute(&ctx, "{}").await.unwrap();
    assert_eq!(out, "marker=42");
}
```

> 说明：需要暴露一个 `pub(crate) fn make_tool_context(&self, call_id, cancel) -> ToolContext` 把 632 行处的构造逻辑抽出来（含 security + 所有 extensions），供 run_stream 与测试共用。`tests_support::NoopProvider` 若不存在，测试内就地定义一个最小 Provider stub（返回空流），与 agent.rs 现有测试风格一致。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p deepseeknova-agent injects_custom_extension`
Expected: FAIL（`with_extension`/`make_tool_context` 未定义）

- [ ] **Step 3: 实现**

- 结构体加字段：`extensions: Vec<Box<dyn Fn(&mut deepseeknova_core::tool::ExtensionRegistry) + Send + Sync>>,`（用闭包装扩展，规避类型擦除；`new` 里初始化为 `Vec::new()`）。
- builder：
  ```rust
  pub fn with_extension<T: std::any::Any + Send + Sync + Clone>(mut self, ext: T) -> Self {
      self.extensions.push(Box::new(move |reg| reg.insert(ext.clone())));
      self
  }
  ```
- 抽出 `make_tool_context`：
  ```rust
  pub(crate) fn make_tool_context(
      &self, call_id: &str, cancel: tokio_util::sync::CancellationToken,
  ) -> deepseeknova_core::tool::ToolContext {
      let mut ctx = deepseeknova_core::tool::ToolContext::with_cancellation(call_id, cancel)
          .with_workspace(self.workspace_root.clone());
      ctx.extensions.insert(self.security.clone());
      for apply in &self.extensions { apply(&mut ctx.extensions); }
      ctx
  }
  ```
- 632 行原构造替换为调用 `self.make_tool_context(&call.id, cancel.child_token())`（保留原有 plan_mode 等字段设置；若原来设置了 plan_mode，在 make_tool_context 里也带上或在返回后设置）。

- [ ] **Step 4: 跑测试通过 + 全 agent 测试不回归**

Run: `cargo test -p deepseeknova-agent`
Expected: 新测试 passed，其余不变

- [ ] **Step 5: Commit**

```bash
git add crates/deepseeknova-agent
git commit -m "feat(agent): generic extension injection into every ToolContext"
```

---

### Task 9: tools — search_code / traverse_graph / retrieve_entity

**Files:**
- Create: `crates/deepseeknova-tools/src/graph_tools.rs`
- Modify: `crates/deepseeknova-tools/src/lib.rs`（`pub mod graph_tools; pub use graph_tools::*;` + `all_builtin_tools_with_sandbox` 追加三工具）；`crates/deepseeknova-tools/Cargo.toml`（加 `deepseeknova-graph = { workspace = true }`）

共享句柄类型：`pub type GraphHandle = std::sync::Arc<std::sync::Mutex<deepseeknova_graph::GraphIndex>>;`。工具经 `ctx.extensions.get::<GraphHandle>()` 取索引；取不到 → 返回降级文字（不报错）。

- [ ] **Step 1: 写失败测试（graph_tools.rs 尾部；建临时仓库→建索引→注入 handle→三连调用）**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_core::tool::{Tool, ToolContext};
    use std::sync::{Arc, Mutex};
    use tempfile::tempdir;

    fn ctx_with_index(root: &std::path::Path) -> ToolContext {
        let mut idx = deepseeknova_graph::GraphIndex::open(root, 1_048_576).unwrap();
        idx.refresh().unwrap();
        let handle: GraphHandle = Arc::new(Mutex::new(idx));
        ToolContext::new("c1").with_workspace(root.to_path_buf()).with_extension(handle)
    }

    #[tokio::test]
    async fn search_then_traverse_then_retrieve() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"),
            "pub fn build_agent() { permission_gate_for(); }\npub fn permission_gate_for() {}\n").unwrap();
        // security ext 供 FileRead 能力校验
        let ctx = ctx_with_index(root)
            .with_extension(deepseeknova_security::context::SecurityContext::with_safe_defaults());

        let s = SearchCodeTool.execute(&ctx, r#"{"query":"build_agent"}"#).await.unwrap();
        assert!(s.contains("build_agent"));

        let t = TraverseGraphTool.execute(&ctx,
            r#"{"entity":"permission_gate_for","direction":"callers"}"#).await.unwrap();
        assert!(t.contains("build_agent"));

        let r = RetrieveEntityTool.execute(&ctx,
            r#"{"entity":"permission_gate_for","view":"full"}"#).await.unwrap();
        assert!(r.contains("pub fn permission_gate_for"));
    }

    #[tokio::test]
    async fn degrades_without_index() {
        let ctx = ToolContext::new("c2")
            .with_extension(deepseeknova_security::context::SecurityContext::with_safe_defaults());
        let out = SearchCodeTool.execute(&ctx, r#"{"query":"x"}"#).await.unwrap();
        assert!(out.contains("索引") || out.to_lowercase().contains("index"));
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p deepseeknova-tools graph_tools`
Expected: FAIL（工具类型未定义）

- [ ] **Step 3: 实现 graph_tools.rs**

三个工具均：`read_only()=true`；execute 开头 `enforce_capability(ctx, Capability::FileRead)?`（同 grep.rs）；`let handle = match ctx.extensions.get::<GraphHandle>() { Some(h)=>h, None=>return Ok("代码图索引构建中或未启用，请改用 grep 检索。".into()) };`。

- `SearchCodeTool`：args `{query, kind?, limit?}` → `handle.lock().search(...)` → 逐行 `format!("{i}. {kind} {name} — {path}:{start}-{end} · {sig}", ...)`，limit 默认 10，输出上限 ~50 行。
- `TraverseGraphTool`：args `{entity, direction, edge_kinds?, hops?}` → `neighbors(...)` → 按 spec 树状缩进输出，节点数上限 40，超出附 `…(+k more)`，单次输出裁到 ~8000 chars（≈2000 tokens）。direction 解析 callers/callees/both；edge_kinds 缺省 `[calls]`；hops 默认 2、`.min(3)`。
- `RetrieveEntityTool`：args `{entity, view?}`。`view=="full"` → `location(entity)` 取 (path,s,e) → 读 `ctx.workspace_root.join(path)` 的 s..=e 行，带行号返回（复用 grep.rs 的路径安全 `sanitize_path`）；否则 → `skeleton(entity)`。文件读取失败或实体不存在 → 返回友好提示字符串。

（schema 的 `parameters` JSON 按各 args 字段写全 description；`kind` enum 列 function/struct/class/…；`direction` enum callers/callees/both；`view` enum skeleton/full。）

- [ ] **Step 4: 注册三工具**

`all_builtin_tools_with_sandbox` 的 vec 末尾追加：

```rust
        Arc::new(SearchCodeTool),
        Arc::new(TraverseGraphTool),
        Arc::new(RetrieveEntityTool),
```

- [ ] **Step 5: 跑测试通过**

Run: `cargo test -p deepseeknova-tools graph_tools`
Expected: 2 passed

- [ ] **Step 6: Commit**

```bash
git add crates/deepseeknova-tools
git commit -m "feat(tools): search_code / traverse_graph / retrieve_entity graph tools"
```

---

### Task 10: runtime — 装配 GraphIndex（后台构建 + 检索策略提示）

**Files:**
- Modify: `crates/deepseeknova-runtime/src/lib.rs`（`build_agent` 内装配）；`crates/deepseeknova-runtime/Cargo.toml`（加 `deepseeknova-graph`、`deepseeknova-tools`（若未依赖）依赖，以及 `tokio`）

- [ ] **Step 1: 写失败测试（runtime lib.rs 尾部 `#[cfg(test)]`）**

```rust
#[test]
fn build_agent_wires_graph_when_enabled() {
    use deepseeknova_config::Config;
    let mut config = Config::default();
    config.graph.enabled = true;
    let root = std::env::temp_dir().join(format!("dnv-graph-wire-{}", std::process::id()));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/x.rs"), "pub fn foo() {}\n").unwrap();

    let provider = std::sync::Arc::new(test_stub_provider());
    let agent = build_agent(&config, root.clone(), provider, 5, None).unwrap();
    // graph 工具已注册
    let names = agent.tool_names(); // 需暴露一个 pub fn tool_names(&self)->Vec<String>
    assert!(names.iter().any(|n| n == "search_code"));
    assert!(names.iter().any(|n| n == "traverse_graph"));
    assert!(names.iter().any(|n| n == "retrieve_entity"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn build_agent_skips_graph_when_disabled() {
    use deepseeknova_config::Config;
    let mut config = Config::default();
    config.graph.enabled = false;
    let provider = std::sync::Arc::new(test_stub_provider());
    let agent = build_agent(&config, std::env::temp_dir(), provider, 5, None).unwrap();
    assert!(!agent.tool_names().iter().any(|n| n == "search_code"));
}
```

> `test_stub_provider()`：最小 Provider（返回空 chunk 流）；runtime 现有测试若已有类似 stub 直接复用，否则在测试模块内定义。`agent.tool_names()`：Task 8 已可顺带在 agent.rs 暴露 `pub fn tool_names(&self)->Vec<String> { self.tools.keys().cloned().collect() }`（若无则在此 Task 补上并单独 commit 到 agent）。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p deepseeknova-runtime build_agent_wires_graph`
Expected: FAIL

- [ ] **Step 3: 实现装配（build_agent 内，在工具注册之后、`Ok(agent)` 之前）**

```rust
    // ── 代码图：可选、后台构建、注入检索工具与句柄 ──
    if config.graph.enabled {
        match deepseeknova_graph::GraphIndex::open(&workspace_root, config.graph.max_file_size) {
            Ok(index) => {
                let handle: deepseeknova_tools::GraphHandle =
                    std::sync::Arc::new(std::sync::Mutex::new(index));
                // 后台首次/增量构建，不阻塞首轮
                let bg = handle.clone();
                tokio::spawn(async move {
                    if let Ok(mut idx) = bg.lock() {
                        if let Err(e) = idx.refresh() {
                            tracing::warn!("graph index refresh failed: {e}");
                        }
                    }
                });
                // 三工具已由 all_builtin_tools 注册；此处注入索引句柄供其取用
                agent = agent.with_extension(handle);
                // 检索策略提示（仅在有系统提示时追加）
                if config.graph.enabled {
                    agent = agent.with_appended_system_prompt(GRAPH_RETRIEVAL_HINT);
                }
            }
            Err(e) => tracing::warn!("graph index unavailable, tools will degrade: {e}"),
        }
    }
```

其中常量与 agent 的 `with_appended_system_prompt`（Task 8 风格的小 builder，若未实现则本 Task 在 agent.rs 补：把字符串追加到 `system_prompt`，None 时设为该串）：

```rust
const GRAPH_RETRIEVAL_HINT: &str = "\n\n## 代码检索策略\n\
定位代码时优先使用图检索工具，避免全片 grep 或整文件读取：\n\
1. `search_code` 按符号名/关键词定位候选实体；\n\
2. `traverse_graph` 查看调用者/被调用者关系；\n\
3. `retrieve_entity`（view=skeleton）看骨架，确认目标后再 view=full 或 read_file 取实现。";
```

> 注意：`agent` 需为 `mut`（现状已是 `let mut agent`）。`tokio::spawn` 要求 build_agent 处于 tokio runtime——desktop/CLI 的调用点都在 async 上下文（submit_prompt 内），满足；若单元测试非 async，用 `config.graph.enabled=false` 或忽略 spawn 失败（spawn 在无 runtime 时 panic，故测试用例里对 enabled=true 的那条需 `#[tokio::test]` 或在测试内 `tokio::runtime` 包裹——将 Step 1 的 `build_agent_wires_graph_when_enabled` 改为 `#[tokio::test]`）。

修正：把 Step 1 第一个测试标注改为 `#[tokio::test]`。

- [ ] **Step 4: 跑测试通过**

Run: `cargo test -p deepseeknova-runtime build_agent`
Expected: 2 passed

- [ ] **Step 5: Commit**

```bash
git add crates/deepseeknova-runtime crates/deepseeknova-agent
git commit -m "feat(runtime): wire GraphIndex into build_agent (bg refresh + retrieval hint)"
```

---

### Task 11: context — repo map 注入 PromptBuilder

**Files:**
- Modify: `crates/deepseeknova-context/src/lib.rs`（`PromptBuilder::build` 与 `CacheAwarePromptBuilder::build` 增 `repo_map: Option<&str>` 参数）；同步更新 `crates/deepseeknova-context/tests/memory_integration.rs` 与 lib.rs 内既有测试的调用点

现状调用点（需同步改签名，spec §5）：`tests/memory_integration.rs:119/140/167`、`lib.rs:1074/1090/1108/1120`。

- [ ] **Step 1: 写失败测试（context lib.rs 尾部 `#[cfg(test)]`）**

```rust
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
    // repo map 在 project context 之后
    assert!(sys.find("PROJECT_CTX").unwrap() < sys.find("pub fn foo()").unwrap());
}

#[test]
fn prompt_builder_none_map_is_noop() {
    let msgs = PromptBuilder::build("SYS", &[], &WorkingMemory::new(), &ProjectMemory::new(), None);
    assert!(!msgs[0].content.contains("Repo Map"));
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p deepseeknova-context prompt_builder_injects_repo_map`
Expected: FAIL（签名不匹配 / `Repo Map` 未注入）

- [ ] **Step 3: 实现**

- `PromptBuilder::build` 增末位参数 `repo_map: Option<&str>`；在注入 project memory 之后、`Available Tools` 之前插入：
  ```rust
  if let Some(map) = repo_map {
      if !map.is_empty() {
          system_content.push_str("\n\n---\n## Repo Map\n\n```\n");
          system_content.push_str(map);
          system_content.push_str("\n```\n");
      }
  }
  ```
- `CacheAwarePromptBuilder::build` 同理增 `repo_map: Option<&str>`，作为 `prefix_parts` 的一段（在 project memory 之后 push），从而纳入稳定前缀 hash。
- 更新全部现有调用点补 `None`（tests/memory_integration.rs 三处、lib.rs 测试四处），保持编译。

- [ ] **Step 4: 跑测试通过 + context 全测试不回归**

Run: `cargo test -p deepseeknova-context`
Expected: 新 2 passed，其余 passed

- [ ] **Step 5: Commit**

```bash
git add crates/deepseeknova-context
git commit -m "feat(context): optional repo_map injection in PromptBuilder / CacheAwarePromptBuilder"
```

> 说明：真正把 GraphIndex 的 repo_map 喂给 PromptBuilder 的接线，取决于 agent 内 PromptBuilder 的调用位置。若 agent 当前直接调 `PromptBuilder::build`，在 Task 10 装配了 handle 后，可在 agent run 开始处 `handle.lock().repo_map(config.graph.repo_map_tokens, &seeds)` 生成 map 传入——本 Task 只落地「参数与注入位置」，实际喂数据作为 Step 6 收尾接线（下）。

- [ ] **Step 6: 接线 repo map 数据（agent run 起点）**

在 agent 生成 prompt 处（grep `PromptBuilder::build(` 在 agent.rs 的调用），若 `ctx.extensions` 或 agent 持有 `GraphHandle`，本轮开始时取 `repo_map(budget, seeds)`（seeds = 本轮 user 输入中出现的、能在库中 find_by_name 命中的符号名；无则空）传入；异常时传 `None`。预算取自 config（agent 需在 build_agent 时记住 `repo_map_tokens`，加一个 `with_repo_map_budget(usize)` builder + 字段）。此步若 agent prompt 构造较复杂，允许折中：先接「无种子的全局 repo map」，个性化种子留 TODO 注释并在 spec 成功标准 4 验证全局版即可。

Run: `cargo test -p deepseeknova-agent && cargo test -p deepseeknova-runtime`
Expected: 无回归

```bash
git add crates/deepseeknova-agent crates/deepseeknova-runtime
git commit -m "feat(agent): feed graph repo map into system prompt at run start"
```

---

### Task 12: 收尾回归与文档

**Files:**
- Modify: `crates/deepseeknova-graph/README.md`（补 API 用法）；`AGENTS.md` 项目结构清单（加 graph crate 一行，若有该清单）

- [ ] **Step 1: 全量回归**

Run: `make check`
Expected: fmt + clippy + test + doc 全绿。若 clippy 对 graph crate 报 `unwrap_used`（core 有 `deny`，graph 无此约束，但保持整洁）——把生产路径的 unwrap 换成 `?`/`unwrap_or`。

- [ ] **Step 2: 真实仓库冒烟（本仓库自举）**

Run:
```bash
cargo run -p deepseeknova-cli -- --help >/dev/null   # 确认 CLI 仍可构建
# 手动：在本仓库根跑一次 GraphIndex 冒烟（写个 examples 或用现有 CLI 子命令若有）
```
Expected: `.deepseeknova/graph.db` 生成；`search_code("PermissionGate")` 首位命中定义（spec 成功标准 2）。若 CLI 无暴露入口，本步可用 `cargo test -p deepseeknova-graph -- --ignored` 形式加一个 `#[ignore]` 的「索引本仓库」集成测试替代。

- [ ] **Step 3: 更新 README 与结构清单，提交**

```bash
git add crates/deepseeknova-graph/README.md AGENTS.md
git commit -m "docs(graph): usage and workspace structure update"
```

---

## 自检记录（写计划后执行）

- **Spec 覆盖**：§3 crate=Task1-6；§4 工具=Task9；§5 repo map 注入=Task11；§6 装配=Task10；§7 config=Task7；§8 错误=Task2(GraphError)+各工具降级；§9 测试=各 Task 的 TDD 步；§10 不做项=计划未含（正确）；§11 成功标准=Task12 Step2 验证标准 2、repo map 预算在 Task6 测试断言标准 4。agent 通用注入（Task8）是 §6 装配的前置依赖，spec 未显式列但为实现必需。
- **占位符扫描**：parser.rs / store.rs 的 `todo!` 与 "省略" 均配「关键实现说明」明确规则且注明「不得留 todo! 提交」，非计划占位符而是受控实现指引；无 TBD/裸 TODO。
- **类型一致性**：`GraphHandle`（tools 定义，runtime/测试引用一致）；`GraphIndex::open(root, max_file_size)`（Task6 定义，Task9/10 同签名）；`Direction`（store 定义，lib re-export，tools 用）；`render_repo_map(nodes, budget)`（Task6 定义与调用一致）；`with_extension`（Task8 定义，Task9/10 使用）；`PromptBuilder::build(..., repo_map)` 全调用点 Task11 统一。

---

## 执行交接

计划已保存到 `docs/superpowers/plans/2026-07-26-graph-engineering.md`。两种执行方式：

1. **Subagent-Driven（推荐）** — 每个 Task 派新 subagent，任务间双阶段 review，快速迭代。
2. **Inline Execution** — 本会话内按 executing-plans 批量执行，带 checkpoint。

选哪种？

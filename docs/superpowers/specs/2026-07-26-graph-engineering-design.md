# Graph Engineering — 代码图检索与 Repo Map 设计

- 日期：2026-07-26
- 状态：已评审（用户逐段确认）
- 目标 crate：`deepseeknova-graph`（新增）+ `deepseeknova-tools` / `deepseeknova-context` / `deepseeknova-runtime` / `deepseeknova-config`（增量）

## 1. 背景与问题

Agent 当前定位代码只有两条路：`grep` 正则全片扫描 + `read_file` 整文件读取。中大型仓库中这会烧掉大量 token（一个 3000 行文件里只需要 40 行函数），且模型缺乏全局结构视野，多轮试错进一步放大消耗。

外部验证过的解法（本设计的来源）：

| 项目 | 借鉴点 |
|---|---|
| Aider repo map | tree-sitter 提取 def/ref → 符号引用图 → 个性化 PageRank → token 预算内输出最重要符号的骨架地图（默认 1k tokens） |
| Agentless (OpenAutoCoder) | 层级定位：骨架先行，文件级 → 类/函数级 → 行级逐层下钻，不给 LLM 看实现 |
| LocAgent (ACL 2025) | 代码库解析为有向异构图（dir/file/class/function 节点；contain/import/invoke/inherit 边），给 agent 图检索工具做多跳推理；树状展示优于平铺；成本 −86%，文件级定位 92.7% |

## 2. 决策记录

| 决策点 | 结论 |
|---|---|
| 消费方式 | 图检索工具（LocAgent 式）+ 自动 repo map（aider 式）一次到位 |
| 语言范围 | 一期 Rust / Python / JavaScript / TypeScript（架构预留多语言扩展） |
| 索引策略 | SQLite 持久化（`.deepseeknova/graph.db`）+ mtime/hash 增量刷新 |
| 架构切分 | 新建 `deepseeknova-graph` crate，tree-sitter 重依赖隔离；tools/context 只加薄适配层 |

被否方案：塞进 `deepseeknova-context`（职责膨胀、依赖传染）；塞进 `deepseeknova-core`（零重依赖地基不可污染）；向量嵌入语义检索（YAGNI，BM25+图排序先行）。

## 3. `deepseeknova-graph` crate

```
crates/deepseeknova-graph/
├── src/parser.rs    tree-sitter 解析（rust/python/javascript/typescript）
├── src/model.rs     实体与关系模型
├── src/store.rs     SQLite 持久化 + FTS5(BM25) + 增量刷新
├── src/rank.rs      个性化 PageRank（幂迭代自实现，~60 行）
├── src/repomap.rs   token 预算内骨架地图生成
└── src/lib.rs       GraphIndex 门面 API
```

### 3.1 图模型

- 节点 `NodeKind`：`Directory / File / Struct / Enum / Trait / Class / Function / Method`；字段：`id, kind, name, path, start_line, end_line, signature, doc`（doc 取首行）
- 边 `EdgeKind`：`Contains / Imports / Calls / Implements / References`
- 精度边界（明确接受）：`Calls/References` 采用「标识符名 → 定义名」名称级近似匹配（aider/LocAgent 同款），不做类型解析；同名误连靠 PageRank 权重稀释。不追求 LSP 级精确。

### 3.2 存储（`.deepseeknova/graph.db`）

```sql
files(path PRIMARY KEY, mtime, hash)          -- 增量刷新依据
nodes(id PRIMARY KEY, kind, name, path, start_line, end_line, signature)
edges(src, dst, kind)
symbol_fts USING fts5(name, signature, doc, id UNINDEXED, path UNINDEXED, tokenize='porter unicode61')  -- 内建 BM25
```

- 增量：mtime 变化 → 比对 hash → 变化则删除该文件全部节点/边重解析；节点 id 稳定（`path#name#start_line` 派生），未变文件不动
- 排除：复用 WorkspaceIndex 的 .gitignore 逻辑 + 硬排除 `target/ node_modules/ .git/ dist/`；超过 `max_file_size` 跳过
- 损坏恢复：graph.db 是派生数据，损坏即删库全量重建，不做 schema 迁移

### 3.3 GraphIndex API

`open(root)` / `refresh()`（增量）/ `search(query, kind?, limit)` / `neighbors(id, edge_kinds, direction, hops)` / `skeleton(id)` / `repo_map(token_budget, personalization: &[String])`

### 3.4 新增依赖（仅本 crate）

`tree-sitter` + `tree-sitter-rust` / `tree-sitter-python` / `tree-sitter-javascript` / `tree-sitter-typescript`。PageRank 自实现，不引 petgraph。

## 4. 图检索工具三件套（`deepseeknova-tools`）

全部 `read_only = true`、`FileRead` capability、输出限长；GraphIndex 经 `ToolContext.extensions` 注入；索引未就绪时降级为文字提示「索引构建中，请用 grep」，不报错。

### 4.1 `search_code`
- 入参：`query`（关键词/符号名/报错片段）、`kind?`、`limit?`（默认 10）
- 实现：FTS5 BM25 + 符号名精确/前缀匹配加权合并
- 输出：`rank. kind name — path:start-end · signature · score` 单行制

### 4.2 `traverse_graph`
- 入参：`entity`（id 或 `path:name`）、`direction`（callers/callees/both）、`edge_kinds?`、`hops?`（默认 2，上限 3）
- 实现：BFS，兄弟节点按 PageRank 排序，树状缩进输出；每节点子边限宽 + `…(+k more)`；单次输出硬上限 ~2000 tokens

### 4.3 `retrieve_entity`
- 入参：`entity`、`view`（`skeleton` 默认 / `full`）
- `skeleton`：签名 + doc + 子实体签名；`full`：仅该实体行区间源码（带行号）——省 token 主力

### 4.4 提示词配合

`build_agent` 系统提示追加检索策略：定位代码优先 `search_code` → `traverse_graph` → `retrieve_entity(skeleton)` → 确认后才 `retrieve_entity(full)`/`read_file`，避免直接 grep/整文件读取。

## 5. Repo Map 注入（`deepseeknova-context`）

- 算法：个性化 PageRank（本轮对话提到的文件/符号作种子，权重 ×10）→ 分数降序贪心装入预算 → 按文件分组输出骨架（`│ 签名` + `⋮` 省略）
- token 估算沿用项目惯例 `chars/4`（与 history.rs 一致）
- 注入点：`PromptBuilder::build` / `CacheAwarePromptBuilder::build` 增加 `repo_map: Option<&str>` 参数，位于 Project Context 之后、对话历史之前（稳定前缀区尾部）
- 前缀缓存权衡：map 只随代码结构变化在**轮次边界**刷新，多轮对话内字节稳定；agent 改码后下一轮 map 变化会 miss 一次前缀缓存——以此换取少读整文件的净收益

## 6. 装配与生命周期（`deepseeknova-runtime`）

1. `build_agent` 时 `GraphIndex::open(workspace_root)`：已有 graph.db 秒级可用；`tokio::spawn` 后台 `refresh()`（首次=全量构建），不阻塞首轮
2. 索引就绪前：工具降级提示、repo map 为空——行为退化为现状，纯增量增强
3. 刷新时机：每轮 run 开始增量检查（通常毫秒级）
4. CLI 与 desktop 共用该装配点，无前端改动

## 7. 配置（`deepseeknova-config` `[graph]` 新节，全 `#[serde(default)]`）

```toml
[graph]
enabled = true            # false 时零开销，行为=现状
repo_map_tokens = 1024    # 0 = 不注入 map，仅保留工具
max_file_size = 524288    # 字节，超过跳过解析
```

## 8. 错误处理（thiserror 惯例）

- `GraphError`：`Parse { path, lang }` / `Storage(#[from] rusqlite::Error)` / `IndexBusy` / `EntityNotFound(String)`
- 单文件解析失败：`tracing::warn` 跳过，索引尽力而为
- 工具层错误全部转为对模型友好的文字提示，不打断 run

## 9. 测试策略

- `parser`：4 语言各一 fixture → 实体/边提取断言（数量、名称、行区间）
- `store`：增量刷新——改一个文件仅该文件节点被替换，其余节点 id 不变
- `rank`：3 节点手算收敛值断言；个性化种子提权断言
- `repomap`：预算硬上限断言（budget=200 → 输出 ≤ 220×4 chars 容差）；空索引 → 空 map
- `tools`：临时仓库集成测试，search→traverse→retrieve 三连产物断言
- 验收：`make check` 全绿（graph crate 自动纳入 workspace clippy/test）

## 10. 明确不做（二期候选）

向量嵌入语义搜索、LSP 级类型精确解析、文件监听实时增量、桌面端图可视化 UI、跨仓库索引。

## 11. 成功标准

1. 对本仓库（21 crates）全量索引 < 30s，增量刷新 < 500ms（单文件变更）
2. `search_code("PermissionGate")` 首位命中定义实体；`traverse_graph` 能列出其 callers
3. `retrieve_entity(full)` 返回字节数 ≤ 对应 `read_file` 的 20%（典型函数场景）
4. repo map 在 1024 token 预算内呈现 ≥ 30 个最高分符号签名
5. `[graph] enabled=false` 时所有行为与现状完全一致

# 任务书 G：Graph 新增 Go 语言支持

## 1. 意图
graph 代码图引擎现仅支持 Rust/Python/JavaScript/TypeScript（parser.rs Lang 枚举 4 项）。本轮新增 Go：tree-sitter-go 解析符号/调用/引用/导入 + go.mod 外部依赖识别，让 Go 项目享受与既有语言同等的 trace_code/impact_code/explore_code/deps_code 能力。

## 2. 我替领导拍的板
- 新依赖 tree-sitter-go = 0.25.0（crates.io 实测最新，与现有 tree-sitter 0.25 / tree-sitter-rust 0.23 生态一致；书内批准，仅此一个新依赖）。
- 不动表结构 → SCHEMA_VERSION=4 保持，无需强制全量重解析；若实现中发现必须加 language 列才升级 v5（graph 先例：版本不符 DELETE FROM files 全量重解析，store.rs:229-236）。
- 工具面不加 language 参数（既有工具按实体名/路径操作，天然语言无关）；deps_code 无 entity 提示语（graph_tools.rs:792-793）补 go.mod。
- 基线=当前分支 feat/memory-lifecycle @ 68fb094（graph crate 与 memory 分支改动零重叠）。
- GRAPH_RETRIEVAL_HINT（runtime/src/lib.rs:25）已核实无语言表述，**不动**。

## 3. 白名单（只改这些，其余只读）
- crates/deepseeknova-graph/（Cargo.toml、src/parser.rs、src/store.rs、src/lib.rs、README.md、tests/）
- crates/deepseeknova-tools/src/graph_tools.rs（仅 deps_code 提示语一行）
- GUIDE.md、CHANGELOG.md、PROGRESS.md、BLOCKED.md

## 4. 任务
- 任务 1：解析接入。Lang::Go + from_path ".go" + language() 映射 tree-sitter-go；五个分派点分支：entity_kind（function_declaration / method_declaration / type_declaration / struct_type / interface_type，parser.rs:99-130）、is_import（import_declaration / import_spec，:132-135）、is_call（call_expression，:140-142）、extract_signature（Go: func name( params )，:186-187 风格对齐）、parse_source 主循环（:276-438 各段）。**tree-sitter-go 0.25 grammar 节点名必须先实测**（写探测测试打印节点类型核对，不许凭猜）。
- 任务 2：Go fixture 采集验证。GO_SRC 内联 fixture（对齐 parser.rs:485-668 既有风格）覆盖：包级函数、struct + method、interface、import（stdlib / 本地 / 第三方路径）、调用链 a→b→c、引用；断言 entities/calls/refs/imports 各 ≥1。
- 任务 3：go.mod 外部依赖。is_manifest（store.rs:1117-1119）加 go.mod；parse_manifest_deps（:1123-1128）加 go.mod 行级解析（require 段 module path）；deps_code 提示语补 go.mod；测试：go.mod 解析 + deps_code 提示含 go.mod。
- 任务 4：文档 + 收尾。GUIDE A3 节与 graph README 语言列表补 Go；CHANGELOG Added；cargo fmt + make check 全绿 + 反向验证（改坏一条 Go fixture 断言→红→还原→绿）；提交 feat/memory-lifecycle（不 push）。

## 5. 防作弊
- 测试只增不减：graph ≥35（Go fixture ≥3）、tools ≥ 基线、workspace 保持 0 failed。
- 新增依赖仅 tree-sitter-go，不许加其他；不许删既有测试/放宽断言（注意 parser.rs:582-585 `fn go(&self)` 动态分发测试与 store.rs:1418-1424 `search("go")` 短词测试是既有语义，不得误改）。
- 新行为各 ≥1 条测试；组合：Go 的 import 三态（stdlib/本地/第三方）都覆盖。
- 反向验证必须真红真绿，贴输出。

## 6. 完成条件（两条硬指标 + 止损）
- 硬指标 A：`make check` EXIT=0；graph ≥35、workspace 0 failed。
- 硬指标 B：Go fixture 全断言过；deps_code 对含 go.mod 的 Go 项目输出外部依赖；GUIDE/README 语言列表含 Go。
- 止损：同一验收连败 3 次换路径；结果比基线差就回滚如实报告；超 3 小时汇报进度停手。
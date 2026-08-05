# CLOSEOUT — 记忆语义检索（embedder）最小闭环（2026-08-05 dev-loop 轮）

## 六事实面状态

| 事实面 | 状态 | 证据 |
|--------|------|------|
| 代码 | changed-and-verified | 分支 feat/semantic-retrieval；`make check` EXIT=0、workspace 0 failed（2 既有 ignored）；core 137+2 / provider 40 / config 35+18 / runtime 53 / tools 67+12+7 / cli 32 |
| 运行态 | verified-current | CLI 冒烟：`memory stats` 输出 `embedded=0`；`memory embed-backfill` 无 provider 时 attempted=0 ok=0 不 panic；反向验证红（1 failed）→ 绿（1 passed）；RemoteEmbedder 用真实本地 HTTP 服务端到端测（路径/Bearer/body/错误/解析） |
| 文档 | changed-and-verified | GUIDE 记忆节补 embedder 配置/回填/CLI；CHANGELOG Added；BLOCKED 两处「语义检索」转已做（代码图侧仍待裁决）；PROGRESS 回执 + 自验收清单逐条打勾 |
| 规则 | not-applicable | AGENTS.md 未改动（无新增约定必要） |
| 记忆 | not-applicable | 无平台记忆写入 |
| 工作区 | changed-and-verified | 分支工作树仅本轮 14 文件（12 修改 + 2 新增）；无未跟踪残留、无 stash |

## 本轮交付物核对（dev-loop 六件证据）
1. 任务书：docs/superpowers/plans/2026-08-05-semantic-retrieval-plan.md（六节齐全，≤4000 字符）
2. 测试全绿：`make check` EXIT=0；core 132→137（+2 集成）、provider 35→40、
   config 51→53、runtime 52→53、tools 66→67（另 12+7 集成不动）
3. 审查报告：crates/REVIEW.md 本轮分节——M1/M2（锁内 HTTP + fallback 语义）已修、
   L1（未知后端静默）已修、L2（测试替身重复）接受；修复后重审 0 新问题
4. 收尾报告：本文件
5. BLOCKED.md：无执行阻塞；local 后端、代码图侧语义检索、TUI 协议面板 / 计划载体 /
   多模型反思 / 记忆清理 UI 留待裁决
6. 提交：分支 feat/semantic-retrieval（不 push，是否提交/推送由用户决定）

## 遗留（如实）
- local embedder 未实现：配置 `embedder="local"` 会显式 warn 并回落 FTS（不会静默）。
- 代码图侧语义检索仍未做（BLOCKED 已记）。
- `RemoteEmbedder::embed` 是同步 trait 内 `block_on`：在 async 工具路径会阻塞一个
  worker 线程至多一个超时（默认 30s）；有独立 runtime + 超时兜底，已在 GUIDE 记录，
  未来可改为 async trait + spawn_blocking。

---

# CLOSEOUT — Protocol 增强收尾 + Graph Go 语言（2026-08-05 dev-loop 双域轮）

## 六事实面状态

| 事实面 | 状态 | 证据 |
|--------|------|------|
| 代码 | verified-current | 分支 feat/memory-lifecycle @ 4d47a6a；`make check` EXIT=0，workspace 0 failed（2 既有 ignored）；graph 42 / metrics 21 / runtime 52 / agent 231 / cli 32 / core 132 |
| 运行态 | verified-current | 三轮反向验证全部真红→真绿（task_rate 断言、Go fixture 断言、R1 refs 归属断言）；G 域 grammar 节点名与 tree-sitter-go 0.25.0 node-types.json 逐项核对一致 |
| 文档 | changed-and-verified | GUIDE 协议节（task_rate/record_use）+ A3 节（Go 语言）；graph README 语言列表；CHANGELOG Added/Fixed；PROGRESS 双域回执 + 自验收清单已打勾；BLOCKED 已更新（两域转已做、阻塞解除、新增待裁决四项） |
| 规则 | not-applicable | AGENTS.md 未改动（无新增约定必要） |
| 记忆 | not-applicable | 无记忆库/记忆代码改动 |
| 工作区 | changed-and-verified | 分支工作树干净；crates/REVIEW.md 含第三/四轮审查 + 修复轮 ×2 记录（覆盖声明、真实行号评论、修复证据）；无未跟踪残留 |

## 本轮交付物核对（dev-loop 六件证据）
1. 任务书 ×2：docs/superpowers/plans/2026-08-05-protocol-followup-plan.md、2026-08-05-graph-go-plan.md（六节齐全，存档保留）
2. 测试全绿：make check EXIT=0；graph 32→42、metrics 17→21、runtime 48→52（+8 新测试，零放宽既有断言）
3. 审查报告：crates/REVIEW.md 第三轮（G 域：1 medium + 4 low）、第四轮（P 域：5 low）、修复轮 ×2 记录、第二轮复核（发现 R1 修复引入缺陷并定位）
4. 收尾报告：本文件
5. BLOCKED.md：无执行阻塞；待裁决四项（TUI 协议面板 / 结构化计划载体 / 多模型反思对比 / 记忆清理 UI）+ 既有（agent_loop 反思 UI、AST 全量持久化、MCP 外壳、语义检索）
6. 提交：285fe60（G 域）+ 95f695e（P 域）+ 7f49ffc（修复轮 1）+ 4d47a6a（修复轮 2），分支未 push

## 审查循环质量说明
- 第一轮两域审查 0 high；修复轮 1 修 4 项（G-M1 分组类型、G-L4 go.mod 尾注释、P-L2 回填语义、P-L4 warn 区分）
- 第二轮复核发现 G-M1 修复引入 refs 归属缺陷（R1，/tmp 实测铁证：伪边 B→A），修 R1/R2（成员逐个入栈 + doc/signature 恢复）
- 最终复核 4 项核对全过（push_entity 语义等价、push/pop 严格 1:1、type_alias 跳过、doc 回退两形态），42 passed，无新问题

## 遗留（如实）
- 无。已知待裁决项全部在 BLOCKED（见上），非本轮范围。

---

## CLOSEOUT — 依赖健康修复（2026-08-05 收尾轮）

### 六事实面状态

| 事实面 | 状态 | 证据 |
|--------|------|------|
| 代码 | changed-and-verified | 未提交工作区 13 文件；`make check` EXIT=0（fmt/clippy/全 workspace/doctest/doc 零警告）；`make audit` ok |
| 运行态 | not-applicable | 无部署/服务面，本轮仅本地构建与测试验证 |
| 文档 | changed-and-verified | CHANGELOG Fixed 条目；telemetry lib.rs 与 README 同步为 tracing-only；README/GUIDE/BUILDING 无版本级过期表述 |
| 规则 | changed-and-verified | AGENTS.md §5 新增「并行测试临时目录撞名」防错条目；deny.toml skip/ignore 与 CI security.yml 同步（lru/paste 豁免移除） |
| 记忆 | not-applicable | 无平台记忆写入 |
| 工作区 | pending | 13 文件未提交（分支 feat/memory-lifecycle），无未跟踪残留、无 stash、无临时/备份文件；提交与否待用户决定 |

### 本轮交付物核对

1. 依赖升级：OTel 0.27→0.32（trace-only）、ratatui 0.29→0.30、crossterm 0.28→0.29、rand 0.8→0.9、thiserror 1→2、criterion 0.5→0.8；重复依赖警告 16 组→0（剩余 5 组上游分叉登记 skip）。
2. 测试全绿：`make check` EXIT=0；scanner flaky（纳秒撞名）改 `tempfile::tempdir()` 后连跑 3 次全过。
3. 审查报告：crates/REVIEW.md 本轮分节（1 medium C1 → 已修 → 重审 0 问题）。
4. 安全：`make audit` ok；`cargo audit` 仅 1 项已登记允许警告（RUSTSEC-2025-0068）。

### 遗留（如实）
- 未提交：13 个文件（含 Cargo.lock），待用户决定 commit / push。
- 无未消除的构建或审计 warning；BLOCKED 待裁决项均非本轮范围。

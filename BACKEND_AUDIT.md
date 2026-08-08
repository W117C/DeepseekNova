# BACKEND_AUDIT — DeepseekNova 后端完整性审计

日期：2026-08-01 ｜ 基线：feat/tui-complete@c19898d（工作树干净）｜ 方法：README/AGENTS 声明 vs 源码实测，
每条结论带 file:line 或命令输出；`make check` EXIT=0，workspace 共 638 测试通过（633 单测/集成 + 5 doctest）、
2 个既有 ignored（graph/provider 集成测试）。`make check` 明确排除 desktop（Makefile:16-18）。

> **2026-08-08 更新**：desktop（Tauri 壳）已整体移除（`crates/deepseeknova-desktop` 删除，
> 见 BLOCKED.md「观测台前端 UI + TUI 演进轮」节）。下文中所有 desktop/桌面端条目均已过时，
> 仅保留历史记录。

## 总览（22 crate）

| crate | 源码行数/文件数 | 测试通过 | 结论 |
|---|---|---|---|
| cli | 2833/6 | 27 | 完整 |
| config | 1540/1 | 38 | 完整（默认提示词缺口见下） |
| provider | 3003/10 | 28 | 完整（1 集成测试 ignored） |
| agent | 7403/15 | 113 | 核心完整（缺默认系统提示词，本任务修复） |
| tools | 2857/12 | 64 | 完整 |
| core | 6837/44 | 85 | 完整 |
| mcp | 1425/7 | 20 | 完整 |
| event | 373/1 | 7 | 完整 |
| graph | 1801/6 | 19 | 完整（1 集成测试 ignored） |
| permission | 735/1 | 19 | 完整 |
| context | 1819/2 | 44 | 完整 |
| runtime | 1539/1 | 25 | 完整（graph hint 装配缺陷见下） |
| sandbox | 661/3 | 13 | 完整 |
| checkpoint | 466/1 | 11 | 完整 |
| store | 550/1 | 16 | 完整 |
| tui | 2392/1 | 35 | 完整 |
| serve | 311/1 | 8 | 完整（REST+SSE 三路由） |
| skills | 381/2 | 15 | 完整 |
| telemetry | 177/1 | 3 | 完整（轻量 OTLP） |
| ~~desktop~~ | ~~2937/25~~ | ~~未跑*~~ | ~~有独立通道（见下）~~ |
| security | 603/7 | 29 | 完整 |
| scanner | 633/6 | 14 | 完整 |

*~~desktop 不在 `make check`（Makefile:16-18 全部 `--exclude deepseeknova-desktop`），走独立
`make check-desktop` 与 CI desktop.yml；本审计未运行前端 lint/桌面构建。~~

## 逐 crate 核对

1. **cli**：11 个子命令 Run/Plan/Scan/Chat/Serve/Setup/Config/Memory/Checkpoint/Artifacts/Init
   （cli.rs:42-122）；Chat 支持 `--tui`（cli.rs:88-95）；27 单测绿。声明与实现一致。
2. **config**：AgentConfig.system_prompt 为 `Option<String>`（lib.rs:482,515），合并逻辑在
   lib.rs:1147；38 测试绿。**缺口**：默认 `None` → 主 agent 裸跑（配合 agent 行见下）。
3. **provider**：OpenAI 兼容 + Anthropic + DeepSeek-V4 thinking（lib.rs:3-4,133,189）；
   28 单测绿；`deepseek_reasoning_protocol` 集成测试 1 个 ignored（既有，原因未追，非本任务范围）。
4. **agent**：run_agent_loop 含工具循环、verify（verify.rs + agent.rs:913-954）、review
   （review.rs + agent.rs:959-974）、delegate（delegate.rs）、observe 压缩
   （agent.rs:1705 compress_observation）、compaction（compaction.rs）、budget（budget/）、
   plan_mode（plan_mode.rs）、coordinator（coordinator.rs）；108 单测 + 5 集成 + 1 doctest 绿。
   **缺口**：`system_prompt: None` 默认（agent.rs:158），只在 `Some` 时注入（agent.rs:531）。
5. **tools**：17 个 `impl Tool for` 结构（fs×4、graph×3、memory×3、ls/glob/grep/shell/
   web_fetch/todo/delegate）；64 测试绿。README「13+ 内置工具」声明属实（17>13）。
6. **core**：44 源文件覆盖 chunk/executor/runner/planner/registry/tool/types/identity/memory/
   prefix 等；85 测试绿。声明「核心类型层」与实现一致。
7. **mcp**：client/connection/discovery/http_client/adapter 齐全；20 测试绿。完整。
8. **event**：事件总线 7 测试绿。完整（轻量）。
9. **graph**：tree-sitter 解析（parser.rs）+ SQLite FTS5 存储（store.rs）+ PageRank（rank.rs）
   + repo map（repomap.rs）；19 单测绿；`self_index` 集成测试 ignored（既有）。
10. **permission**：allow/ask/deny 规则模型（lib.rs:33-48,125-127）；19 测试绿。README「12 条
    规则」未逐条核对（未见常量清单，标假设，未验证）。
11. **context**：PromptBuilder（lib.rs:177）+ CacheAwarePromptBuilder（:266）+
    OrderedPromptBuilder（:423）；44 测试绿。提示词装配点（自动注入工具描述/项目上下文/
    repo map/压缩摘要），本任务不改此处。
12. **runtime**：组合根，config→agent 装配（lib.rs:306-307）、graph hint 追加（:424）、
    delegate 预设装配（:841-856,911,973）；25 测试绿。**缺陷**：无 system_prompt 时
    `with_appended_system_prompt` 把 GRAPH_RETRIEVAL_HINT 变成整个系统提示词（:424 触发
    agent.rs:196 None→Some(extra)）。
13. **sandbox**：macOS Seatbelt / Linux bubblewrap（lib.rs:4-9,57-65）；13 测试绿。完整。
14. **checkpoint**：SHA-256 快照 + 持久化 + rollback（lib.rs:16-63）；11 测试绿。完整。
15. **store**：JSONL SessionStore load/append/delete/list（lib.rs:94-155）；16 测试绿
    （含 resume_fidelity 2）。完整。
16. **tui**：35 单测 + 1 doctest 绿（本轮视觉改造已交付，PR #53）。完整。
17. **serve**：axum Router：`/health`、`/v1/chat`、`/v1/approval` + SSE（lib.rs:97-100）；
    8 测试绿。与 README「HTTP/SSE 服务」声明一致。
18. **skills**：frontmatter 解析（loader.rs:136）+ 激活工具返回 system_prompt
    （lib.rs:60-101）；15 测试绿。完整。
19. **telemetry**：OTLP 导出（lib.rs 模块文档）；3 测试绿。完整（轻量）。
20. **~~desktop~~（已移除）**：~~61 处 `#[tauri::command]`（commands/ 下 22 个文件，rg 实测）
    vs README 声明「44 个 Tauri 命令」——数字过时；Rust 侧测试未跑（独立通道）。~~
21. **security**：audit/capability/context/limits/policy 模块齐全；29 测试绿。完整。
22. **scanner**：静态规则（rule.rs）+ AI 调查（investigate.rs build_prompt:11）+ 报表
    （report.rs）+ 发现模型（finding.rs）；14 测试绿。完整。

## 关键发现

**阻塞（本任务处理）**
- 主 agent 默认无系统提示词（agent.rs:158/531 + config lib.rs:515 None 默认）→ 裸跑。
- runtime 无提示词时 graph hint 会独占 system prompt（runtime/lib.rs:424 + agent.rs:196）。

**建议（写 BLOCKED.md 待裁决）**
- README tests 徽章 536 落后实际 638（README.md:44）。
- ~~README「44 个 Tauri 命令」vs 实测 61 个 `#[tauri::command]` 标记。~~（desktop 已移除）
- graph `self_index` 与 provider `deepseek_reasoning_protocol` 集成测试 ignored（既有）。
- ~~desktop 不在 `make check`，本机完整校验需 `make check-desktop`（需前端产物）。~~（desktop 已移除）

**顺手活（写 BLOCKED.md 待裁决）**
- verify 目前是确定性命令 + 固定文案（agent.rs:933），可考虑 LLM 化。
- ~~desktop 设置页 system_prompt 入口未接新默认值（前端已搁置）。~~（desktop 已移除）

## 结论

后端功能整体完整：22 个 crate 声明与实现基本一致，638 测试全绿，无大规模 stub；
最大的真实缺口是「默认系统提示词缺失」与「子提示词风格不统一」，由本任务 2/3 修复；
其余为文档数字过期与既有 ignored 测试，不阻塞交付。

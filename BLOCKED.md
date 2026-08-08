# BLOCKED — 待裁决清单

## 观测台前端 UI + TUI 演进轮（2026-08-07）

### 本轮明确不做、留给领导裁决
- ~~**Tauri 壳（P1）**~~（2026-08-08 已撤销：`crates/deepseeknova-desktop` 已整体
  移除，桌面端不再是产品方向；历史见 git）。
- ~~**桌面端后续页（P3/P4）**~~（2026-08-08 已撤销：同 Tauri 壳，桌面端移除）。
- ~~**界面文案语言**~~（2026-08-07 已拍板：**双语 i18n 框架，英文默认 + 中文可选**，TUI 词表已落地，见 PRODUCT.md 与 roadmap 记忆）。
- **Logo/应用图标**：无现存资产，实现期先用文字标。
- **风险标签接线端到端测试**：`responder.request` 收到带 `[风险:…]` 前缀描述
  的 agent 集成断言未补（本轮有纯函数 + 权限分类 + TUI 渲染测试覆盖）。
- ~~`crates/deepseeknova-tui/src/repro_tmp.rs`~~（2026-08-07 已清：临时复现测试，
  文件头注明“用完即删”，已移入废纸篓并移除 `mod repro_tmp;`）
- ~~`.impeccable/mocks/obs-comp-d-combined-agnes-v2.png`~~（2026-08-07 已清：
  被否决的 Agnes 文生图备份，git rm 可经历史恢复）

### 执行阻塞
- 无（本轮无阻塞；全量验证以 `make check` EXIT=0 为准）。

## 路线图调研轮（2026-08-07，领导裁决）

七子代理并行调研（竞品对标 / 执行引擎 / 安全域 / 接口面 / 代码健康 / 发布就绪）
后的裁决与待办：

### 已拍板（2026-08-07 领导确认，落地为后续轮任务）
- **默认安全姿态**：默认开启权限门控（`permissions.enabled=true`）；沙箱保持关闭但
  启动横幅明示未启用；另提供 `--secure-defaults` 一键开沙箱。~~（P0-7）~~ **已做
  （2026-08-07 第二批 B3**：config 默认 true + `--secure-defaults` + runtime 横幅 +
  REPL 审批 responder）。
- **Ask 无 responder 默认 fail-closed**：非交互/库级默认 deny，新增配置项允许显式
  改回 allow。~~（P0-6）~~ **已做（2026-08-07 第二批 B3**：`ask_without_responder`
  默认 deny + `with_ask_without_responder_deny` builder + 两条回归测试）。
- **i18n**：双语框架，英文默认 + 中文可选，TUI 词表已落地。~~（第三批受阻 402）~~
  **已做（2026-08-08 第四批**：`crates/deepseeknova-tui/src/i18n/` 236 键词表 +
  Lang/Tr/interpolate + 190 处迁移，162 测试；`[ui] lang` / `DEEPSEEKNOVA_LANG`
  接线）。~~（2026-08-07 曾因 API 402 中断，重启后复用备份完成）~~
- ~~**Tauri 桌面壳降为 P2**~~（2026-08-08 已撤销：桌面端整体移除，见本文件"观测台
  前端 UI + TUI 演进轮"节标注）。

### 调研核实的 P0 发布阻塞（未裁决，按路线图待做）
- 版本 bump 重发 crates.io（scanner/graph/metrics 三 crate 缺 description 不可发布）。
- ~~API key 环境变量约定失效（代码读 `DEEPSEEK_API_KEY`，README 推荐
  `DEEPSEEKNOVA_API_KEY`）~~ **已做（2026-08-07 第二批 B2 文档侧**：README/README_EN
  统一到代码默认 `DEEPSEEK_API_KEY`；命名最终裁决仍在"待领导裁决"）。
- README 截图空占位。
- ~~`/dev/tcp` 只读分类器漏网（P0-4）~~ **已做（第一批 B1**：Dangerous 硬拒 + 回归测试）。
- ~~serve CORS `allow_origin(Any)`（P0-5）~~ **已做（2026-08-07 第二批 B2**：收窄为
  loopback-only + 恶意 Origin 回归测试）。
- ~~done SSE 事件缺 session_id（P1-4）~~ **已做（2026-08-07 第二批 B2**：`WireEvent::Done`
  增 `session_id`，serve 注入 durable run id / 会话 id）。
- ~~审计日志持久化（P1-10）~~ **部分已做**：security 侧 `JsonlAuditLogger`（第一批 B1）
  + runtime 侧切 JSONL 后端（第二批 B3）；`PermissionGate` 裁决统一进审计流仍未做。
- SECURITY.md 支持版本表过期 + GitHub 元数据（description 仍写 Desktop）。

### 待领导后续裁决（非本批阻塞）
- Runtime/EventBus/ContextEngine 三选一（接线 / 标注库级 API / 删除）。
- ~~README"三层缓存"承诺（实现真实命中率 vs 撤稿）~~ **已按撤稿路线执行
  （2026-08-08 P2-6**：README/README_EN 改如实表述（API 级前缀缓存真实 +
  会话级命中率标注 [规划中]），core `session_cache_hit_tokens` 字段 doc 标注
  "当前恒 0、统计 [规划中]"、serde 契约保留，context builder 标注库级 API）。
- API key 命名统一到哪个变量名。
- npm 安装器承诺（cargo-dist vs 删死配置）。
- docs/superpowers 内部任务书（移出 / 标注 / 保留）。
- Windows 沙箱排期。
- temperature 配置接线 vs 删除。

## 本轮明确不做、留给领导裁决的顺手活
- ~~状态栏常驻成本显示~~（2026-08-02 已做：router ledger 每帧刷新）
- ~~多行输入框~~（2026-08-02 已做：Shift+Enter/Ctrl+J 换行）
- ~~MCP 实时连接状态探测~~（2026-08-02 已做：/mcp 短超时 spawn 探测）
- ~~桌面端样式（领导已搁置前端）~~（2026-08-04：desktop crate 已随 `3ab55d7` 移除，本项废止）
- ~~diff 高亮~~（2026-08-02 已做：行级 + 绿 / - 红 / @@ 青，未加 syntect 外部依赖）

## 执行阻塞

### ~~Graph Go 任务书（2026-08-05，G 域 worker 上报）~~ 已解除

- 阻塞项：`make check` 的 fmt 阶段被并行 worker（protocol 域）未 fmt 的
  `crates/deepseeknova-runtime/src/lib.rs` 卡住，G 域按任务书 §7 上报未触碰对方文件。
- ~~解除~~（2026-08-05）：P 域 worker 提交 95f695e 后全量 `make check` EXIT=0；
  后续两轮修复（7f49ffc / 4d47a6a）均全绿。属并行 worker 半成品阻塞的先例，已按
  AGENTS.md §5 归档。

## CodeGraph 增强任务书（2026-08-02）

执行阻塞：无（任务 1–5 全部落地，见 PROGRESS.md 任务状态；无待裁决项）

## Context7 任务书（2026-08-02）

执行阻塞：无。基线说明：开工时 PR #54 未合入 main，tools 实际基线 45+12+7（任务书
写的 50+12+7 含 PR #54 新增测试）；两分支存在重叠文件（runtime/GUIDE/CHANGELOG/
PROGRESS/BLOCKED），本分支已合并 main 解决冲突并重验全绿；#54、#55 均已合入。

## 代码库智能任务书（2026-08-02）

执行阻塞：无。基线说明：书里 tools=50+12+7 以 PR #54 分支测得，main 合入 PR #55 后
实测 59+12+7，硬指标 ≥50+12+7 满足；工作树有无关未跟踪文件 codex_desktop_ui.html
（非本书产物，未动）。顺手活（待裁决，不做）：~~Go 等新语言~~（2026-08-05 已做：
feat/memory-lifecycle 285fe60，tree-sitter-go 解析 + go.mod 外部依赖）、AST 全量
持久化、MCP 外壳、~~语义检索~~（2026-08-05 已做记忆侧 remote，见长期记忆 LLM
蒸馏节；代码图侧语义检索仍待裁决）。

## 后端审计分级清单（2026-08-01，详见 BACKEND_AUDIT.md）

**建议（不阻塞）**
- ~~README tests 徽章 536 落后实际 638（README.md:44）~~（2026-08-04：徽章已更新为 786）
- ~~README「44 个 Tauri 命令」vs 实测 61 个 `#[tauri::command]` 标记~~（2026-08-04：desktop 已移除，本项废止）
- graph `self_index` 与 provider `deepseek_reasoning_protocol` 集成测试为 ignored（既有，不在白名单）
- ~~desktop 不在 `make check`，本机完整校验需 `make check-desktop`（需前端产物）~~（2026-08-04：desktop 已移除，本项废止）

**顺手活（不做，待裁决）**
- ~~verify LLM 化~~（2026-08-02 已做：`[verify] llm = true`，默认关）
- ~~desktop 设置页 system_prompt 入口接新默认值（前端已搁置）~~（2026-08-04：desktop 已移除，本项废止）

## 长期记忆 LLM 蒸馏任务书（2026-08-02）

执行阻塞：无。说明：main 上 review::extract_json 为私有函数，改可见性不在白名单，
按「建议有更好的路」在 memory_distill.rs 自带等价实现（已记 PROGRESS）。顺手活
（待裁决，不做）：~~语义检索 embedder~~（2026-08-05 已做记忆侧 remote：写入即嵌入 +
`memory embed-backfill` 回填 + hybrid 检索，fail-open；local 后端仍待裁决）、记忆
清理 UI、蒸馏结果写 agentskills.io skill 文件。
## 反思→修复闭环任务书（2026-08-02）

执行阻塞：无。说明：main 上 review::extract_json 私有，reflection.rs 自带等价实现
（同第二本先例，已记 PROGRESS）；record_reflection_lesson 与 PR #58 的
record_llm_knowledge 并存，合入后可统一（待裁决）。顺手活（待裁决，不做）：反思 UI
展示、教训分级衰减、多模型反思对比。
## 记忆生命周期闭环任务书（2026-08-05）

执行阻塞：无。说明：本轮单域做透=记忆生命周期，以下三域写入待裁决留待下轮：
~~① protocol 增强~~（2026-08-05 已做：feat/memory-lifecycle 95f695e，task_rate 指标
first_pass/retry_rounds 落地 + fitness record_use 回填接线，两未落地项收尾）；
~~② graph 新语言支持~~（2026-08-05 已做：285fe60 Go 语言 + 7f49ffc/4d47a6a 修复轮）；
③ agent_loop 反思 UI（待裁决）。遗留→已修（review-fix C3）：runtime 起点/mid-run
召回与 tools recall 工具已全部接线 `rank_lifecycle_weight`（见 crates/REVIEW.md 第二轮复核）。

## Protocol 增强收尾 + Graph Go 任务书（2026-08-05）

执行阻塞：无。说明：protocol 域设计 §11 范围外项与偏差记录中其余未落地项写入待裁决：
① TUI 完整协议状态面板（现为最小文本段渲染，apply.rs:141-164）；② 主循环结构化计划
载体（drift 检测现为失败路径版）；③ 多模型反思对比；④ 记忆清理 UI（BLOCKED 蒸馏节
遗留）。

## 记忆语义检索任务书（2026-08-05）

执行阻塞：无。说明：记忆侧 remote 语义检索已做透（写入即嵌入 + `memory
embed-backfill` 回填 + hybrid 检索 `0.5*bm25 + 0.5*余弦 - 生命周期惩罚`，fail-open，
零新增依赖），审查修复后全绿。留待裁决：① local 嵌入后端（当前配置会显式 warn
回落 FTS）；② 代码图侧语义检索（graph 向量化）；③ RemoteEmbedder 改 async trait
+ spawn_blocking（消除同步 block_on 阻塞 worker 线程，有超时兜底、非阻塞）。

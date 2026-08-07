# BLOCKED — 待裁决清单

## 观测台前端 UI + TUI 演进轮（2026-08-07）

### 本轮明确不做、留给领导裁决
- **Tauri 壳（P1）**：`crates/deepseeknova-desktop` 本轮只有纯前端
  （Vite + SolidJS + Tailwind 4，非 cargo crate）；`src-tauri` 壳 + serve
  sidecar 托管 + 随机 bearer token 注入留待下轮。
- **桌面端后续页（P3/P4）**：星座图点星跳条目交互、审批卡、归档/诊断/聚合/
  设置/onboarding、印刷星图浅色档，均待前端工程进入下轮后按规范分期实现。
- **界面文案语言**：未决（现状中文；i18n 双语 / 全英留待拍板）。
- **Logo/应用图标**：无现存资产，实现期先用文字标。
- **风险标签接线端到端测试**：`responder.request` 收到带 `[风险:…]` 前缀描述
  的 agent 集成断言未补（本轮有纯函数 + 权限分类 + TUI 渲染测试覆盖）。
- **`crates/deepseeknova-tui/src/repro_tmp.rs`**：未跟踪的用户调试文件，非本轮
  产物，保留不提交；是否删除由用户决定。
- **`.impeccable/mocks/obs-comp-d-combined-agnes-v2.png`**：被否决的 Agnes 文生图
  备份，删除候选（未确认前不删）。

### 执行阻塞
- 无（本轮无阻塞；全量验证以 `make check` EXIT=0 为准）。

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

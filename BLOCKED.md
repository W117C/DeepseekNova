# BLOCKED — 待裁决清单

## 本轮明确不做、留给领导裁决的顺手活
- ~~状态栏常驻成本显示~~（2026-08-02 已做：router ledger 每帧刷新）
- ~~多行输入框~~（2026-08-02 已做：Shift+Enter/Ctrl+J 换行）
- ~~MCP 实时连接状态探测~~（2026-08-02 已做：/mcp 短超时 spawn 探测）
- ~~桌面端样式（领导已搁置前端）~~（2026-08-04：desktop crate 已随 `3ab55d7` 移除，本项废止）
- ~~diff 高亮~~（2026-08-02 已做：行级 + 绿 / - 红 / @@ 青，未加 syntect 外部依赖）

## 执行阻塞

无

## CodeGraph 增强任务书（2026-08-02）

执行阻塞：无（任务 1–5 全部落地，见 PROGRESS.md 任务状态；无待裁决项）

## Context7 任务书（2026-08-02）

执行阻塞：无。基线说明：开工时 PR #54 未合入 main，tools 实际基线 45+12+7（任务书
写的 50+12+7 含 PR #54 新增测试）；两分支存在重叠文件（runtime/GUIDE/CHANGELOG/
PROGRESS/BLOCKED），本分支已合并 main 解决冲突并重验全绿；#54、#55 均已合入。

## 代码库智能任务书（2026-08-02）

执行阻塞：无。基线说明：书里 tools=50+12+7 以 PR #54 分支测得，main 合入 PR #55 后
实测 59+12+7，硬指标 ≥50+12+7 满足；工作树有无关未跟踪文件 codex_desktop_ui.html
（非本书产物，未动）。顺手活（待裁决，不做）：Go 等新语言、AST 全量持久化、MCP 外壳、
语义检索。

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
（待裁决，不做）：语义检索 embedder、记忆清理 UI、蒸馏结果写 agentskills.io skill 文件。
## 反思→修复闭环任务书（2026-08-02）

执行阻塞：无。说明：main 上 review::extract_json 私有，reflection.rs 自带等价实现
（同第二本先例，已记 PROGRESS）；record_reflection_lesson 与 PR #58 的
record_llm_knowledge 并存，合入后可统一（待裁决）。顺手活（待裁决，不做）：反思 UI
展示、教训分级衰减、多模型反思对比。
## 记忆生命周期闭环任务书（2026-08-05）

执行阻塞：无。说明：本轮单域做透=记忆生命周期，以下三域写入待裁决留待下轮：
① protocol 增强；② graph 新语言支持；③ agent_loop 反思 UI。遗留→已修（review-fix
C3）：runtime 起点/mid-run 召回与 tools recall 工具已全部接线
`rank_lifecycle_weight`（见 crates/REVIEW.md 第二轮复核）。

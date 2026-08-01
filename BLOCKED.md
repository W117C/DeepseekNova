# BLOCKED — 待裁决清单

## 本轮明确不做、留给领导裁决的顺手活
- 状态栏常驻成本显示（当前只有 /cost 命令）
- 多行输入框（当前单行 + 横向跟随）
- MCP 实时连接状态探测（当前只列已启用 server 名）
- 桌面端样式（领导已搁置前端）
- diff 高亮（Codex CLI 用 syntect，按任务书不加外部依赖，留待裁决）

## 执行阻塞

无

## CodeGraph 增强任务书（2026-08-02）

执行阻塞：无（任务 1–5 全部落地，见 PROGRESS.md 任务状态；无待裁决项）

## Context7 任务书（2026-08-02）

执行阻塞：无。基线说明：开工时 PR #54 未合入 main，tools 实际基线 45+12+7（任务书
写的 50+12+7 含 PR #54 新增测试）；两分支存在重叠文件（runtime/GUIDE/CHANGELOG/
PROGRESS/BLOCKED），本分支已合并 main 解决冲突并重验全绿；#54、#55 均已合入。

## 后端审计分级清单（2026-08-01，详见 BACKEND_AUDIT.md）

**建议（不阻塞）**
- README tests 徽章 536 落后实际 638（README.md:44）
- README「44 个 Tauri 命令」vs 实测 61 个 `#[tauri::command]` 标记
- graph `self_index` 与 provider `deepseek_reasoning_protocol` 集成测试为 ignored（既有，不在白名单）
- desktop 不在 `make check`，本机完整校验需 `make check-desktop`（需前端产物）

**顺手活（不做，待裁决）**
- verify LLM 化（当前为确定性命令 + 固定回炉文案，agent.rs:933）
- desktop 设置页 system_prompt 入口接新默认值（前端已搁置）

## 反思→修复闭环任务书（2026-08-02）

执行阻塞：无。说明：main 上 review::extract_json 私有，reflection.rs 自带等价实现
（同第二本先例，已记 PROGRESS）；record_reflection_lesson 与 PR #58 的
record_llm_knowledge 并存，合入后可统一（待裁决）。顺手活（待裁决，不做）：反思 UI
展示、教训分级衰减、多模型反思对比。

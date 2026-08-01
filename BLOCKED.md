# BLOCKED — 待裁决清单

## 本轮明确不做、留给领导裁决的顺手活
- 状态栏常驻成本显示（当前只有 /cost 命令）
- 多行输入框（当前单行 + 横向跟随）
- MCP 实时连接状态探测（当前只列已启用 server 名）
- 桌面端样式（领导已搁置前端）
- diff 高亮（Codex CLI 用 syntect，按任务书不加外部依赖，留待裁决）

## 执行阻塞

无

## Context7 任务书（2026-08-02）

执行阻塞：无。基线说明：CodeGraph 增强 PR #54 尚未合入 main，tools 实测基线为
45+12+7（任务书写的 50+12+7 含 PR #54 新增的 5 条测试）；本任务与 PR #54 改动文件
互不重叠，按实际基线 45+12+7 继续，PR #54 合入后重跑仍应全绿。

## 后端审计分级清单（2026-08-01，详见 BACKEND_AUDIT.md）

**建议（不阻塞）**
- README tests 徽章 536 落后实际 638（README.md:44）
- README「44 个 Tauri 命令」vs 实测 61 个 `#[tauri::command]` 标记
- graph `self_index` 与 provider `deepseek_reasoning_protocol` 集成测试为 ignored（既有，不在白名单）
- desktop 不在 `make check`，本机完整校验需 `make check-desktop`（需前端产物）

**顺手活（不做，待裁决）**
- verify LLM 化（当前为确定性命令 + 固定回炉文案，agent.rs:933）
- desktop 设置页 system_prompt 入口接新默认值（前端已搁置）

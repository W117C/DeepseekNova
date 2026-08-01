# 会话证据消费盲区声明（仅供 harness 评审使用，不影响构建/运行时）

> 本文件是本项目 harness 评审的会话证据边界声明。任何消费
> `session-analysis facts` 输出（尤其 `populationCoverage` 的
> `withChanges` / `withChecks`）的评审结论，都必须先执行下述协议并
> 引用其结果作为边界声明。

## 强制预检协议（每次 harness 评审前执行）

1. **源根状态确认**：运行
   `session-analysis sources --platform qoder --workspace <本项目> --format json`，
   记录各源根的 `exists` / `enabled` 状态与全部 `warnings`。
2. **嵌套结构核对**：从
   `~/.qoder/projects/<workspace-slug>/transcript/` 抽取一条已知含编辑
   行为的近期会话，逐行解析 JSONL，核对工具事件位于顶层 `tool_name`
   字段还是嵌套在 `message.content[].tool_use` 内，并记录两类计数。
3. 将 1、2 的结果作为该评审窗口会话结论的**证据边界声明**写入报告。

## 结论禁区

- **不得**据 `withChanges=0` / `withChecks=0` 断言窗口内"无编辑行为"或
  "无验证行为"。零覆盖率在本项目属**解析盲区**而非行为事实：facts 的
  `inferToolName` 只读顶层 `tool_name` 字段（仅 Skill 有嵌套特判），而
  本项目 transcript 把全部工具事件嵌套在 `message.content[].tool_use`
  内，编辑/检查事件对分析器不可见。
- `--include-cache` / `--include-global-capabilities` 只控制 cache 与
  global 源根的启用，**不能**恢复变更计数；缺失的可选源根
  （audit.jsonl、logs/sessions 工作区目录、home sessions）属产品未写入，
  无法通过配置恢复。

## 当前边界声明（核对于 2026-07-29）

### 源根状态（sources 路由输出摘要）

| 源根 | exists | enabled | 说明 |
|------|--------|---------|------|
| qoder-audit (audit.jsonl) | ✗ | ✓ | missing-optional-root，产品未写入 |
| qoder-run-manifests | ✓ | ✓ | 可用 |
| qoder-log-sessions（工作区） | ✗ | ✓ | missing-optional-root，产品未写入 |
| qoder-projects (transcript) | ✓ | ✓ | 唯一实际会话事实源 |
| qoder-home-sessions | ✗ | ✓ | missing-optional-root |
| qoder-cache-projects | ✓ | ✗ | 默认禁用，需 --include-cache |
| qoder-global-projects | ✓ | ✗ | 默认禁用，需 --include-global-capabilities |

### 嵌套结构核对（抽样会话逐行解析结果）

- 抽样：`task-4c570761e83541de9877.session.execution.jsonl`（641 行，
  已知含真实编辑行为）
- 顶层 `tool_name` 字段事件数：**0**
- 嵌套 `message.content[].tool_use` 事件数：**193**，其中编辑类
  `SearchReplace` 13 次、`Write` 10 次，另含 `Bash` 26 次等验证类事件
- 结论：本项目 transcript 工具事件**全部为嵌套结构**，facts 的
  `withChanges` / `withChecks` 零值不可作为行为结论；该状态持续有效，
  直至上游分析器支持嵌套解析并经重新核对推翻本声明。

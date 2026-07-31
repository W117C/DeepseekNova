# deepseeknova-scanner

deepsec 式安全扫描（P1）：regex matcher 扫描 → 每 finding 一次性 agent 调查 → 报表。

- `rule` — 内置 matcher 规则集
- `scan` — 文件遍历 + 逐行匹配（零 AI）
- `investigate` — 一次性 agent 调查裁定真伪
- `report` — severity 分组 + md/json 渲染

CLI 入口：`deepseeknova scan`。P2 规划：triage / revalidate。

## 已知限制（P1）

- 调查 agent 会注册已配置的 MCP 工具（最小装配之外）；非交互下 permission gate 的 Ask 回落 Allow——威胁模型为本地自扫
- `--severity-min`/`--format` 非法值静默回落默认（low/md）
- markdown 报表对 excerpt/note 不做转义；excerpt 原样注入调查 prompt（本地工具，风险可接受）
- 单文件跳过（权限/非 UTF-8/超大）不计数，报表无 skipped 统计

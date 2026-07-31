# deepseeknova-scanner

deepsec 式安全扫描（P1）：regex matcher 扫描 → 每 finding 一次性 agent 调查 → 报表。

- `rule` — 内置 matcher 规则集
- `scan` — 文件遍历 + 逐行匹配（零 AI）
- `investigate` — 一次性 agent 调查裁定真伪
- `report` — severity 分组 + md/json 渲染

CLI 入口：`deepseeknova scan`。P2 规划：triage / revalidate。

# DeepseekNova Eval 基准任务集

对标 eval 基准：任务分层覆盖，用于量化优化前后通过率 / 综合分 / 成本 / 轮数 / 缓存命中率。
运行：`deepseeknova-cli eval --path evals/core.jsonl --require-min-score 3.5`（或 `make eval-ci`）。

## 分层说明

- `core.jsonl` — 核心面（单文件修改 / 调试 / 验证闭环），CI 门禁默认跑此集。
- `advanced.jsonl` — 进阶面（多文件重构 / 跨语言 / 长会话 / 压缩边界），**规划中，尚未落地**（暂无该文件，每周跑计划未执行）。
- 每层预留 20% 盲测用例（`blind/`），优化迭代时避免过拟合。**盲测集为规划中设计，尚未落地**（`blind/` 目录不存在）。

## 用例字段

```json
{"name":"...","prompt":"...","must_contain":["..."],"min_score":0.8,
 "dimension_min":{"governance":0.9},"cost_max":0.05,"rounds":3}
```

- `min_score`：评分卡综合分阈值（0..1 或 0..5 均支持）。
- `cost_max`：累计成本上限（USD）。
- `rounds`：重试轮次上限。
- `cache_hit_rate_min`：前缀缓存命中率下限（仅对缓存端点生效；无缓存记账时跳过）。

## 结果归档

`evals/results/` 存放每次运行报告（含 token/命中率/轮数），命名 `<date>-<tag>.json`——**规划中，尚未落地**。

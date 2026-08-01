# Extreme Token Savings C1 — 极致 Token 节省一期设计

- 日期：2026-08-01
- 状态：已评审（用户逐项决策确认 + 设计整体批准）
- 战略支柱：③ 极致 Token 节省（对标 Codex / Claude Code：写代码、检查代码、开发项目全程最小 token）
- 目标 crate：`deepseeknova-tools` / `deepseeknova-runtime` / `deepseeknova-config`（增量，零新 crate）
- 基线：main @ `84652a1`（B0–B3 + 模型指针/成本分账/Compact 计量统一均已合入）

## 1. 背景：读码盘点后的真实浪费点

立项时设想的四个方向经全仓读码盘点后收敛：语义压缩已由 B2 交付（L1/L2/L3+熔断）、
成本分账已由模型指针工作线交付（ModelRouter/CostLedger/MeteredProvider）、缓存纪律
已有（CacheAwarePromptBuilder 稳定前缀 + SHA256 稳定性追踪）。剩余浪费点排名：

| # | 浪费点 | 影响 | C1 处置 |
|---|---|---|---|
| 1 | 无 diff/patch 编辑——`write_file` 发整文件，`edit_file` 单块且须先读全文 | **CRITICAL**（大文件一次编辑 ≈3000+ token 白烧） | ✅ 本期 |
| 2 | `compaction_threshold_tokens` 默认 None——L1/L2/L3 平时不触发，只靠 budget 兜底 | HIGH | ✅ 本期 |
| 3 | 工具 schema 无瘦身——稳定前缀内 20+ 工具每次缓存 MISS 全额重付（5–10K token） | HIGH | ✅ 本期 |
| 4 | reasoning_effort 不随任务复杂度自适应 | MEDIUM | C2 候选 |
| 5 | 桌面端 compact_model 绕过 ModelRouter（丢计量） | MEDIUM | C2 候选 |
| 6 | `read_file` 无区间读 | MEDIUM | ✅ 并入 #1 |
| 7 | repo map 预算固定 1024 | LOW | C2 候选 |
| 8 | full_results 易失（crash/resume 丢失） | LOW | C2 候选 |

## 2. 已锁定的决策

| 决策点 | 结论 | 依据 |
|---|---|---|
| 首期切片 | 前三大浪费点（#1+#6、#2、#3），其余记录后置 | 直击最大收益，一期可验收 |
| 编辑形态 | **多块 search/replace**（升级现有 `edit_file`）+ `read_file` 区间读；不做 unified-diff 工具 | Aider/Claude Code 实证：search/replace 对 LLM 最鲁棒（不依赖行号）；diff 行号易错、重试反费 token；双工具并存与 #3 瘦身相悖 |
| 阈值默认策略 | `None` 且 `budget.enabled` 时运行时推导 `budget.max_total_tokens / 2`（默认 128K→64K）；显式配置优先；budget 关则维持 None | 单一真相源跟随 budget；小窗口自动缩；L1 无损先行（全文在 side-band 可经 fetch_full_result 取回） |
| schema 瘦身路线 | 文案压缩（目标总体积 **-40%**）+ 体积回归测试（总字符数预算上限）；不做动态选拨 | 动态选拨改变工具集=前缀变化伤缓存命中、还需任务分类器；文案压缩零行为风险、收益永久 |

## 3. 设计明细

### 3.1 多块编辑（`edit_file` 就地升级，工具名不变）

- 参数扩展：接受 `edits: [{search, replace}, …]`（沿用现有字段名）；向后兼容单块调用
  形式（顶层 `search`/`replace` 仍接受，实现取 serde 最简方案）。
- **全有或全无**：任一块匹配失败（0 命中）或歧义（≥2 命中）→ 整次调用失败，
  错误信息指明第几块、失败原因与命中数；不产生半改状态。
- **行为收紧（显式声明）**：现行单块语义是“替换首个匹配”；C1 后单块调用同样改为
  “唯一匹配否则报错”（与多块规则一致，确定性优先；错误信息引导模型补上下文重试）。
  属工具语义微型 breaking，CHANGELOG 标注。
- 块按文件内出现顺序应用；块间目标区域重叠 → 报错。
- 沿用现有 snippet staleness 验证 + checkpoint 回滚，语义不变。

### 3.2 区间读（`read_file` 加可选参数）

- 新增 `start_line`/`end_line`（1-based 闭区间，均可选；缺省=现行为，1MB 上限不变）。
- 区间内容带行号前缀与范围标注；区间读同样注册 snippet（staleness 验证覆盖片段编辑）。
- 越界处理：start 超文件尾 → 报错；end 超尾 → 截到文件尾（宽松）。
- schema 描述内置省 token 引导：「大文件先 grep/search_code 定位 → 区间读 → 多块编辑」。

### 3.3 压缩阈值默认接通（runtime 装配处）

- `build_agent` 装配：`config.agent.compaction_threshold_tokens` 为 `None` 且
  `config.budget.enabled` → 传 `budget.max_total_tokens / 2`；显式 Some(N) → N；
  budget 关闭 → 维持 None（现状）。
- 非 breaking：仅让既有无损 L1 先行生效；L2/L3 链条、熔断、`l3_compaction` 开关全部不变。
- 配置注释 + CHANGELOG 说明推导规则。

### 3.4 schema 文案压缩 + 防膨胀（tools crate）

- 逐个审校内置工具（fs/grep/shell/memory/web_fetch/todo/graph/delegate）的
  description 与参数描述：删冗余、并重复、统一简洁风格；**压缩废话，保留行为引导**
  （如 3.2 的定位-区间读-多块编辑引导属于「信息」，保留并强化）。
- 回归测试：`all_builtin_tools()` 全量 schema 序列化总字符数 ≤ 预算常量
  （压缩后实测值上浮 ~10% 定值）；超预算即红——未来新工具/改文案强制自省。
- 已知代价：schema 属稳定前缀，本次改动一次性击穿现有缓存（不可避免，收益永久）。

## 4. 验收

1. 多块编辑：一次调用替换同文件 3 处，全成功；1 处歧义 → 全次失败且错误指明块号；与单块调用兼容。
2. 区间读：读 500 行文件的 L100–L140，返回仅该区间（带行号）；基于区间 snippet 的编辑通过 staleness 验证。
3. 阈值推导：默认配置下 Agent 实际拿到 64K 阈值（单测），显式配置与 budget 关闭路径各自正确。
4. schema 体积：改前/改后总字符数写入 PR 描述，降幅 ≥40%；回归测试上限生效。
5. 模拟任务对比：编辑 500 行文件中 2 处的任务，token 消耗（读+写合计）对比改前下降一个数量级（估算基准写入 plan）。
6. `make check` + `make check-desktop` 全绿；全程零行为开关（三项均为默认改良），#2 有显式配置逃生舱。

## 5. 范围外（C2 候选，按 C1 实测数据决定）

自适应 reasoning_effort（#4）、桌面 compact 计量归位（#5）、repo map 自适应预算（#7）、
full_results 持久化（#8）。

## 6. 风险与边界

- 多块编辑失败语义偏严（全有或全无）：模型需重试整次调用——换来的是永不半改的确定性；
  错误信息按块定位将重试成本压到最低。
- 阈值推导让 L1 在长会话默认启动：截断只动 side-band 有全文备份的工具结果，且
  `fetch_full_result` 可取回；行为面温和。
- schema 压缩的缓存一次性击穿：与任何 schema 演进相同，属既有代价类别。

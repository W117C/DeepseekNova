# 多模型指针与成本分账设计（外部方案融入·一期）

- 日期：2026-07-29
- 状态：已确认（用户批准）
- 来源：融入 [shareAI-lab/Kode-CLI](https://github.com/shareAI-lab/Kode-CLI) 的 ModelManager
  设计与 [vercel-labs/deepsec](https://github.com/vercel-labs/deepsec) 的分级模型调度理念

## 背景与两期路线

Kode-CLI 的核心设计是按角色分配模型（main/task/compact/quick 四指针）并按模型分账成本；
deepsec 的核心设计是「正则 matcher → AI 深度调查 → 廉价模型 triage → revalidate 复核」的
安全扫描流水线，其分级调度天然依赖模型角色能力。因此分两期融入：

- **一期（本 spec）**：模型指针体系 + 成本分账，改造 config / provider / agent / cli 四层
- **二期（独立 spec，暂缓）**：deepsec 式安全扫描流水线，落在 `deepseeknova-security` 之上，
  复用 `deepseeknova-checkpoint`（可恢复执行）与 `deepseeknova-sandbox`（隔离执行），
  triage 用 `quick` 指针、深度调查用 `task` 指针
- **明确暂缓项**：Markdown 声明式代理模板（`.deepseeknova/agents/*.md`）、
  `@ask-*` / `@run-agent-*` 提及语法、AGENTS.md 目录级联发现

## 方案选型

| 方案 | 结论 |
| --- | --- |
| A：provider 层 ModelRouter + 配置指针（**选定**） | 不新增 crate，复用 `resolve_provider_for_model` 解析链，为二期铺路 |
| B：新建 deepseeknova-model-manager crate | 边界清晰但一期体量偏重，且有 manager↔provider 循环依赖风险 |
| C：仅给 SubAgentConfig 加 model 字段 | 无角色语义，二期需重做，弃 |

## 配置层（deepseeknova-config）

新增 `[model_pointers]` 段：

```toml
[model_pointers]
main = "deepseek-v4"          # 主对话
task = "deepseek-v4-flash"    # 子代理/委派
compact = "deepseek-v4-flash" # 历史压缩
quick = "deepseek-v4-flash"   # 快速操作（标题生成、分类等）
```

`ModelConfig` 增加可选单价字段（单位 $/1M tokens，`Option<f64>`）：

- `input_price_per_mtok`
- `output_price_per_mtok`
- `cache_hit_price_per_mtok`

规则：

1. 四个指针全部可选；未配置角色回落 `main`；`main` 未配置回落现行默认解析逻辑
   （零配置时行为与现状完全一致，向后兼容）
2. **加载期校验**：指针引用的模型名必须存在于 `[[models]]`，否则 `Config::load`
   报错并列出指针名与候选模型；负单价同样在加载期拒绝
3. 沿用现有 merge 语义：项目级 `deepseeknova.toml` 覆盖用户级 `~/.deepseeknova/config.toml`

## Provider 层（deepseeknova-provider）

### router.rs — ModelRouter

- `ModelRole` 枚举：`Main | Task | Compact | Quick`
- `ModelRouter::from_config(&Config) -> Result<Self>`：解析四指针为
  `(ModelConfig, ProviderConfig)` 对
- `provider_for(role) -> anyhow::Result<Arc<dyn Provider>>`：惰性构建 + 缓存；
  **缓存 key = (provider 名, 模型名)**，同 provider 不同模型互不串实例，
  同一模型被多角色引用时共享同一实例
- `set_pointer(role, model_name)`：会话内热切换（仅内存态，不写盘）
- factory 新增 `create_provider_with_model(cfg, model_name, task_classification)`
  作为模型名 override 入口；现有 `create_provider` / `create_provider_for_task`
  签名与行为不变

### cost.rs — CostLedger

- `record(role, model_name, &Usage)`：累加 prompt / completion / cache_hit /
  cache_miss / reasoning tokens（`Usage` 为 `deepseeknova-core::chunk::Usage`，
  字段已齐备，无需改流式协议）
- `record_unmetered(role, model_name)`：流中断未收到 Usage 时计一次未计量调用
- `report() -> CostReport`：按 模型×角色 汇总 token；模型配了单价则折算美元
  （cache_hit 按 cache 单价，未配 cache 单价时按 input 单价计），任一单价缺失
  该行仅显示 token，不报错
- 内部 `Mutex` 保证线程安全，以 `Arc<CostLedger>` 全链路共享

## Agent / CLI 接线

- `Agent` / `SubAgent` / delegate：构造时按角色注入 Provider——主循环用 `Main`、
  子代理循环用 `Task`、`compact_with_provider` 改用 `Compact`；各调用点将流末
  `Usage` 打上角色标记记入 ledger
- CLI `chat.rs`：
  - `/model`：显示四个指针当前指向；`/model use <role> <model>` 会话内热切换
  - 新增 `/cost`：打印 CostReport 表格（模型×角色×tokens×估算成本，
    含未计量调用计数）
- **不改动** `Provider` trait 与 `Chunk` 流协议

## 错误处理与边界

| 失效场景 | 处理 |
| --- | --- |
| 指针指向未定义模型 | config 加载期报错，含指针名与候选模型列表 |
| 角色模型的 API key 环境变量缺失 | 首次 `provider_for` 报错并提示所需 env 名，不影响其他角色 |
| 流中断未收到 Usage | 不估算，计入未计量调用数，`/cost` 中注明 |
| 单价表部分缺失 | 该模型仅显示 token，不显示美元，不报错 |
| 负单价 | 加载期拒绝 |

## 测试计划

- **config**：指针解析、缺省回落、悬空指针报错、单价字段反序列化；
  负例：负单价拒绝、指针指向不存在模型
- **router**：角色→模型解析、缓存 key 隔离（同 provider 两模型互不串）、
  未配置角色回落 main、热切换后新实例生效
- **ledger**：多角色并发记账、部分单价缺失的报告输出、未计量调用计数
- **agent**：以现有 `MockProvider` 基建验证子代理走 `Task`、压缩走 `Compact`
- **回归**：`make check` 全量通过（跨 crate 变更强制项）

## 假设与置信度

- 置信度：**高**
- 已验证假设：`ModelConfig` 为一等公民且有 `find_model` / `resolve_provider_for_model`；
  `Usage` 字段齐备且随流传递；factory 以模型名为构造参数、可加 override 入口；
  CLI 已有 `/model switch` 可扩展
- 残余风险（中）：delegate / coordinator 链路的 Provider 传递方式需在实现时按
  实际签名微调，属实现细节，不影响架构

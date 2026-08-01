# Closed-Loop Learning & Memory Engine — 闭环学习与记忆引擎设计

- 日期：2026-07-28
- 状态：已评审（用户逐段确认 + 反馈已并入）
- 战略支柱：② 记忆与自我进化（对标 Codex / Claude Code，"越用越聪明"）
- 目标 crate：`deepseeknova-embed`（新增，feature-gated）+ `deepseeknova-core` / `deepseeknova-tools` / `deepseeknova-agent` / `deepseeknova-runtime` / `deepseeknova-config` / `deepseeknova-cli`（增量）

## 1. 背景与根因

代码审阅证实：项目里存在**两套互不连通的记忆系统**，这正是 agent 目前"不会越用越聪明"的根因。

| 系统 | 位置 | 现状 |
|---|---|---|
| 模型侧记忆工具 | `deepseeknova-tools/src/memory.rs`：`remember` / `recall` / `forget` | 写入一个 `static OnceLock<Mutex<HashMap>>` 的**进程内易失存储**，进程重启即蒸发；自带一套私有 BM25；已注册进 `all_builtin_tools()`，模型天天在用 |
| 真正的持久引擎 | `deepseeknova-core/src/memory/`：FTS5 `MemoryStore`、`RecallEngine`、`SkillManager`、`UserProfile`、`DistillationEngine`、`lifecycle` | 实现完整、测试完备，但**在运行路径上无任何调用者**；且 `lifecycle`（Candidate→Verified→Permanent + 衰减）**未持久化**——FTS5 schema 里没有 `stage`/`recall_count` 列 |

结论：模型"记"进的是一个会蒸发的桶，而真正能让它变聪明的引擎无人调用。本设计的核心是把二者**合一**、**接线到每一次 run**、并补齐语义层与生命周期。

外部参照：Claude Code 的项目记忆 + 会话学习、Codex 的经验沉淀。借鉴其"召回入 / 沉淀出"的闭环，但用本项目已有的 FTS5 + lifecycle + skill 机制落地，语义层作为数据驱动的二期增强。

## 2. 决策记录

| 决策点 | 结论 |
|---|---|
| 野心边界 | 记忆层升级：接通闭环 + 向量/语义检索 + 分层归纳（分期落地） |
| 嵌入后端 | 可插拔 `Embedder` trait，本地/远程可切换，FTS5 始终为兜底，混合 RRF 排序 |
| 学习自主度 | 全自动沉淀 + 护栏（够格才触发、廉价模型、去重、衰减归档、一键关闭） |
| 召回注入 | 混合：起点注入 top-3 精简块（volatile 区，不破前缀缓存）+ 按需 `recall` 工具取全文 |
| 架构落点 | 核心内扩 + 新建 feature-gated `deepseeknova-embed`（本地重依赖隔离），照搬 GraphHandle 先例 |
| **redaction + 可审查性** | **`auto_learn=true` 的前置条件（P1 硬性交付），非 P3 锦上添花** |
| **P2 启动依据** | **由 P1 可观测性指标（召回命中率/reinforce 比例）数据驱动，非按计划表推进** |

被否方案：新建独立 `deepseeknova-memory` crate（迁移churn大）；嵌入后端全塞进 core（污染轻量地基、拖慢构建）；召回块注入 system prompt 稳定前缀（每轮击穿 DeepSeek-V4 前缀缓存）。

## 3. 架构与 crate 落点

| 组件 | 落点 | 说明 |
|---|---|---|
| `Embedder` trait | `deepseeknova-core::memory` | 轻量契约：`async fn embed(&[String])->Result<Vec<Vec<f32>>>`、`fn dim()->usize`、`fn id()->&str` |
| `LocalEmbedder` | **新建 `deepseeknova-embed`**（feature `local`，默认关闭） | candle/fastembed + bge-small 类小模型；重依赖（onnx/candle + 权重）隔离于此；CPU 推理走 `spawn_blocking` |
| `RemoteEmbedder` | `deepseeknova-provider` | 复用现有 HTTP 层调 OpenAI 兼容 `/embeddings` |
| `MemoryEngine`（统一门面） | `deepseeknova-core::memory` | 组合 `MemoryStore`(FTS5) + `Embedder`(可选) + `UserProfile` + `SkillManager` + `lifecycle`；对外暴露 `recall / remember / forget / distill / maintain / stats` |
| 装配与钩子 | `deepseeknova-runtime::build_agent` | 构造引擎，注入 `MemoryHandle` 到 ToolContext 扩展（照搬 GraphHandle）+ 起点召回注入 + 结束沉淀钩子 |
| Agent 钩子 | `deepseeknova-agent` | 新增 `RecallProvider` 闭包（仿 `repo_map_provider`）+ `DistillHook`；run 结束后非阻塞触发沉淀 |
| 工具改接 | `deepseeknova-tools/memory.rs` | `remember/recall/forget` 改用 ctx 注入的 `MemoryHandle`（打持久引擎 + 混合排序）；**删除**易失 static HashMap；句柄缺失时降级为友好文字提示 |
| CLI 审查入口 | `deepseeknova-cli` | `memory list/search/forget/stats`（P1）、`memory reembed`（P2） |
| 配置 | `deepseeknova-config` | 新增 `[memory]` 段（全 `#[serde(default)]`，对齐 `[graph]`） |

`MemoryHandle = Arc<MemoryEngine>`（引擎内部持有 `Arc<Mutex<rusqlite::Connection>>`，天然可跨 agent/子 agent 共享）。

## 4. 统一数据模型与 schema（`.deepseeknova/memory.db`）

```sql
-- 保留现有全文表
memory_fts USING fts5(content, tags, category, source,
  created_at UNINDEXED, importance UNINDEXED, id UNINDEXED,
  tokenize='porter unicode61');

-- 新增常规伴表：持久化 lifecycle + 向量
memory_meta(
  id               TEXT PRIMARY KEY,   -- 关联 memory_fts.id
  stage            TEXT NOT NULL,      -- candidate/verified/permanent/archived
  recall_count     INTEGER NOT NULL DEFAULT 0,
  last_recalled_at INTEGER,
  embedding        BLOB,               -- f32 小端；embedder=none 时为 NULL
  embed_dim        INTEGER,
  embed_model      TEXT
);

-- 沉淀成本硬上限计数
distill_log(day TEXT PRIMARY KEY, count INTEGER NOT NULL DEFAULT 0);
```

- 写入：`memory_fts` 与 `memory_meta` 在**同一事务**内写，保证一致。
- 向量检索：加载候选 embedding 在 Rust 内做 cosine（数千条足够毫秒级；HNSW 留 P2+ 后续）。
- **并发（新增，回应反馈）**：`PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000`；写路径事务化；引擎内 `Mutex<Connection>` 串行化进程内多 agent 写；WAL + busy_timeout 兜住 CLI/desktop 多进程并发。
- **embed_model 迁移策略（新增，回应反馈）**：切换 embedder 或维度不符时——召回阶段该候选**排除出向量打分**（仅贡献 FTS5 名次）并标记 stale；`maintain()` 每次机会式**懒惰重算**一小批 stale 条目（有界）；另提供 `memory reembed` 一次性 backfill。**不做阻塞式迁移**，换 embedder 永远安全。
- 损坏恢复：memory.db 含**唯一真相**（非派生），损坏不删库；schema 变更走 `IF NOT EXISTS` + 轻量版本列，向后兼容。

## 5. 数据流 —— 召回 IN（混合注入，保前缀缓存）

- **仅新会话起点**（`history` 为空时）：Agent 调用 `RecallProvider(首条 prompt)` → `MemoryEngine.recall()` 返回 top-3 极简行（匹配技能 + 高价值记忆 + 画像要点），组成 `## Recalled Context` 小块。
- **注入位置**：作为**独立消息**插入到稳定前缀（system prompt + 工具 + 项目记忆 + repo map）**之后**的 volatile 区，位于用户首条消息之前。→ 查询相关内容变化也**不改稳定前缀 hash，不击穿前缀缓存**；且每会话仅注入一次（非每轮）。
- **正反馈（"越用越聪明"的核心）**：每条被召回条目触发 `lifecycle.record_recall()`（`recall_count++`、更新 `last_recalled_at`、Candidate→Verified 迁移）。
- **深度按需**：`recall` 工具打持久引擎 + 混合排序，模型需要细节时自取全文 —— token 只在必要时花。

## 6. 数据流 —— 沉淀 OUT（全自动 + 护栏 + redaction + 成本上限）

- 时机：`run_agent_loop` 返回后，Agent 装配 `TaskObservation`（任务文本、工具调用计数、步数、结果 Success/Partial/Failure、错误文本），调用 `DistillHook`（**spawn 非阻塞**，绝不拖慢用户可见响应；失败仅 warn）。
- **触发护栏**：`tool_calls ≥ min_tool_calls(默认5) && steps ≥ min_steps(默认3)`；且**每日/每会话硬上限**（新增，回应反馈）`max_distillations_per_day`（查 `distill_log` 表）/ `max_distillations_per_session`（进程内计数器），超限即跳过——防高频小任务悄悄堆积成本；`auto_learn=false` 一键全关。
- **redaction（新增，前置硬需求，回应反馈）**：任何将写入持久库的内容（工具调用、错误信息、摘要）先过 `redact()` 脱敏——正则匹配常见密钥/token/`.env` KEY=VALUE/PEM 私钥块/高熵串 → 替换为 `[REDACTED:kind]`。同时对 `remember` 工具写入路径生效（纵深防御）。配置 `redact_secrets=true` 默认开。
- 沉淀 LLM：用**廉价模型/低 reasoning_effort**（`distill_model`）产出结构化 JSON（宽松解析，仿 coordinator 的 `extract_json_block`）：
  - 成功 → 合成可复用 **skill**（Markdown）入 `SkillManager`；
  - 失败/重试 → **经验教训**（什么没成/为何/下次怎么做）入 Skill 类记忆，标签 `["failure","lesson"]`——即"自我总结错误形成经验积累"；
  - 画像观察 → `UserProfile.observe`；任务摘要 → Task 记忆；来源统一标 `source="auto-distill"` 便于审查与统计。
- **去重（防记忆污染）**：写入前 `recall(item, k=3)`，相似度 ≥ `dedup_threshold` → 改为 `reinforce` 已有条目（importance 提升 + 生命周期），不新插。

## 7. 语义层 Embedder（P2，数据驱动启动）

- `trait Embedder`（core）：三态选择 `none`（纯 FTS5，仍完整可用）/ `local`（`deepseeknova-embed`）/ `remote`（provider）。
- 写入时算 embedding 存 `memory_meta`；召回时查询嵌入一次。
- **混合排序 RRF**：对 FTS5 BM25 名次与 cosine 名次做 Reciprocal Rank Fusion，再乘 lifecycle-stage/importance 权重，取 top-K。
- 启动依据见 §10：只有当 P1 指标显示关键词召回不够准时才上 P2。

## 8. 生命周期与分层归纳（P3）

- `maintain()`（run 结束机会式触发，限频，如每日或每 N 次运行）：
  - **衰减 + 归档**：对非 permanent 记忆按 `daily_decay_rate` 衰减；跌破阈值归档（复用 `lifecycle.rs` 既有逻辑，只补持久化）。
  - **分层归纳（记忆层升级）**：按 embedding 聚类相关 Skill/Task 记忆；簇过大时用廉价 LLM 产出一条更高层"洞见"记忆（高 importance）并归档冗余叶子——让库随时间**变精、变省 token**。
  - 顺带懒惰 re-embed 一小批 stale 条目（见 §4 迁移策略）。

## 9. 可审查性与手动控制（P1 前置，回应反馈）

全自动学习必须让用户能"看见并撤销被学到的东西"，否则无法建立信任。P1 即交付 CLI 入口（桌面 UI 属支柱⑤，后续接同一引擎）：

- `memory list [--category] [--limit]` — 列出记忆（含 stage/recall_count/source）
- `memory search <query>` — 混合检索预览
- `memory forget <id>` — 手动删除
- `memory stats` — 输出 §10 可观测性指标
- （P2）`memory reembed` — 向量 backfill

## 10. 可观测性与 P2 数据驱动门槛（P1 前置，回应反馈）

P1 加入**轻量计数**（非新功能，几行 tracing + `memory_meta` 聚合查询）：

- **召回命中率** = `recall_nonempty / recall_calls`
- **每条召回增长** = `recall_count` 分布
- **reinforce 比例** = `source="auto-distill"` 条目中达到 Verified（`recall_count≥1`）的占比

**P2 启动准则（给出初始阈值，可据实调整）**：P1 上线累计 ≥ 200 次召回后，若**命中率 < 60%** **或** **reinforce 比例 < 25%**（说明关键词召回不够准）→ 才启动 P2 语义层，并据指标选 local/remote embedder；否则维持 FTS5，把资源留给其它支柱。阈值本身也作为 §10 stats 的观测项持续校准。

## 11. Token 护栏（对齐支柱③）

召回注入块封顶 `recall_inject_tokens`（默认 ~200，top-3）且在 volatile 区、每会话一次 → 不破缓存；深度走按需工具；沉淀在廉价模型、离响应路径、去重防膨胀、每日成本硬上限；归纳持续压缩库；全部受配置预算 + 双 kill-switch（`auto_learn` / `embedder`）控制。

## 12. 配置（`deepseeknova-config` `[memory]` 新节，全 `#[serde(default)]`）

```toml
[memory]
enabled = true                    # false 时零开销，行为=现状
db_path = ".deepseeknova/memory.db"
auto_learn = true                 # 全自动沉淀；依赖 redact_secrets + 可审查性（前置条件）
redact_secrets = true             # 写入前脱敏（auto_learn 的硬前提）
embedder = "none"                 # none | local | remote（P2 起用）
embed_model = ""                  # local: 模型名；remote: embeddings 模型
recall_inject_tokens = 200        # 起点注入块封顶；0 = 不注入，仅保留按需工具
recall_top_k = 3
min_tool_calls = 5                # 沉淀触发门槛
min_steps = 3
max_distillations_per_day = 50    # 成本硬上限
max_distillations_per_session = 10
distill_model = ""                # 空 = 复用廉价档模型
dedup_threshold = 0.85
daily_decay_rate = 0.02
consolidate_every_runs = 100      # 分层归纳触发频率（P3）
```

> 显式声明：`auto_learn=true` 依赖 `redact_secrets` + §9 可审查性，二者是 **P1 前置条件**而非 P3 锦上添花。

## 13. 错误处理与降级（thiserror 惯例）

- 新增 `MemoryError`（core）：`Storage(#[from] rusqlite::Error)` / `Embed(String)` / `Distill(String)` / `NotFound(String)`。
- **全链路优雅降级**：embedder 失败 → 回退 FTS5-only；DB 打开/写失败 → 本轮禁用记忆并 `warn`，run 照常；沉淀/归纳失败 → 仅 `warn`；工具层错误全转模型友好文字，绝不阻断/崩溃 run（照搬 graph 姿态）。

## 14. 测试计划

- **单元**：混合 RRF 排序；`memory_meta` 持久化 + lifecycle 迁移（Candidate→Verified→Permanent、decay、archive、reinforce）；去重（reinforce 而非重插）；cosine；沉淀 JSON 宽松解析；护栏门控（含每日/每会话上限）；redaction（各类密钥/token 命中脱敏、误伤最小）；召回注入 token 封顶。
- **集成**：① "存 → 重启 → 仍能召回"（修复蒸发 bug 的验收）；② run 结束沉淀写出 skill + lesson，来源标记正确；③ 召回块进 volatile 区、**不改稳定前缀 hash**；④ **并发写入（新增，回应反馈）**：多 agent/子 agent 同时写 `memory_fts`/`memory_meta`，WAL + busy_timeout 下无丢失/无死锁；⑤ embedder 切换后 stale 条目降级为 FTS5 打分、`reembed` 后恢复。
- **负例**：embedder 关、DB 锁、坏 JSON、空库、redaction 边界（无密钥文本不被误改）。
- 验收：`make check` 全绿；`deepseeknova-embed` 的 `local` feature 默认关闭，CI 主链路不引重依赖。

## 15. 分期与前置条件

- **P1 统一 + 持久 + 前置项（最高杠杆、最低风险）**：工具改接持久 `MemoryStore`；`MemoryEngine` 门面；起点召回注入 + 结束沉淀钩子（先 FTS5）；**redaction**、**CLI 可审查入口**、**可观测性计数**同批交付（三者为 `auto_learn` 前提）。→ 首次真正形成闭环，修复核心断连 bug，端到端打通"越用越聪明"。
- **P2 语义（数据驱动启动，见 §10）**：`Embedder` trait + 本地/远程后端 + `memory_meta` 向量 + 混合 RRF + `memory reembed`。
- **P3 生命周期 + 归纳**：持久化 lifecycle 迁移与衰减、分层归纳、懒惰 re-embed。

## 16. 明确不做（后续候选）

HNSW/ANN 索引（先暴力 cosine）、跨机器联邦记忆、桌面端记忆可视化 UI（属支柱⑤）、多用户画像隔离、记忆加密存储。

## 17. 成功标准

1. `remember` 写入的记忆**跨进程重启后仍可 `recall`**（当前会蒸发——这是首要验收）。
2. 一次够格任务结束后，`.deepseeknova/skills/` 或记忆库自动新增对应 skill/lesson，且经过 redaction（无明文密钥）。
3. 起点召回注入使稳定前缀 hash **不变**（前缀缓存不被破坏）。
4. 被召回记忆的 `recall_count` 随复用增长，并按规则晋级 Verified/Permanent。
5. `[memory] enabled=false` 时所有行为与现状完全一致；`auto_learn=false` 时不自动沉淀（模型仍可显式 `remember`，召回照常）。
6. `memory stats` 能输出召回命中率与 reinforce 比例，作为 P2 决策依据。

## 18. 假设与置信度

- **置信度：高**。"两套记忆断连"与"lifecycle 未持久化"已在 `store.rs` / `tools/memory.rs` / `recall.rs` / `lifecycle.rs` / `agent.rs` / `runtime` 直接读码证实。
- 假设（中置信）：本地嵌入模型体积/推理成本可接受（P2 前用 P1 数据验证再定）；廉价档模型足以产出可用的 skill/lesson 摘要（P1 可先用主模型低 effort，观察质量）。
- 待实现时确认：`distill_model` 经由 provider factory 的解析路径；`spawn` 沉淀任务与 desktop `CumulativeUsage` 统计的计费归集口径。

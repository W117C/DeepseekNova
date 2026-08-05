# 任务书：记忆生命周期闭环（2026-08-05）

## 1. 意图
把长期记忆从"写入→关键词检索"升级为完整生命周期闭环：经验会晋级（candidate→verified→permanent）、因久不用而衰减、归档、超期清理；检索排序融合生命周期信号（重要且常用的经验排前面）；蒸馏双轨（反思 lesson / LLM 知识）统一入口。干完后记忆库不再无限膨胀，检索结果贴合实际经验价值。

## 2. 我替领导拍的板
- 单域做透=记忆生命周期；protocol / graph 新语言 / agent_loop 反思 UI 三域写入 BLOCKED 待下轮。
- 零新增依赖：排序融合用 SQL 权重实现（bm25 已在用），不引 embedder crate。
- 衰减不自动跑：只由 `memory cleanup` 命令显式触发，避免运行期不可预期写放大。
- 默认值（猜错代价低，均可配置）：decay_rate=0.1、archive_ttl_days=30、rank_lifecycle_weight=0.3（=0 时与纯 bm25 行为等价）。
- 记忆库不清空：schema 版本机制本轮只引入不迁移（暂无破坏性变更）。

## 3. 白名单（只改这些，其余只读）
- crates/deepseeknova-core/src/memory/（engine.rs / store.rs / lifecycle.rs / skill.rs）
- crates/deepseeknova-core/tests/memory_engine.rs
- crates/deepseeknova-config/src/lib.rs（仅 MemoryConfig 段 + merge + 测试）
- crates/deepseeknova-cli/src/（仅 memory 子命令相关：main.rs / cli.rs）
- GUIDE.md 记忆节、CHANGELOG.md、PROGRESS.md、BLOCKED.md

## 4. 任务
- 任务 1：schema 版本机制 + archived 检索过滤核验。store open 时读/写 meta.schema_version（初值 "1"，graph 先例 SCHEMA_VERSION=4 在 crates/deepseeknova-graph/src/store.rs:170/220-232）；版本不符走迁移表（现为空）不炸。**核验 store.search 是否排除 archived**；若未排除，补过滤（archived 不参与召回）。测试：版本写入、版本不符不炸、archived 不召回。
- 任务 2：检索排序融合生命周期因子。store.search 的 bm25 分数融合 importance/stage/recency（SQL 内实现：bm25 权重 + importance 系数 + stage 系数 permanent=1.2/verified=1.1/candidate=1.0 + recency 由 last_recalled_at 距今衰减），融合权重来自 `[memory] rank_lifecycle_weight`（默认 0.3，=0 纯 bm25）。测试：同文本不同 importance 排序不同；weight=0 与旧行为等价（组合回归）。
- 任务 3：衰减接线 + 清理闭环。lifecycle.rs:163-189 apply_decay 已实现未接线：engine.decay()（decay_rate 对非 permanent 衰减，<0.1→archived，permanent 豁免）；engine.cleanup()（decay + 删除 archived 且距最后召回 > archive_ttl_days，返回 (decayed, deleted)）；MemoryConfig 增 decay_rate/archive_ttl_days/rank_lifecycle_weight（含 merge 与测试）；CLI 增 `memory cleanup`；`memory stats` 增 stage 分布/archived 计数。测试：衰减后 importance 下降、超阈值归档、cleanup 删除过期、permanent 豁免、stats 分布正确。
- 任务 4：蒸馏入口统一。engine 增 record_knowledge(kind,title,body,tags,source)，统一 content 格式与 id 前缀；record_reflection_lesson 与 record_llm_knowledge 改为薄封装（既有调用点与测试断言不变）。测试：两入口产出同格式；跨入口去重生效（reflect 写入后 llm-distill 同内容不重复）。
- 任务 5：文档 + 收尾。GUIDE 记忆节补 cleanup/衰减/排序权重；CHANGELOG Added；cargo fmt；反向验证（改坏任务 2 一条排序断言→红→还原→绿）；提交分支 feat/memory-lifecycle（不 push）。

## 5. 防作弊
- 测试数只增不减：core memory ≥ 70、core 总 ≥ 118、cli ≥ 32、config ≥ 50、workspace ≥ 998 通过 / 0 failed（2 既有 ignored 除外）。
- 不许 mock 被测对象、删测试、放宽断言、|| true、跳过 fmt。
- 新行为必须有测试：排序融合、衰减、cleanup、版本机制、跨入口去重各 ≥1 条。
- 组合测试：rank_lifecycle_weight=0 时排序与纯 bm25 一致。
- 反向验证必须真红真绿，贴输出。

## 6. 完成条件（两条硬指标 + 止损）
- 硬指标 A：`make check` EXIT=0；workspace 通过数 ≥ 998；core memory 测试 ≥ 70。
- 硬指标 B：`memory stats`（含 stage 分布）与 `memory cleanup`（空库/有数据均不 panic）CLI 冒烟符合预期；`cargo test -p deepseeknova-core` 全绿。
- 止损：同一验收连败 3 次换路径；结果比基线差（< 998）就回滚如实报告；超 3 小时汇报进度停手。
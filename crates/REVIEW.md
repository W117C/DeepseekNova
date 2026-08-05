# REVIEW — Open Code Review 审查记录

> 追加式记录：每个审查轮次新增一个分节，保留前文。本文件首次创建于 2026-08-05。

---

## 轮次：feat/memory-lifecycle（8c7c450..ddfd4b4，记忆生命周期闭环）

### 1. 覆盖声明

- **范围确认**：`ocr delegate preview` 工作区模式输出 0 reviewable（工作树干净属预期）；改用 `--from 8c7c450 --to ddfd4b4` 后输出 10 文件 / +844 / −55，与任务声明一致。5 个代码文件可审（store.rs / engine.rs / config lib.rs / cli main.rs / cli.rs），5 个 md 文档被 ocr 按 unsupported_ext 排除，已人工审 diff。
- **规则**：仓库根无 `ocr.rules.json`，`ocr delegate rule` 使用 system 默认规则（Rust 组 + default 组），无自定义规则。
- **人工审 diff**：`git diff 8c7c450..ddfd4b4` 覆盖全部 10 个改动文件；辅助阅读未改动但相关的 lifecycle.rs（apply_decay/LifecycleMeta）与 store.rs 全文（run_memory_search SQL、record_recall、delete）。
- **测试**：已跑 `cargo test -p deepseeknova-core memory` → **72 passed / 0 failed**（lib）+ 1 集成通过，与 PROGRESS.md 自述一致。未跑全量。
- 注：任务清单中的 `lifecycle.rs`、`tests/memory_engine.rs` 本轮**无 diff**（preview 已确认），未改动。

### 2. 评论表

| # | 路径 | 内容 | 起止行 | 分类 | 严重度 |
|---|------|------|--------|------|--------|
| C1 | crates/deepseeknova-core/src/memory/engine.rs | `decay()` 是跨锁 read-modify-write：`all_lifecycle()`（锁 1 读快照，store.rs:513）与逐条 `update_lifecycle()`（每条独立锁 2，store.rs:543）之间无事务；并发 `record_recall`（store.rs:378，同为 RMW）的 recall_count/last_recalled_at 增量会被 decay 的全量覆盖写回冲掉（丢失更新）。引擎可被多线程共享（runtime/tools 均调用 recall）。 | 135-151 | bug | medium |
| C2 | crates/deepseeknova-core/src/memory/engine.rs | 蒸馏 id 前缀从 "reflect"（旧 `record_reflection_lesson`）改为统一 "distill"（`content_id("distill", ...)`）：升级边界前已入库的 reflect-* 条目与新 distill-* 条目 id 不同，去重（`store.meta(&e.id)` 短路）只查新 id，同内容 lesson 会在升级后首次写入时产生重复条目。`knowledge_dedupes_across_entrances` 测试只覆盖新-新去重，未覆盖旧前缀数据。 | 297（关联 117-125） | bug | medium |
| C3 | crates/deepseeknova-cli/src/main.rs | `rank_lifecycle_weight` 配置仅 CLI `memory search` 接线（传 `recall_with_weight`）；runtime（crates/deepseeknova-runtime/src/lib.rs:529、618）与 tools（crates/deepseeknova-tools/src/memory.rs:161）仍走 `recall()` 硬编码默认 0.3——用户配置 0（回退纯 bm25）或自定义值在 agent 运行时**不生效且无法关闭融合**。已如实披露于 BLOCKED.md（遗留），但仍是产品行为缺口。 | 534（关联 runtime/lib.rs:529,618、tools/memory.rs:161） | bug | medium |
| C4 | crates/deepseeknova-core/src/memory/store.rs | `ensure_schema_version` 对未知未来版本（如 '999'）静默回写当前版本 '1'（"不炸"设计）：未来 v2 库被旧二进制打开后版本标记被降级抹除，v2 代码再打开时误判无需迁移。测试 `reopen_with_future_schema_version_does_not_crash` 固化了该降级行为；建议版本 > 当前时返回错误而非改写。 | 125-146 | bug | medium |
| C5 | PROGRESS.md | 自相矛盾："旧签名与默认行为不变" vs 同节"默认入口 recall/search 用 0.3 = 配置默认"——`store.search()`/`engine.recall()` 默认行为实际从纯 bm25 变为 0.3 融合（store.rs:255、engine.rs:81），runtime/tools 等既有调用方排序被静默改变（签名不变、行为变）。 | 第 32-33 行（跨 crate 协议记录段） | documentation | low |
| C6 | crates/deepseeknova-config/src/lib.rs | `decay_rate`（f32）无范围校验：配置负数会使 importance 上升（apply_decay 无 clamp 下限之外的保护），>1 可一次清零；`memory cleanup` 直接消费该值。 | 446-448 | bug | low |

### 3. 按严重度分组

- **critical**：无。
- **high**：无。
- **medium**：
  - C1 decay 跨锁 RMW 丢失更新（并发 recall 场景触发，窗口小但语义损坏）。
  - C2 蒸馏 id 前缀变更破坏旧数据（reflect-* 旧条目）跨入口去重，升级后可能产生重复条目。
  - C3 rank_lifecycle_weight 仅 CLI 生效，runtime/tools 无法关闭/自定义融合（已披露，非隐蔽缺陷）。
  - C4 未来版本库打开时版本号被静默降级重写，破坏版本簿记（当前迁移表为空，属潜在风险）。
- **low**：C5（PROGRESS 文档自相矛盾）、C6（decay_rate 无范围校验）。
- 已核实无问题的重点项（不报）：weight=0 数值等价纯 bm25 成立（`bm25 + 0*lifecycle`，无除零/无 NaN 源，测试断言分数相等）；archived 过滤全覆盖四路（FTS/trigram、LIKE 回退、search_hybrid 嵌入扫描、category 检索）；cleanup 三表删除事务化且与既有 `delete()` 一致；schema 版本写入单条 UPSERT 幂等原子；CLI 新 variant 匹配穷尽、无解析错误分支。

### 4. 结论

本轮 **无 critical / high**。4 条 medium（C1-C4）+ 2 条 low（C5-C6）。**建议进入修复轮**：C1（事务化或字段级更新）、C2（旧前缀数据兼容处理或文档化）、C3（runtime 接线或明确降级声明）、C4（未来版本报错而非改写）均可低成本修复；C5/C6 顺手。

---

## 修复轮：ddfd4b4 之上的 review-fix（2026-08-05）

### 修复计划（≤10 行）

1. **C1+C6**：store.rs 新增 `decay_all(decay_rate)` —— 单一 SQLite 事务内完成读-算-写（锁一次、事务一次），并在入口 `clamp(0.0, 1.0)`（顺带修 C6）；engine.decay 改走 `store.decay_all`。
2. **C2**：record_knowledge 去重检查同时查 `distill-<hash>` 与旧前缀 `reflect-<hash>` 两个候选 id，命中任一即已存在（不写返回 false）。
3. **C3**：runtime lib.rs 两处 recall 调用点（起点 :529 / mid-run :618）改 `recall_with_weight`，取 `config.memory.rank_lifecycle_weight`；tools/memory.rs 定义 `MemoryRankWeight(f64)` 扩展并由 RecallTool 读取（runtime 装配处注入）。
4. **C4**：ensure_schema_version 仅当库内版本 < 当前版本且为已知版本时迁移/回写；库内版本 > 当前版本保持原版本号不写；future 测试断言改为保持 '999'（收紧修正固化 bug 行为的断言）。
5. **C5**：PROGRESS.md 任务 2 措辞修正（默认入口行为实为 0.3 融合，签名不变行为变）。
6. 测试：C2/C4/C6 各加/改测试；验证 `cargo fmt --check` + 聚焦测试 + `make check`；反向验证 C2/C6 新断言红→绿；新 commit（含 "review-fix"）。

### 逐条处置记录

- **C1（decay 并发丢失更新）— 已修，选「store 层事务化批量衰减」**：store.rs 新增 `MemoryStore::decay_all(decay_rate)`，在**单一 SQLite 事务**内完成读-算-写（一次加锁、`tx.prepare` 读快照 → 逐条 `apply_decay` → 事务内 UPDATE，`drop(stmt)` 后 `tx.commit()`），彻底消除 `all_lifecycle()`（锁 1）与逐条 `update_lifecycle()`（锁 N）之间的跨锁 read-modify-write；并发 `record_recall` 的 recall_count/last_recalled_at 增量不再被 decay 覆盖写回冲掉。engine.decay 改为薄封装调 `store.decay_all`（签名与返回值语义不变）。**为何选事务化而非条件更新**：条件更新（`UPDATE ... WHERE importance = 旧值`）仍需先读旧值且对 archived 判定（apply_decay 内部）不透明，事务化更贴合"读-算-写"原子性且不改变语义。**证据**：既有 `decay_reduces_importance_and_exempts_permanent`/`decay_archives_below_threshold`/`cleanup_deletes_expired_archived_only` 全绿（74 passed 含新增）。
- **C2（旧库 reflect-* 去重失效）— 已修**：engine.rs `record_knowledge` 写入前去重检查改为同时查**两个候选 id**：`distill-<hash>`（新前缀）与 `reflect-<hash>`（旧 `record_reflection_lesson` 前缀，git 核实 ddfd4b4^ 用 `content_id("reflect", ...)`），命中任一即视为已存在（不写、返回 false，保留首次入库条目）。**新增测试** `knowledge_dedupes_against_legacy_reflect_prefix`：手工写入 `reflect-<hash>` 条目后，reflect 与 llm-distill 两个入口同内容写入均返回 false、列表仍 1 条、保留旧前缀条目。**证据**：core memory 74 passed（基线 72，+2）。
- **C3（runtime/tools 未接 rank_lifecycle_weight）— 已修**：runtime lib.rs 记忆装配处（约 :477）注入 `MemoryRankWeight(config.memory.rank_lifecycle_weight)` 扩展（tools recall 工具读取）；起点召回（约 :529 `rp.recall`）与 mid-run 召回（约 :618 `mid_mem.recall`）均改为 `recall_with_weight(..., rank_lifecycle_weight)`；tools/src/memory.rs 新增 `pub struct MemoryRankWeight(pub f64)` 扩展，RecallTool::execute 从 `ctx.extensions` 读取，缺失时回落 `h.recall()`（默认 0.3，行为不变）。CLI（main.rs:534）此前已接线，未动。**证据**：runtime 48 passed；tools/runtime 编译通过；默认 0.3 行为保持（扩展缺失路径即旧路径）。
- **C4（未来版本库被静默降级回写）— 已修（收紧）**：store.rs `ensure_schema_version` 重写为三态——库内版本 == 当前：无操作；库内版本为**已知旧版本**（可解析为数字且 < 当前）：走迁移（当前空）并回写当前版本；库内版本 **> 当前**（如 '999'）或不可解析：**保持原版本号不写**、只读可用。**既有测试断言收紧**：`reopen_with_future_schema_version_does_not_crash` 原断言（未断言版本，实际固化回写 '1'）改为**断言版本保持 '999' 且库可检索**——这是**收紧**（修正固化 bug 行为的断言），非放宽：原行为把未来 v2 库的版本标记降级抹除，v2 代码再打开会误判无需迁移；现断言明确禁止回写。旧版本库测试 `reopen_with_older_schema_version_does_not_crash`（'0' → 回写 '1'）未改，保持通过。**证据**：store 测试全绿。
- **C5（PROGRESS 自相矛盾）— 已修**：任务 2 条目措辞改为明确「**签名不变、行为变**：既有调用方排序从纯 bm25 变为 0.3 融合，weight=0 才等价旧行为」；遗留段更新为「遗留→已修（runtime/tools 已全部接线）」。
- **C6（decay_rate 无范围校验）— 已修，选「engine/decay 入口 clamp」**：`MemoryStore::decay_all` 入口 `decay_rate.clamp(0.0, 1.0)`（engine.decay 是唯一入口，cleanup 亦经其消费；config 侧不动以免破坏既有配置解析）。**为何选 clamp 而非报错**：`memory cleanup` 为运维命令，clamp 保证命令永不因配置值失败；负数 clamp 为 0 = 不衰减（不上升），>1 clamp 为 1 = 一次清零，均落在语义界内。**新增测试** `decay_clamps_rate_to_valid_range`：`decay(-0.5)` → decayed==0 且 importance 保持 0.6；`decay(5.0)` → 一次清零（importance==0）。**证据**：core memory 74 passed。
- **测试数字变化**：core lib memory 72 → 74（+2：C2 旧前缀去重、C6 clamp）；runtime 48 不变（装配路径已有覆盖）；workspace 计数以 make check 输出为准。
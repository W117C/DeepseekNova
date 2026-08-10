# REVIEW — Open Code Review 审查记录

> 追加式记录：每个审查轮次新增一个分节，保留前文。本文件首次创建于 2026-08-05。

---

## 轮次：feat/semantic-retrieval（工作区未提交，二轮审查修复，2026-08-06）

### 覆盖声明

- **范围确认**：`ocr delegate preview` 工作区模式输出 55 可审查文件 / 总插入
  8539 / 删除 938（Markdown 与已删除文件被 OCR 排除，已人工审关键 diff）。
- **规则**：仓库根无 `ocr.rules.json`，`ocr delegate rule` 使用 system 默认规则；
  审查按 AGENTS.md 强制路由走 open-code-review-delegate 委派模式。
- **人工审 diff**：重点审 security/readonly、permission、sandbox、provider、
  sub_agent、coordinator、TUI 权限审批路径；readonly.rs（1691 行）与
  sanitize.rs（217 行）全文阅读。
- **验证**：`make check` EXIT=0（fmt / clippy / 全 workspace 测试 / doctest /
  doc 零警告）；security 103 / permission 34 / tools 69+12+7 / coordinator
  history 用例全绿。

### 评论表（二轮审查）

| # | 路径 | 内容 | 严重度 |
|---|------|------|--------|
| R1 | readonly.rs | `gh api` 带 `-f`/`-F`/`--input` 且未显式 GET 时判只读，gh 实际自动切 POST（创建/更新资源免询问） | critical |
| R2 | permission/lib.rs | 建议规则把含 `*` 的命令原文当 glob，`rm *.tmp` 建议可放大放行 `rm -rf /` | critical |
| R3 | readonly.rs | `file`/`file -f` 放行 `-C`/`--compile`（写 magic.mgc） | high |
| R4 | readonly.rs | 裸 `printenv` 输出全部环境变量（含 API key）进 transcript | high |
| R5 | readonly.rs + shell.rs + permission | 链式/重定向/命令替换判 Dangerous 硬拒，allow 规则无法覆盖，常规命令不可执行 | high |
| R6 | coordinator.rs | 步骤历史无界增长，后续 prompt 线性膨胀 | medium |
| R7 | tui/render/dbg_status_test.rs | 未挂模块的调试测试死文件 | low |

### 修复轮验证

- R1：api 分支检测写 payload flag，无显式 GET 一律 NotReadOnly；正反测试覆盖
  `-f`/`-F`/`--input`/`-X POST` 与 `--method GET -f`。
- R2：`Rule` 新增 `exact` 精确匹配，建议规则不再做 glob 解释；用户配置规则语义不变。
- R3/R4：`file` 移出任意参数表并新增 `file_allowed` 拒绝 `-C`；`printenv`
  仅放行显式变量名。
- R5：普通 shell 组合归 NotReadOnly 走审批/规则，Dangerous 仅保留工具级注入面。
- R6：history 上限 50 条 / 50 万字符 / 单条 2000 字符截断。
- R7：删除 dbg_status_test.rs。
- 复跑：`make check` EXIT=0；修复前复现用例全部翻转为预期结果。

---

## 轮次：feat/semantic-retrieval（工作区未提交，安全边界收尾，2026-08-06）

### 1. 覆盖声明

- **范围确认**：`ocr delegate preview` 工作区模式输出 17 可审查文件 / 总插入
  3218 / 删除 97（AGENTS/GUIDE/SECURITY/Cargo.lock 被 OCR 按 unsupported_ext
  排除，已人工审 diff）。
- **规则**：仓库根无 `ocr.rules.json`，`ocr delegate rule` 使用 system 默认规则。
- **人工审 diff**：逐文件审工作区 diff；新增 readonly.rs（1650 行）与
  sanitize.rs（217 行）全文阅读；辅助阅读 permission/sandbox/runtime/sub_agent
  调用链与 fs 工具路径消毒实现。
- **测试**：`cargo check --workspace` 通过；permission/security/sandbox/runtime
  聚焦测试全绿；`make check` EXIT=0（1108 passed / 2 既有 ignored）。

### 2. 评论表（审查轮）

| # | 路径 | 内容 | 严重度 |
|---|------|------|--------|
| R1 | readonly.rs | `date -u`/`date +%s`/`hostname -f/-s` 前缀匹配放行写形态（`date -u -s ...`、`hostname -f newname`） | high |
| R2 | readonly.rs | `gh auth status --show-token=true`/`-t=true` 绕过 token 拒绝 | high |
| R3 | permission/lib.rs | `is_within_workspace` 回溯丢弃 `..`，`root/missing/../../outside` 误判工作区内 | high |
| R4 | permission/lib.rs | deny 规则命中仍附无效 allow 建议且无说明 | medium |
| R5 | sandbox/bubblewrap.rs | FullAccess 档未实现"可写任意路径"，与 seatbelt/文档不一致 | medium |
| R6 | GUIDE.md | sandbox 节仍写 `tools.sandbox = true`，且"工作区默认可写"当时未实现 | medium |
| R7 | readonly.rs | `journalctl --setup-keys`/`--update-catalog` 写操作漏拒 | medium |
| R8 | sub_agent.rs | 无 permission gate 时全部 fail-closed，与主 agent/文档不一致 | medium |
| R9 | agent.rs（测试） | 并行测试临时目录纳秒撞名 → `git init` flaky | low |

### 3. 修复轮验证

- R1/R2/R7：readonly 表/gh 判定/journalctl 拒绝列表修复；独立 harness 实测
  `date -u -s ...`、`hostname -f newname`、`gh auth status --show-token=true`
  均由 ReadOnly 翻转为 NotReadOnly，`=false` 形态仍 ReadOnly。
- R3：回溯余段保留 `ParentDir` + 拼接后词法折叠；新增
  `check_denies_dotdot_escape_through_missing_dir` 回归测试。
- R4：规则 deny 不再生成 allow 建议；`agent_permission_gate_denies_tool` 断言
  同步收紧。
- R5：FullAccess 绑定 `/` 读写、移除只读系统绑定；bubblewrap 测试补断言。
- R6：runtime 把工作区根并入沙箱可写绑定；GUIDE sandbox 节改
  `[sandbox] enabled = true`。
- R8：子代理无 gate 时直接执行（与主 agent 权限关闭语义一致）。
- R9：改用 `tempfile::tempdir()`（agent dev-deps 增加 tempfile）。
- 复跑：`make check` EXIT=0（fmt / clippy -D warnings / 全 workspace 1108 passed /
  doctest / doc），修复前复现用例全部翻转为预期结果。

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

---

## 第二轮审查：修复 diff ddfd4b4..c65b0e0（2026-08-05）

### 1. 覆盖声明

- **范围确认**：`ocr delegate preview` 工作区模式 0 reviewable（工作树干净属预期）；改用 `--from ddfd4b4 --to c65b0e0` 后输出 4 文件可审（engine.rs / store.rs / runtime lib.rs / tools memory.rs）+ 2 md 排除（PROGRESS.md / REVIEW.md，已人工审 diff）。与任务声明（6 文件 +258/−26）一致。
- **规则**：仓库根无 `ocr.rules.json`（`ls` 确认），`ocr delegate rule` 使用 system 默认规则（Rust 组 + default 组），与第一轮一致。
- **人工审 diff**：`git diff ddfd4b4..c65b0e0` 覆盖全部 6 个改动文件；辅助阅读未改动但相关的源码：content_id（engine.rs:51）、apply_decay（lifecycle.rs:124-152）、all_lifecycle（store.rs:525-550，与 decay_all 内联 SQL 逐列比对）、update_lifecycle（store.rs:555-571，与 decay_all UPSERT 列集比对）、record_recall（store.rs:390，锁同一 `self.db`）、MemoryStore::open 建表顺序（store.rs:180-206）、config rank_lifecycle_weight 类型（config lib.rs:457-458，f64）。**只审本轮 diff + 支撑上下文，未审范围外代码。**
- **测试**：`cargo test -p deepseeknova-core memory` → **74 passed / 0 failed**（lib）+ 1 集成通过（memory_persists_across_reopen），与修复者声明一致（基线 72 → 74，+2 为 C2/C6 新增测试）；`cargo check -p deepseeknova-runtime -p deepseeknova-tools` 编译通过（C3 改动）。未跑全量 make check。

### 2. 评论表

| # | 路径 | 内容 | 起止行 | 分类 | 严重度 |
|---|------|------|--------|------|--------|
| （无） | — | 本轮未发现缺陷，见下方逐条复核与已核实无问题项。 | — | — | — |

### 3. 逐条复核（修复是否真修对）

- **C1（事务化衰减）— 修对，无新问题**：`decay_all`（store.rs:582-646）单一事务内完成读-算-写：`db.transaction()`（DEFERRED）→ 事务内 `tx.prepare` SELECT（SQL 与 all_lifecycle 逐列一致：LEFT JOIN + COALESCE stage/recall_count/created_at/importance 兜底）→ 逐条 `apply_decay` → `tx.execute` UPSERT（与 update_lifecycle 列集一致，含 created_at 兜底）→ `drop(stmt)` 后 `tx.commit()`。**事务边界正确**：锁一次（`self.db` 单 Mutex，record_recall store.rs:390 同锁 → 串行化，丢失更新消除）；无死锁（单锁无嵌套获取）；写放大反而减少（N 个 autocommit → 1 事务 N 条语句）；游标迭代中写 memory_meta 在 SQLite 事务快照语义下安全（测试实证绿）。**语义无丢失**：permanent 豁免（decay_all 前置跳过 + apply_decay 内双保险）、<0.1→archived（apply_decay 内改 stage，UPSERT 写回）、decayed 计数与旧逻辑逐条等价（`meta.importance < before`）。engine.decay（engine.rs:136-138）为纯薄封装，签名与返回语义不变。锁持有时间随条目数 O(n) 线性增长，但对显式运维命令（cleanup）属可接受设计权衡。
- **C2（双候选 id 去重）— 修对，无新问题**：`content_id`（engine.rs:51-57）= `format!("{prefix}-{:016x}", hash(content))`，hash **只吃 content**，prefix 仅是 format 字符串 → `distill-<hash>` 与 `reflect-<hash>` 两 id 均可算出且 hash 后缀一致。旧库条目核实：git show 8c7c450 的 `record_reflection_lesson`（engine.rs:99-113）用 `format!("kind: lesson\n{lesson}")` + redact + `content_id("reflect", content)`，与现 record_knowledge 的 content 构造**完全一致** → reflect 候选 id 真能命中旧库条目（同 redact 配置下）。命中任一即 `return Ok(false)` 不写、保留首次条目（engine.rs:288-290），返回语义与既有 distill 命中一致。测试 `knowledge_dedupes_against_legacy_reflect_prefix`（engine.rs:693-723）覆盖 reflect 与 llm 两个入口。成本：每次写入多一次 meta 查询，可忽略。
- **C3（runtime/tools 权重接线）— 修对，无新问题**：config `rank_lifecycle_weight: f64`（config lib.rs:458）与 `MemoryRankWeight(pub f64)`（tools memory.rs:18）类型一致；runtime 装配处注入扩展（runtime lib.rs:484-488，全仓唯一构造点）；起点召回（runtime lib.rs:533-539）与 mid-run 召回（runtime lib.rs:627-633）均改 `recall_with_weight(query, top_k, weight)` 且取同一配置值；`recall()` 委托 `recall_with_weight(..., DEFAULT_RANK_WEIGHT=0.3)`（engine.rs:79-83）默认一致性成立；tools 侧缺失扩展回落 `h.recall()`（tools memory.rs:166-171）= 0.3，与配置默认一致，无 panic 路径（`extensions.get` 返回 Option）。闭环：CLI `memory search`（main.rs:534，第一轮已接线）与 runtime/tools 全部接同一配置源。
- **C4（三态版本核对）— 修对，且确为收紧**：`ensure_schema_version`（store.rs:129-158）三态：None（旧库，open 先 `execute_batch(MEMORY_SCHEMA_SQL)` 建 meta 表 store.rs:190 → 无版本行）→ 写当前版本；可解析且 < 当前 → 迁移空跑 + 回写；> 当前（'999'）或不可解析 → **不回写**。测试断言（store.rs:1405-1422）从"未断言版本（固化降级回写）"改为"断言保持 '999' 且可检索"——**确为收紧**（加强不变式，非放宽）；旧版本库测试（'0' → 回写 '1'）未改且随 74 passed 保持绿。已知旧版本迁移路径完好。
- **C6（clamp 语义）— 修对，无新问题**：`decay_all` 入口 `decay_rate.clamp(0.0, 1.0)`（store.rs:583），所有调用路径（engine.decay、cleanup → decay → decay_all）均过此入口；负值 → 0 = 不衰减（apply_decay(0) 数值不变，测试断言 decayed==0 且 importance 保持 0.6）、>1 → 1 = 一次清零（0.6-1.0 → max(0.0) = 0.0，测试断言 importance==0.0）；两边界均有测试覆盖（engine.rs:727-746 `decay_clamps_rate_to_valid_range`）；clamp 而非报错符合"运维命令不因配置失败"意图。
- **C5（文档）**：PROGRESS.md 任务 2 措辞与遗留段均修正，与实现一致。

### 4. 新引入问题排查（均无）

- **SQL 注入面**：无新输入拼接，decay_all 全部参数化（id/stage/recall_count/last_recalled_at/created_at/importance）；版本核对 SQL 无用户输入。
- **事务锁持有时间**：单 Mutex 串行 + 单连接，无死锁可能；O(n) 锁持有属 cleanup 运维命令可接受权衡（已在 C1 注明）。
- **扩展读取 panic 路径**：`extensions.get::<MemoryRankWeight>()` Option 回落，无 unwrap；`with_extension` 无 panic 路径；MemoryRankWeight 为 Copy f64，无生命周期问题。
- 无 unsafe 新增；无性能热点（双前缀去重多一次 meta 查询、clamp 零成本）。

### 5. 按严重度分组

- **critical**：无。**high**：无。**medium**：无。**low**：无。
- 本轮 0 条评论原因：6 条修复逐条核对均"修对且无新问题"——C1 事务边界/语义/并发均核实（含与 all_lifecycle/update_lifecycle 的 SQL 逐列比对与 record_recall 同锁确认）；C2 的 hash 机制与旧格式（git 历史 8c7c450 实证）核实为真能命中；C3 类型/注入/回落/默认一致性核实；C4 收紧方向核实（断言加强）；C6 两边界测试实证；新增测试 74 passed 全绿。未发现需修复的缺陷，故评论表为空。

### 6. 结论

**0 critical / 0 high / 0 medium / 0 low**。聚焦测试 `cargo test -p deepseeknova-core memory` 74 passed + 1 集成通过，runtime/tools 编译通过。**满足退出条件，可进入收尾**（建议收尾时按仓库惯例跑一次全量 `make check` 作最终确认）。
## 第四轮审查（P 域 protocol-followup）：285fe60..95f695e（2026-08-05）

### 1. 覆盖声明

- **范围**：285fe60..95f695e（单提交 95f695e feat(protocol): task_rate 指标落地 + fitness record_use 回填接线）。仅审该 diff；285fe60（graph 域）未审（另一 worker 负责）。
- **流程**：`ocr delegate preview`（3 reviewable：cli/metrics/runtime；PROGRESS.md 与 plan 为 unsupported_ext 排除）→ `ocr delegate rule`（仓库无 ocr.rules.json，用 system 默认）→ `git diff 285fe60..95f695e` 全量 + 现行文件精读（runtime :515-740 注入区、:1240-1340 metrics hook、:1395-1445 diagnose hook；agent.rs diagnose hook 触发条件测试；diagnose.rs failures 构造；cli/main.rs :850-915 装配）。
- **测试实测**：`cargo test -p deepseeknova-metrics` 20 passed 0 failed；`cargo test -p deepseeknova-runtime` 51 passed 0 failed（与提交声明一致；未跑全量 make check）。

### 2. 评论表

| # | 路径 | 内容 | 起止行 | 分类 | 严重度 |
|---|------|------|--------|------|--------|
| L1 | crates/deepseeknova-metrics/src/lib.rs | retry_rounds 语义=失败详情条数而非真实重试轮次：DiagnoseReport.failures 每条=一次失败详情（工具失败/阶段失败），同一重试轮内可含多条，故 retry_rounds 是失败详情的计数代理。代码注释与设计文档已明示该近似（"每条失败详情记一轮重试"），且报告无显式轮次结构可数。语义偏差、有文档、非缺陷。 | 341-350（fill_task_rate 文档） | 指标语义 | low |
| L2 | crates/deepseeknova-runtime/src/lib.rs | diagnose 回填 `first_pass = failures.is_empty()` 不看 outcome：非 Completed 且零失败详情的会话（如 Cancelled 用户中断无工具失败）会被标 first_pass=true（"一次通过"）。Completed 路径不受影响（metrics hook 已填 true 且 diagnose 被 suppress）。窄路径，建议回填仅覆盖 failures 非空场景或排除非失败型 outcome。 | 1431-1435 | 指标语义 | low |
| L3 | crates/deepseeknova-runtime/src/lib.rs | session_skills 去重跨整个 agent 生命周期：同一 agent 复跑多会话时，第二会话注入同名技能因集合已含该名不再写入 → fitness uses 少计（仍为 1）。当前 CLI 每进程单 run、serve/TUI 不走此 builder，实际不可达；若未来复用 agent 多 run 需按会话清空或分桶。 | 609-615 + 1316-1336 | 计数正确性 | low |
| L4 | crates/deepseeknova-metrics/src/lib.rs | update_scorecard_task_rate 对损坏文件静默 Ok（与 list_scorecards 同口径，设计如此），但解析失败连 warn 都没有（诊断回调仅对 Err 分支 warn）——真实损坏时 task_rate 回填静默丢失。诊断报告本身仍在可对账，可接受；建议 parse 失败 warn 一次、与 NotFound 静默区分。 | 376-402 | 错误处理 | low |
| L5 | docs/superpowers/plans/2026-08-05-protocol-followup-plan.md | 计划白名单自相矛盾：§3 声明"memory 装配区 :480-635 只读"，但任务 2 要求改注入侧，提交实际在该区内插入 sink 代码（:531-533、:609-615）。改动为 None 时零行为变化的纯增量，无实质风险；属计划表述问题。 | 白名单段（计划 §3） | 流程/文档 | low |

### 3. 重点复核逐条结论

- **task_rate 推导正确性**：无冲突。Completed → metrics hook 在 write_scorecard（:1291）**前**填 first_pass=true（:1273-1284）；diagnose 仅非 success 触发（agent.rs:3456 测试实证 success suppress）→ 无覆写路径。Paused/error/unverified → metrics hook 先落保守 false/0，诊断回调随后按 failures 回填；CLI 挂载序 metrics→quality→diagnose（main.rs:880-898）与测试 `scorecard_task_rate_failed_run_backfilled_from_diagnose` 均实证。retry_rounds 计数来源真实（DiagnoseReport.failures 条数），但语义是"失败详情数"而非"轮次"（见 L1）。
- **旧 scorecard 兼容**：`#[serde(default)]` false/0 正确（保守口径），旧 JSON 测试覆盖；update 辅助对缺字段旧卡同样可覆写（测试覆盖）。读-改-写对缺失→Ok（metrics 未启用必需）、损坏→Ok（与 list_scorecards 同口径）合理；真实 IO 错误（非 NotFound）仍返回 Err 并 warn，未吞（见 L4 建议）。
- **session_skills 可选参设计**：None=旧行为成立（build_agent_with_task_provider 透传 None，签名不变；全仓生产调用方仅 CLI 传 Some）。注入路径覆盖完整：**技能注入仅存在于起点召回**（mid-run 召回 :640-700 只注入 memory+graph 命中，无技能）→ sink 覆盖全部技能注入路径。去重防重复。fitness hook 在 record_result 前补 record_use（:1331-1332），每次会话 hook 仅触发一次 → 幂等；SkillManager::record_use（三态迁移）与 FitnessStore::record_use（uses 计数）各司其职，无双写冲突（见 L3 多 run 边界）。
- **并发安全**：无死锁。注入侧在持有 SkillManager 锁期间获取 session_skills 锁（:604-615），hook 侧仅单独获取 session_skills（:1316-1318）→ 单向锁序；两处均无 await 持锁（hook 为同步闭包）；`if let Ok` 静默吞 poison 可接受。
- **既有行为保护**：warn-once 移除后空集合静默跳过、不写文件不 warn；既有测试 `fitness_empty_skills_skips_silently_and_writes_no_file` 未改动、仍绿。memory 装配区未误动（仅插入 None 时零行为的增量 sink 代码）。
- **测试质量**：成功（first_pass=true 无 diagnose 目录）、Paused 失败（回填=条数）、空集合（既有测试）、旧卡兼容、update 辅助五态全覆盖；Cancelled 路径未单独测（回填逻辑 outcome-agnostic，Paused 已覆盖同代码路径）。既有断言无放宽——metrics 既有测试仅因新字段补结构体初始化（first_pass/retry_rounds），runtime 既有测试仅加 `None` 参数；均非削弱。

### 4. 按严重度分组

- **critical**：无。**high**：无。**medium**：无。**low**：5 条（L1-L5，见上表）。
- 本轮无 critical/high 的原因：task_rate 双端接线经 agent 侧 suppress 测试实证无冲突路径、时序（metrics 先于 diagnose）与落盘顺序（fill 先于 write）均正确；注入收集覆盖唯一技能注入路径；旧卡兼容与静默跳过策略与既有 list_scorecards 口径一致；并发无死锁（单向锁序、无 await 持锁）。

### 5. 结论

**0 critical / 0 high / 5 low**。核心闭环（task_rate 双端 + record_use 回填 + 旧卡兼容）正确，测试实测 metrics 20 / runtime 51 全绿。L1-L4 为可择机处理的语义/边界项，L5 为计划表述问题，均不阻塞。**建议进入修复轮**（可选：修复 L2 的 Cancelled 零失败误标 first_pass=true 与 L4 的损坏文件 warn 区分，其余记录即可）。

## 第三轮审查（G 域 graph-go）：68fb094..285fe60（2026-08-05）

### 1. 覆盖声明

- **范围**：68fb094..285fe60（单提交 285fe60 feat(graph): Go 语言支持——tree-sitter-go 解析 + go.mod 外部依赖）。仅审该 diff；95f695e（protocol 域）未审（另一 worker 负责）。新文件 docs/superpowers/plans/2026-08-05-graph-go-plan.md 读全文；其余按 diff 审。
- **流程**：`ocr delegate preview`（workspace 模式 0 文件 → 按任务书用 `--from 68fb094 --to 285fe60`，4 reviewable：Cargo.toml/parser.rs/store.rs/graph_tools.rs，其余 7 个 .md/.lock 为 unsupported_ext 排除）→ `ocr delegate rule`（仓库根无 ocr.rules.json，用 system 默认两组规则）→ `git diff` 全量 + 现行文件精读（parser.rs extract_signature/entity_name/callee_name、store.rs collect_files/refresh 循环/node_id/find_by_name）。
- **grammar 实证**：对照 ~/.cargo/registry 内 tree-sitter-go-0.25.0/src/node-types.json 逐节点核对——type_declaration（children **multiple**: type_spec|type_alias）、type_spec(name+type 字段)、type_alias(name+type)、method_declaration(name=field_identifier、receiver=parameter_list)、import_spec(path=interpreted/raw_string_literal)、selector_expression(field=field_identifier)、call_expression(function)、interface_type(method_elem)——与 PROGRESS.md 实测记录一致。
- **测试实测**：`cargo test -p deepseeknova-graph` 38 passed 0 failed（unit 38 + self_index 1 ignored 既有），与提交声明一致；未跑全量 make check。

### 2. 评论表

| # | 路径 | 内容 | 起止行 | 分类 | 严重度 |
|---|------|------|--------|------|--------|
| M1 | crates/deepseeknova-graph/src/parser.rs | Go 分组类型声明 `type ( A struct{...}; B interface{...} )` 只采集第一个 type_spec：entity_kind 与 entity_name 均只取 `named_child(0)`，而 grammar 实证 type_declaration 的 children 是 **multiple**（type_spec|type_alias）——同一节点可含多个 type_spec，遍历时子 type_spec 命中 `_ => None` 不建实体 → 组内第 2+ 个类型静默丢失（无错误无警告），其引用不可解析。分组声明在真实 Go 代码（含生成代码）常见。修复方向：entity_kind/entity_name 对 Go type_declaration 遍历全部 named children 逐个产出实体。fixture 未覆盖该形态。 | entity_kind ~138-150、entity_name ~191-199 | bug | medium |
| L1 | crates/deepseeknova-graph/src/parser.rs | Go import 分支注释只写"相对路径=File，其余=External"，未注明 grammar 无法区分 stdlib 与第三方（该限制仅记录于 PROGRESS.md）；GUIDE 表述"stdlib/第三方路径记外部依赖"与实现一致。建议代码注释补一句限制说明，避免后续误读为可区分。 | import 分支 ~364-388 | documentation | low |
| L2 | crates/deepseeknova-graph/src/parser.rs | method_declaration 实体（如 Greet）只归属文件/包（path），与接收者类型（User）无任何图边/字段关联——receiver 仅出现在 signature 文本中。与既有"名称级"设计一致（Rust impl 方法同层级），但 struct→method 归属关系在图结构中不可见，影响按类型聚合的 impact 分析。非回归，属新功能设计边界，建议文档注明或后续按 receiver 建 contains 边。 | method_declaration 分支 ~131-132 + Node 构造 ~457-470 | maintainability | low |
| L3 | GUIDE.md / CHANGELOG.md | 285fe60（graph 提交）内混入 protocol 域文档条目（task_rate first_pass/retry_rounds、fitness record_use 回填，GUIDE :321-330、CHANGELOG Added 首条）——实测 95f695e 未触碰这两个文件，protocol 域代码在 95f695e、其文档却落在 graph 提交。功能无影响，但提交归属不纯，回滚/二分/归因时易混淆。 | GUIDE.md Added 段（protocol 条目）、CHANGELOG.md Added 首条 | other | low |
| L4 | crates/deepseeknova-graph/src/store.rs | parse_go_mod_deps 对 `require ( // 行尾注释` 形态失效：块检测用精确相等 `t == "require ("`，带尾注释时整块依赖静默丢失（后续裸 module 行无 "require " 前缀全被跳过）。gofmt 正常输出不带此形态（注释独立成行），属畸形但合法的 go.mod；建议块检测改 strip_prefix 容忍尾注释。另无 replace/exclude 段负例测试（逻辑分析确认正确跳过，属测试覆盖缺口）。 | parse_go_mod_deps ~1136-1165 | bug（边界） | low |

### 3. 重点复核逐条结论

- **grammar 节点名实测**：38 passed 强证据；node-types.json 逐项核对与 PROGRESS 记录及实现完全一致（function_declaration/method_declaration/type_declaration>type_spec/interface_type>method_elem/selector_expression(field)/import_spec(path)），无凭猜错误。唯一偏离 grammar 能力之处见 M1（multiple children 只取首个子节点）。
- **Go 语义**：import 三态实现如实（相对→File、其余→External），stdlib/第三方不可分限制在 PROGRESS 有说明、代码注释缺（L1）；callee_name selector_expression 取 field 末段正确（node-types 实证 field=field_identifier）；method_declaration 归属=包/文件级，receiver 不建边（L2）；extract_signature 沿用 "{" 正确——Go 函数体一律 `{`，单行 func main() { ... } 也在 `{` 处截断，fixture 断言 `func MakeUser(name string) *User` 且不含 `{` 实证。
- **go.mod 解析**：块式+单行+注释过滤+`// indirect` 行（split_whitespace 取首段天然剥离尾注释）均正确；go 指令行、toolchain、replace/exclude 段经逻辑分析确认跳过（无 "require " 前缀/非 `require (` 块首）；唯一漏洞见 L4（`require (` 行尾注释）。
- **既有语义保护**：parser.rs `fn go(&self)` 动态分发测试（records_dyn_call_site_as_regular_call，:773-781）与 store.rs search("go") 短词 LIKE 测试（fts_search_short_english_falls_back_to_like，:1450-1465）均原样未动，38 passed 含两者。
- **SCHEMA_VERSION=4 合理性**：未加列/无 language 字段成立——node_id=path#name#start_line（model.rs:102）路径作用域，Go 与 Rust 同名实体（如 "New"/"new"）按 path 区分、find_by_name 精确匹配返回多行按 path 排序，跨语言同名共存为既有设计，无新冲突。Go 实体入库不需要 schema 变更，v4 不动合理。
- **依赖与文档**：新依赖仅 tree-sitter-go 0.25.0（Cargo.toml +1，Cargo.lock 新增其条目，传递依赖 tree-sitter-language/cc 均为既有 crate 版本）；README/GUIDE/CHANGELOG 语言列表均含 Go 且描述与实现一致；graph_tools.rs deps_code 提示语补 go.mod 同步。文档与实现一致（除 L3 提交归属问题）。

### 4. 按严重度分组

- **critical**：无。**high**：无。**medium**：1 条（M1 分组类型声明丢实体）。**low**：4 条（L1-L4）。
- 无 critical/high 的原因：核心分派点（实体/调用/import/签名）经 38 passed 测试与 node-types.json 实证双重验证正确；go.mod 解析主路径（块式/单行/注释/indirect）正确且 replace/exclude 跳过经逻辑分析确认；既有语义测试未被误改；SCHEMA 未动无兼容风险；新依赖单一。

### 5. 结论

**0 critical / 0 high / 1 medium（M1）/ 4 low**。聚焦测试 `cargo test -p deepseeknova-graph` 38 passed 0 failed 实测通过。M1（分组 type 声明丢实体）为新功能真实缺陷，建议修复轮处理（遍历全部 type_spec 子节点建实体 + 补分组 fixture 测试）；L4 的 `require (` 尾注释容错可一并加固；L1-L3 记录即可。**建议进入修复轮**。

---

## 修复轮：95f695e 之上（G/P 域 review-fix，2026-08-05）

### 修复计划（≤10 行）

1. **G-M1**：parser.rs 的 Go type_declaration 移入 parse_source 特殊分支——遍历全部 type_spec/type_alias 子节点逐个产出实体（单声明走同一路径）；Step::Exit `pop_def` 改计数以匹配多实体出栈；entity_kind/entity_name 删除 Go type_declaration 分支（不再可达）。
2. **G-L4**：store.rs `parse_go_mod_deps` 块起始检测改 `strip_prefix("require (")` + 尾注释容忍（trim 后为空或以 `//` 开头即块首）；replace/exclude 段负例测试。
3. **P-L2**：runtime 诊断回调 task_rate 回填仅在 `failures` 非空时覆写 `first_pass=false`/`retry_rounds`；零失败（Cancelled/unverified）不覆写、保持 metrics 侧已填值；抽 `backfill_scorecard_task_rate` 辅助函数便于单测。
4. **P-L4**：metrics `update_scorecard_task_rate` parse 失败 `eprintln!` warn 一次后返回 Ok（metrics 无 tracing 依赖且白名单不含其 Cargo.toml）；真实 IO 错误仍 Err。
5. 测试：G-M1 分组 fixture（A/B 两实体）、G-L4 尾注释块 + replace/exclude 负例、P-L2 Cancelled 零失败不被标 first_pass=true、P-L4 目录路径 Err 传播。
6. 验证：cargo fmt --check + 三 crate 聚焦测试 + `make check` 全绿；反向验证 G-M1/P-L2 新断言红→绿；提交含 "review-fix"。

### 逐条处置记录

- **G-M1（Go 分组类型声明丢实体）— 已修，选「parse_source 内遍历子节点逐个产出」**：Go `type_declaration` 移入 parse_source 的实体产出特殊分支——遍历全部 `type_spec`/`type_alias` named children，按 `type` 字段（struct_type→Struct、interface_type→Trait）逐个 `push_entity`；单声明（`type A struct{}`）走同一路径，行为与旧逻辑等价（旧路径也只认 named_child(0) 即第一个 type_spec）。`Step::Exit.pop_def` 由 bool 改 usize 计数，多实体时逐个出栈（def_stack/refs_stack 对齐）。entity_kind/entity_name 删除 Go type_declaration 分支（不再可达），entity_kind 移除未再使用的 `node` 参数。**为何选遍历产出而非取子节点集合**：refs 归属按「节点=一个定义体」建模，分组节点内多个 type_spec 各是独立定义体，遍历产出才能保持 refs/calls 归属正确。**测试证据**：新增 `parses_go_grouped_type_declarations`——单行分号分隔（`type ( A struct{}; B interface{} )`）与多行两形态均断言 A=Struct、B=Trait 恰 2 实体；type 别名到非 struct/interface（`A = int`）不产出。graph 38 → 40 passed。反向验证：断言改坏（B 不产出）→ 真红 1 failed → 还原 → 真绿。
- **G-L4（go.mod `require (` 尾注释整块丢失）— 已修**：块起始检测由精确相等 `t == "require ("` 改为 `strip_prefix("require (")` 后 trim，尾部为空或以 `//` 开头即视为块首（gofmt 不产但合法的畸形形态）；尾随版本等非注释内容（如 `require ( v1` 无效语法）不会误判。**测试证据**：新增 `go_mod_block_with_trailing_comment_and_negative_blocks`——`require ( // 依赖块` 形态 3 个依赖全解析；**replace/exclude 负例**：`replace ( old => new )` 与 `exclude ( ... )` 段内路径（含 `old`/`new`/`skip` 子串）均不进 deps，deps.len()==3 精确断言。graph 40 passed 含此测试。
- **P-L2（Cancelled 零失败误标 first_pass=true）— 已修，选「仅失败型覆写」**：抽 `backfill_scorecard_task_rate(dir, report)` 辅助函数——`failures.is_empty()` 直接 return（不覆写，保持 metrics hook 已填的值：Completed 的 true/0、非 Completed 的保守 false/0）；非空时覆写 `first_pass=false` + `retry_rounds=条数`（与旧行为一致）。**为何选「零失败不覆写」而非「outcome 白名单」**：outcome 是自由字符串（paused/unverified/failed/cancelled），白名单易漏且未来新增 outcome 需同步；failures 非空即「确有失败详情」是回填唯一合法依据，且 Paused 失败路径（既有 E2E 测试覆盖）不受影响。**测试证据**：新增 `diagnose_backfill_keeps_first_pass_for_zero_failure_reports`——Cancelled 零失败报告回填后评分卡 first_pass 保持 false、retry_rounds 0；失败型报告（failures 1 条）仍覆写 false/1。既有 `scorecard_task_rate_failed_run_backfilled_from_diagnose`（Paused 失败回填）未改、仍绿。runtime 51 → 52 passed。反向验证：断言改坏（期望被标 true）→ 真红 1 failed → 还原 → 真绿。
- **P-L4（损坏 scorecard 静默无 warn）— 已修**：`update_scorecard_task_rate` parse 失败分支由静默 `return Ok(())` 改为 `eprintln!` warn 一次后返回 Ok——与 NotFound 静默**区分**（NotFound 属 metrics 未启用/并发清理正常路径，无 warn）；真实 IO 错误（非 NotFound）仍返回 Err，调用方 warn。**为何用 eprintln! 而非 tracing**：deepseeknova-metrics 无 tracing 依赖（Cargo.toml 仅 serde/serde_json/provider/core），白名单不含其 Cargo.toml，且库函数无 Logger 注入点；工作区已有 eprintln! warn 先例（cli main.rs:25、runtime [diag]）。**测试证据**：新增 `update_scorecard_task_rate_propagates_real_io_errors`——路径为目录（真实 IO 错误非 NotFound）必须 Err 传播；既有「损坏文件 → 静默 Ok」测试不改（语义保持，仅内部多一次 warn），metrics 21 passed 全绿。
- **测试数字变化**：graph 38 → 40（+2：G-M1 分组、G-L4 尾注释+负例）；metrics 20 → 21（+1：P-L4 IO 错误传播）；runtime 51 → 52（+1：P-L2 零失败不覆写）。`cargo fmt --check` 零 diff；`make check` 全绿（workspace 0 failed）。
- **反向验证**：G-M1 新断言改坏 → `parses_go_grouped_type_declarations` 真红（1 failed，parser.rs:635 panic）→ 还原 → 真绿（1 passed）；P-L2 新断言改坏 → `diagnose_backfill_keeps_first_pass_for_zero_failure_reports` 真红（1 failed，lib.rs:4135 panic）→ 还原 → 真绿（1 passed）。

---

## 第二轮审查（修复轮复核）：95f695e..7f49ffc（2026-08-05）

### 1. 覆盖声明

- **范围确认**：`ocr delegate preview` 工作区模式 0 reviewable（工作树干净属预期）；改用 `--from 95f695e --to 7f49ffc` 后输出 4 文件可审（parser.rs / store.rs / metrics lib.rs / runtime lib.rs）+ 3 md 排除（CHANGELOG.md / PROGRESS.md / REVIEW.md，已人工审 diff）。与任务声明（4 代码文件 +354/−83）一致。
- **规则**：仓库根无 `ocr.rules.json`，`ocr delegate rule` 使用 system 默认规则（Rust 组 + default 组），与既往轮次一致。
- **人工审 diff**：`git diff 95f695e..7f49ffc` 覆盖全部 7 个改动文件；辅助阅读：parse_source 全文（parser.rs:270-530，含 Exit pop_def 循环、push_entity 闭包、Go 特殊分支、refs/calls 采集）、extract_doc/extract_signature（parser.rs:194-240）、parse_go_mod_deps 全文（store.rs:1136-1180）、update_scorecard_task_rate（metrics lib.rs:374-405）、runtime metrics hook first_pass 填写条件（runtime lib.rs:1275-1283）、attach_diagnose_hook_with_ingest 回填调用点（runtime lib.rs:1450-1461）、旧版 95f695e parser.rs Exit 处理（`git show` 对比 def_stack.pop 语义）。**只审本轮 diff + 支撑上下文。**
- **实证验证（/tmp 临时工程引用 graph crate，未改仓库代码）**：分组 Go 源码实测 refs/doc/signature 输出（见 R1/R2 证据）；旧版路径经代码走查确认。
- **测试实测**：`cargo test -p deepseeknova-graph` **40 passed / 0 failed**、`-p deepseeknova-metrics` **21 passed / 0 failed**、`-p deepseeknova-runtime` **52 passed / 0 failed**，与修复者声明完全一致。未跑全量 make check。

### 2. 评论表

| # | 路径 | 内容 | 起止行 | 分类 | 严重度 |
|---|------|------|--------|------|--------|
| R1 | crates/deepseeknova-graph/src/parser.rs | Go 分组 type_declaration 的 **refs 归属错误**：全部成员实体在父节点 Enter 时一次性 push（refs_stack 按序入栈、最后一个成员在栈顶），而成员子节点（结构体体、成员自身 name 节点）在**之后**才被遍历，其 identifier/type_identifier 引用全部落入**最后一个成员的 set**。实测（/tmp 工程）：`type ( A struct{ next *B; ext *C }; B struct{} )` 输出 refs=[(B,"A"),(B,"C")]——正确应为 A→B、A→C；实际 A 完全无出边，且 A 自身的 name 节点进入 B 的 set 未被 self 过滤产生**伪边 B→A**。修复者声称「遍历产出才能保持 refs/calls 归属正确」，实现恰相反。影响：分组类型声明的引用边系统性错误（find_references/impact 分析失真），无声无测试覆盖（新增 fixture `parses_go_grouped_type_declarations` 只断言实体名/kind，不查 refs）。修复方向：type_spec 子节点 Enter 时逐个建实体（而非父节点 Enter 时批量），或按成员分别入栈后再遍历。 | 446-493（push_entity 闭包 446-470、Go 分支 481-493） | bug | medium |
| R2 | crates/deepseeknova-graph/src/parser.rs | Go 类型实体 **doc 注释丢失 + signature 变化**（单声明与分组均受影响）：实体节点从 type_declaration 改为 type_spec 后，extract_doc 的 `node.prev_sibling()` 在 type_spec 上取不到声明前的注释（其前一 sibling 为 "type" 关键字或无），实测 `// User is a user.\ntype User struct{...}` 实体 doc=""（旧路径 type_declaration.prev_sibling=注释可提取）；signature 由 "type User struct" 变为 "User struct"。修复者声称单声明「行为与旧逻辑等价」不成立；GO_SRC fixture 未断言类型实体的 doc/signature，无测试暴露。 | 446-456（push_entity 内 signature/doc 构造） | bug（回归） | low |

### 3. 逐条复核（修复是否真修对）

- **G-M1 实体产出 — 部分修对，refs 归属错误（R1）**：① type_alias 跳过正确——`type A = int` 的 type 字段非 struct_type/interface_type → `continue`，fixture（grouped2）断言恰 1 实体实证；② pop_def bool→usize **无回归**——Rust/Python/JS 既有路径 pushed_def ∈ {0,1}，Exit 循环逐次弹出与旧 `if pop_def` 语义逐一等价（git show 95f695e 实证旧 def_stack.pop() 在 if 内、新在循环内，1 次时行为一致；0 次均不弹）；③ fixture 覆盖单行分号（`type ( A struct{}; B interface{} )`）与多行两形态 + 别名跳过形态，覆盖齐全；④ 实体 id/行号——node_id=path#name#start_line，组内同名（非法 Go）才会碰撞，正常不冲突；但组内实体 refs 归属（R1）与 doc（R2）均无断言。
- **G-L4 — 修对，无新问题**：块起始 `strip_prefix("require (")` + trim 后为空或以 `//` 开头即块首；`require ( v1`（无效语法）落单行分支且 `(` 前缀过滤不误入 deps；块内 `// indirect` 行过滤仍在（`!path.starts_with("//")`）；replace/exclude 段 in_require_block 恒 false 整段跳过；负例断言 `deps.len()==3` 精确（既防漏也防多），`old`/`new`/`skip` 子串检查与长度断言双重保证。
- **P-L2 — 修对，无新问题**：metrics hook 仅 `outcome==Completed` 填 first_pass=true（runtime lib.rs:1275-1283 实证），Paused/Cancelled 保持 compute 保守默认 false/0；回填 `failures.is_empty()` 直接 return 保持该值——非 Completed 零失败会话保持 false/0 是**正确保守语义**（Cancelled 不算「一次通过」），无漏标路径（outcome 非失败型但确有失败 ⇒ 非 Completed ⇒ metrics 已填 false ⇒ 回填覆写 false/条数，语义仍对）；失败型覆写 false + failures.len() 条数正确。测试覆盖 Cancelled 零失败保持 false/0 与失败型覆写 1 条。
- **P-L4 — 修对，无新问题**：metrics Cargo.toml 核实**确无 tracing 依赖**（仅 serde/serde_json/provider/core），eprintln! 与仓库先例风格一致（cli main.rs:25 `eprintln!("warning: failed to load config...")` 同 "warning:" 前缀；修复者所述 runtime [diag] 先例未单独核实，cli 先例已足够）；IO 错误测试**真实**——路径为目录时 `fs::read_to_string` 返回 IsADirectory（非 NotFound）→ Err 传播，测试实证绿；损坏文件 → warn 后 Ok 与既有「静默 Ok」测试兼容（内部多一次 warn，断言未放宽）。
- **文档**：CHANGELOG +9 / PROGRESS +1 条目与实现一致，无夸大（G-M1 条目措辞「逐个产出实体」属实，但未提 refs 归属问题）。

### 4. 新引入问题排查（除 R1/R2 外均无）

- **SQL 注入面**：本轮无新 SQL（go.mod 解析纯文本行级；metrics/runtime 改动不触 SQL）。
- **panic 路径**：新增代码无 unwrap/expect/panic（parse_source 用 `let Some(...) else { continue }`，backfill_scorecard_task_rate 全 Option/Result 处理）；无新增 unsafe。
- **并发**：无新共享状态、无锁新增；metrics/runtime 改动为既有同步文件 IO 路径。
- **性能**：分组遍历 O(组内成员) 单次，refs set 有 MAX_REFS_PER_DEF 上限，无新热点。

### 5. 按严重度分组

- **critical**：无。**high**：无。**medium**：1 条（R1 分组 refs 归属错误 + 伪边）。**low**：1 条（R2 doc/signature 回归）。
- 仅 2 条评论的原因：G-L4 / P-L2 / P-L4 三条修复逐条核对均「修对且无新问题」（含负例断言精度、metrics 侧先填值语义、eprintln! 先例与 IO 测试真实性核实）；G-M1 实体产出主诉求达成（组内每类型建实体、alias 跳过、pop_def 无回归、fixture 形态齐全），但 refs 归属与修复者声明的「保持归属正确」相反（R1，/tmp 实测铁证），单声明 doc/signature 语义变化未被声明且无测试覆盖（R2）。

### 6. 结论

**0 critical / 0 high / 1 medium（R1）/ 1 low（R2）**。聚焦测试实测 graph 40 / metrics 21 / runtime 52 全绿，与修复者声明一致。**字面退出条件（0 critical/high 且测试全绿）满足**，但 R1 是 G-M1 修复引入的真实新缺陷（分组类型声明的引用边系统性错误，附伪边），不修则本轮「修复轮」目标未完全达成：**建议再修 R1**（refs 归属改按成员逐个入栈，补 refs 断言测试；R2 doc 可顺手——extract_doc 改回在 type_declaration 上提取）。R1 修复并测试全绿后再收尾；若接受 R1 作为已披露遗留（需在 PROGRESS/BLOCKED 记录），也可进入收尾。

---

## 修复轮 2：R1/R2（2026-08-05）

**修复计划（≤10 行）：**
1. R1：Go 实体产出从「type_declaration Enter 批量 push 全部成员」改为「type_spec/type_alias 成员 Enter 时逐个 push_entity、成员子树 Exit 时 pop（复用 pop_def 计数）」——成员体内引用与自身 name 归属本成员，type_declaration 不再产出实体；grammar 实证 type_spec 仅在 type_declaration 下（node-types.json 2271-2288），改判据安全。
2. R1 测试（新 `go_grouped_type_refs_attribution`）：单行/多行两形态断言 A refs ⊇ {B,C}、B 无出边（无伪边 B→A）；方法调用归属 M→helper 且无组内残留 caller。
3. R2：signature 保留 "type " 前缀（成员签名前拼 "type "，单声明与旧行为逐字等价；分组旧产物 "type ( A struct" 本身是伪影，改后为有意义的 "type A struct"）。
4. R2 doc：成员自身 prev_sibling 取不到注释（单声明 type_spec 前一 sibling 是 "type" 关键字匿名节点）时回退父节点 type_declaration 的 prev_sibling——单声明 doc 恢复、分组成员内逐成员注释优先。
5. R2 测试（新 `go_type_doc_and_signature_restored`）：单声明 doc="User is a user."/signature="type User struct"；分组首成员 doc=组注释、各成员 signature 带 "type " 前缀。
6. 验证：cargo fmt --check + `cargo test -p deepseeknova-graph` + make check 全绿；反向验证 R1 断言改坏→真红→还原→真绿。

### 修复轮 2 执行记录

- **R1（分组 Go type_declaration refs 归属错误）— 已修，选「成员节点 Enter 逐个产出、Exit 逐个出栈」**：Go 实体产出判据由 `type_declaration`（Enter 时批量 push 全部成员，栈顶=最后成员，成员 1..n-1 的体内引用/自身 name 全部落最后成员 set，产生伪边）改为 `type_spec`/`type_alias` 成员节点 Enter 时逐个 `push_entity`、成员子树遍历完 Exit 时经 `pop_def` 计数逐个出栈——与单实体路径完全一致，成员体内引用与自身 name 节点归属本成员；`type_declaration` 不再产出实体。**为何如此修**：grammar 实证（node-types.json 2271-2288）`type_spec`/`type_alias` 仅作为 `type_declaration` 的 named children 出现（`type_declaration` 仅出现在 source_file），成员判据在单声明/分组两形态下语义一致，且复用既有 push_entity/pop_def 机制无需新状态。**测试证据**：新增 `go_grouped_type_refs_attribution`——单行分号与多行两形态断言 refs 含 (A,B)、(A,C)，B 无任何出边（无伪边 B→A）；追加分组后方法调用归属 (M,helper) 且组内类型不残留为 caller。graph 40 → 42 passed。
- **R2（Go 类型 doc 丢失 + signature 变化）— 已修，选「doc 回退父节点提取 + signature 保留 type 前缀」**：doc 先取成员自身 prev_sibling 紧邻注释（分组内逐成员注释可用），取不到时回退父节点 `type_declaration` 的 prev_sibling——单声明 `// User is a user.\ntype User struct{}` 的注释在 type_declaration 之前、type_spec 的 prev_sibling 是 "type" 匿名关键字节点，回退后 doc 恢复；signature 拼 `"type "` 前缀，单声明逐字等价旧行为（"type User struct"），分组旧产物 "type ( A struct" 本身是首个成员伪影，改后为有意义的 "type A struct"。**为何如此修**：以「与旧行为尽量等价」为原则（任务指定），两处都恢复旧语义而非接受变化；父节点回退仅对 Go 成员生效（push_entity 改收预计算 sig/doc，Rust/Python/JS 路径传自身节点，行为不变）。**测试证据**：新增 `go_type_doc_and_signature_restored`——单声明 doc="User is a user."/signature="type User struct"；分组首成员 doc=组注释、各成员 signature 带 "type " 前缀（"type A struct"/"type B interface"）。
- **测试数字变化**：graph 40 → 42（+2：R1 refs 归属、R2 doc/signature）。既有断言零改动（含 `parses_go_grouped_type_declarations` 原样保留，仅其未覆盖的 refs/doc/signature 由新测试补足）。`cargo fmt --check` 零 diff；`make check` 全绿（MAKE_CHECK_EXIT=0，55 处 test result 全 0 failed；doc 阶段仅 cli.rs:136 既有 unresolved link warning，非本轮引入）。
- **反向验证**：`go_grouped_type_refs_attribution` 中 A refs 断言改坏（期望伪边 (B,A)）→ 真红（1 failed，parser.rs:681 panic）→ 还原 → 真绿（1 passed）。

---

## 轮次：工作区未提交——依赖健康修复（2026-08-05）

### 1. 覆盖声明

- **范围确认**：`ocr delegate preview` 工作区模式输出 10 reviewable / 13 total；`Cargo.lock`、`AGENTS.md`、`CHANGELOG.md` 被 OCR 按 unsupported_ext 排除，已人工审 diff。全部 13 个改动文件均覆盖（含 `.github/workflows/security.yml`、`Cargo.toml`、`deny.toml`、cli/executor/scanner/telemetry/bench）。
- **规则**：仓库根无 `ocr.rules.json` / `.opencodereview/rule.json`，走 system 默认规则（workflow、Cargo.toml、Rust、default 四组）；AGENTS.md §1.1 跨 crate 变更协议已按完整路径执行。
- **测试实测**：`make check` EXIT=0（fmt/clippy/全 workspace/doctest/doc 零警告）；`make audit` ok；`cargo audit` 仅 1 allowed warning（RUSTSEC-2025-0068 serde_yml，deny.toml 已登记）。

### 2. 评论表

| # | 路径 | 内容 | 起止行 | 分类 | 严重度 |
|---|------|------|--------|------|--------|
| C1 | Cargo.toml | `opentelemetry-otlp` 已收窄为 trace-only，但 `opentelemetry` / `opentelemetry-sdk` / `opentelemetry-stdout` 仍用默认特性（含 metrics/logs），与意图不一致；telemetry lib.rs 与 README 仍宣称 metrics。 | 98-101 | maintainability | medium |

### 3. 处置

- **C1 — 已修**：四个 OTel 依赖统一 `default-features = false` + `trace`（sdk 保留 `rt-tokio`）；`crates/deepseeknova-telemetry/src/lib.rs` 与 `crates/deepseeknova-telemetry/README.md` 文档改为 tracing-only。
- **重审**：修复后再次 `ocr delegate preview` + 人工审 diff，无新问题；`make check` 重跑 EXIT=0。

### 4. 结论

**0 critical / 0 high / 1 medium（C1，已修）**。重审 0 问题；未提交 13 文件待用户决定提交。

---

## 轮次：记忆语义检索（embedder）最小闭环（2026-08-05）

### 1. 覆盖声明

- **范围确认**：`ocr delegate preview` 工作区模式输出 9 reviewable / 14 total；
  `BLOCKED.md` / `CHANGELOG.md` / `GUIDE.md` / `PROGRESS.md` / 计划文件被 OCR 按
  unsupported_ext 排除，已人工审 diff；9 个代码文件全部覆盖（含新文件
  `embeddings.rs` 全文）。文档类改动（GUIDE/CHANGELOG/BLOCKED/PROGRESS）在收尾步
  复核。
- **规则**：仓库根无 `ocr.rules.json`，走 system 默认规则（**/\*.rs 一组）；
  AGENTS.md §1.1 跨 crate 变更协议已按完整路径执行（预扫描/备选路径/自检见
  PROGRESS.md）。
- **测试实测**：`make check` EXIT=0（fmt/clippy/全 workspace/doctest/doc 全绿）；
  core 137 单测 + 2 集成、provider 40、config 35+18、runtime 53、tools 67+12+7、
  cli 32；2 既有 ignored（graph self_index、provider reasoning protocol）。

### ocr delegate preview 原始输出

```text
# Files (9 reviewable / 14 total)
- mode: workspace
- total_insertions: 1049
- total_deletions: 17
  - crates/deepseeknova-cli/src/cli.rs [modified] +2/-0
  - crates/deepseeknova-cli/src/main.rs [modified] +14/-3
  - crates/deepseeknova-config/src/lib.rs [modified] +66/-2
  - crates/deepseeknova-core/src/memory/engine.rs [modified] +193/-1
  - crates/deepseeknova-core/src/memory/store.rs [modified] +199/-8
  - crates/deepseeknova-provider/src/lib.rs [modified] +1/-0
  - crates/deepseeknova-runtime/src/lib.rs [modified] +67/-1
  - crates/deepseeknova-tools/src/memory.rs [modified] +38/-0
  - crates/deepseeknova-provider/src/embeddings.rs [added] +299/-0
```

### 2. 评论表（第一轮）

| # | 路径 | 内容 | 起止行 | 分类 | 严重度 |
|---|------|------|--------|------|--------|
| M1 | crates/deepseeknova-core/src/memory/store.rs | `search_hybrid_with_weight` 先拿 SQLite 锁再调 `provider.embed(query)`——嵌入是潜在慢 HTTP 调用，持锁期间其它记忆读写会被阻塞最多一个超时（默认 30s） | 739-754（修复前 749-753） | concurrency | medium |
| M2 | crates/deepseeknova-core/src/memory/store.rs | provider=None 或查询嵌入失败时返回「纯 bm25 未加生命周期权重」的结果，与文档承诺的 `search_with_weight` 等价不一致（文档写着等价） | 739-754（修复前 749-756） | bug | medium |
| L1 | crates/deepseeknova-provider/src/embeddings.rs | `try_memory_embedder` 对未知 embedder 值（如文档提到的 "local"）静默返回 None，用户配置写错得不到任何提示 | 131-142（修复前） | maintainability | low |
| L2 | core store/engine 测试 + tools memory 测试 | `FakeEmbed` 确定性向量替身在三个测试模块重复定义（各 10 行） | store.rs:1631 / engine.rs:867 / memory.rs:236 | maintainability | low |

### 3. 处置与修复记录

- **M1 + M2 — 已修（同一处重构）**：查询向量改为**拿锁前**计算（`provider.embed`
  失败/缺 provider 时 `qv=None`），随后持锁路径只做纯 SQL/余弦运算；`qv` 为 None
  时直接 `run_memory_search(..., rank_weight)` 回落纯 FTS + 生命周期权重，与文档
  承诺一致。为何如此修：HTTP 调用与锁的先后关系是根因，两问题共享同一段代码；
  重排后既消除锁内慢调用，也让回落路径语义闭环（store.rs:747-757）。
- **L1 — 已修**：`try_memory_embedder` 改为显式三态匹配，`none` → None；
  `remote` → 装配（失败 warn）；其它值 → warn「not implemented; falling back to
  FTS-only」并返回 None（embeddings.rs:131-146）；补测试断言
  `embedder="local"` fail-open 到 None。
- **L2 — 不修（记录理由）**：测试替身按 crate 就地定义可保持各测试模块自包含、
  避免为测试暴露跨 crate 公共 fixture；重复仅 3 处、每处 ~10 行，改动收益低。
- **接受的权衡（非评论）**：`RemoteEmbedder::embed` 是同步 trait 内的 `block_on`，
  在 async 工具路径（remember/recall）会阻塞一个 worker 线程至多一个超时。替代
  方案（trait 改 async + spawn_blocking）需要改动 store/engine/tools 整条调用链，
  超出本轮白名单；已有独立 runtime + 超时兜底，风险受限，已在 GUIDE 记录。

### 4. 重审（修复后）

`ocr delegate preview` 与人工 diff 重查两处修复：锁外嵌入、fallback 带生命周期
权重、未知后端 warn 均有测试/代码佐证；无新问题。聚焦测试全绿（core memory 79、
provider embeddings 5），`make check` EXIT=0。

### 5. 结论

**0 critical / 0 high / 2 medium（M1+M2，已修）/ 2 low（L1 已修、L2 接受）**。
字面退出条件（无 High/Critical 且测试全绿）满足；第二轮复核无新问题。

---

## 轮次：观测台前端 UI + TUI 演进（2026-08-07 dev-loop 轮，工作区未提交）

### 1. 覆盖声明

- **范围确认**：`ocr delegate preview` 工作区模式输出包含上一轮 P0（serve 会话 API
  + 认证，已在更早审查轮覆盖）与本轮改动混在同一工作树；本轮人工审查聚焦本轮新增：
  `permission/lib.rs`（shell_readonly_kind）、`agent.rs`（Ask 风险标签接线）、
  TUI `theme.rs` / `render/sidebar.rs` / `render/approval.rs` /
  `render/message.rs` / `model/scorecard.rs` / `commands/builtin.rs` /
  `app/state.rs` / `model/mod.rs`，以及桌面前端 `crates/deepseeknova-desktop/frontend/**`
  （TypeScript 全读；`.test.ts` 被 OCR 默认路径排除，人工读）。
- **规则**：仓库根无 `ocr.rules.json`，`ocr delegate rule` 使用 system 默认规则
  （Rust 组 + default 组）。
- **验证**：`cargo test -p deepseeknova-permission`（35 绿）、
  `cargo test -p deepseeknova-agent`（239 绿）、`cargo test -p deepseeknova-tui`
  （154 单测 + 1 doctest 绿）；`cargo clippy` 三个 crate `-D warnings` EXIT=0；
  `cargo fmt --check` EXIT=0；前端 `vitest run` 14 绿 + `npm run build` EXIT=0；
  反向验证 TUI 夜次分组与前端 nightKeyFromId 均真红→真绿。
  全量 `make check` 结果见轮次结尾。

### 2. 评论表（审查轮）

| # | 路径 | 内容 | 严重度 |
|---|------|------|--------|
| R1 | agent.rs Ask 分支 | 风险标签只写进 `RunEvent::ApprovalRequest` 描述，TUI 审批浮层消费的是 `ApprovalResponder::request` 的 description（原始参数）→ 实际 TUI 永远看不到「风险：非只读」 | high |
| R2 | agent.rs | 风险接线缺少“responder 收到带前缀描述”的端到端断言（仅有纯函数 + 渲染测试） | medium |
| R3 | permission/lib.rs | `shell_readonly_kind` 与 `check` 各解析一次参数 JSON（查询路径开销可忽略） | low |
| R4 | sidebar.rs | `group_by_night` 对每条 id 线性 `find` 组；行数上限 16，可忽略 | low |
| R5 | 工作区 | `crates/deepseeknova-tui/src/repro_tmp.rs` 为未跟踪的用户调试文件（非本轮产物），提交时排除 | other |

### 3. 修复轮验证

- R1 已修：Ask 分支改为 `request_desc = [风险:…] + 原始参数`，`responder.request`
  与 RunEvent 均传 `request_desc`；TUI 浮层可展示风险标签与完整命令。
- R2 记录为测试盲区（接线简单、纯函数/渲染/权限分类已有 4 条测试覆盖），
  后续可在 agent 集成测试补“捕获 responder 描述”断言，本轮接受。
- R3/R4 接受（开销在 16 行/单次审批以内）。
- R5：`git add` 显式排除 `repro_tmp.rs`，保留文件不动。
- 复跑：permission/agent/tui 聚焦测试 + clippy + fmt 全绿（见覆盖声明）；
  全量 `make check` 见轮次末尾。

### 4. 结论

**0 critical / 1 high（R1 已修）/ 1 medium（R2 记录接受）/ 2 low（接受）**。
修复后聚焦测试全绿；最终 `make check` EXIT=0 后满足字面退出条件。

---

## 轮次：2026-08-10 全面体检（3 子代理并行 + 父级复核）

### 1. 覆盖声明

- 3 个子代理分域审查：构建/CI 健康（`make check`、依赖、文档账本）、核心架构
  与安全边界（security/permission/sandbox/scanner/core/runtime/agent）、功能
  完整性与文档漂移（cli/config/provider/tools/mcp/graph/checkpoint/store/skills/
  telemetry/serve/tui）。按 AGENTS.md 路由使用 `ocr delegate rule` 获取审查规则。
- 父级对每条子代理结论做了源码级复核（发现子代理报告中的过期/误报后以当前
  代码为准），并补全最终验证。
- 验证：两次全量 `make check` EXIT=0（fmt / clippy `-D warnings` / 全 workspace
  测试 / doctest / doc 零警告）；新增聚焦测试单独跑绿。

### 2. 评论表（审查轮）

| # | 路径 | 内容 | 严重度 |
|---|------|------|--------|
| R1 | store/lib.rs | `new_session_id` 秒级精度，同秒连续新建会话写同一 JSONL | medium |
| R2 | cli/chat.rs | `/resume` 未校验会话 id，`../`/绝对路径可越界读写 | medium |
| R3 | store/lib.rs | TUI 侧边栏预览整文件 `read_to_string` 后只取首行 | medium |
| R4 | cli/main.rs | `config` 命令明文打印内联 api_key 与认证头 | high |
| R5 | runtime/delegate.rs | SubAgentRunner/DelegateEngine 未挂用户级 hooks，工具调用可绕过 tool_before/tool_after | high |
| R6 | agent/agent/mod.rs | 风险标签到 responder 的端到端断言缺失（上轮 R2 遗留） | medium |
| R7 | telemetry/lib.rs | 全局 subscriber 已存在时 init 仍返回 Ok，调用方误以为 OTLP 生效 | low |
| R8 | 文档 | BLOCKED/PRODUCT/README/CHANGELOG/install 脚本状态或计数过期 | low |

### 3. 修复轮验证

- R1：`new_session_id` 改为 `chat-YYYYMMDD-HHMMSS-mmm-ssss`（毫秒 + 进程内
  序号），补同秒 100 次唯一性测试。
- R2：store 新增 `is_valid_session_id`（字母数字/`-`/`_`，长度 ≤128），CLI
  `/resume` 复用并补越界回归测试。
- R3：`preview_first_prompt` / `session_workspace` 改 `BufReader` 只读首行。
- R4：`config` 展示路径对 api_key 与 authorization/x-api-key/cookie 等认证头
  统一脱敏为 `[REDACTED]`，补只读展示回归测试。
- R5：`build_delegate_engine` / `build_sub_agent_runner` 与主 agent 对称挂载
  `user_hooks_from_config`，补两条端到端回归测试（SubAgentRunner + DelegateEngine）。
- R6：新增 `ask_risk_prefix_reaches_approval_responder` 集成测试，断言
  responder 收到 `[风险:非只读]` 前缀与原始参数——上轮 R2 盲区闭环。
- R7：`TelemetryGuard` 新增 `installed()` 访问器，安装失败可被调用方识别。
- R8：README 测试数 1689→1717、CHANGELOG/BLOCKED i18n 键数→257、BLOCKED 对账、
  PRODUCT 状态更新、install 脚本版本示例对齐 0.5.0、清除桌面端过期注释。
- 复跑：聚焦测试（store/cli/runtime hooks/agent 风险链路/telemetry）全绿；
  两次全量 `make check` EXIT=0。

### 4. 结论

**0 critical / 2 high（R4/R5 已修）/ 4 medium（R1/R2/R3/R6 已修）/ 2 low（R7/R8 已修）**。
已知未做项（记录待裁决）：主对话 @-mention 拦截、DelegateEngine 递归深度贯通、
README 截图占位、API key 环境变量命名、npm 安装器承诺、Windows 沙箱排期。

---

## 轮次：2026-08-11 未提交变更复查（提交前审查）

### 1. 覆盖声明

对本轮 33 个修改 + 5 个新路径做逐块审查；用 `rustc --target
x86_64-pc-windows-msvc` 对 sandbox cfg 分支做定向复现。

### 2. 发现与修复

| # | 严重度 | 内容 | 修复 |
|---|--------|------|------|
| C1 | P1 | release.yml npm-publish 的 job 级 `if` 引用 `secrets`（该 context 在 job 级 if 不可用），tag 推送时 workflow 必失败 | 移出 job 级 if；token 经 job 级 env 透传，Publish/版本同步步骤用 `if: env.NPM_TOKEN != ''` 门控 |
| C2 | P1 | sandbox 三个 `platform_sandbox*` 的兜底 `cfg(not(any(macos, linux)))` 未排除 Windows，Windows 上与 `cfg(windows)` 分支同时编译 → E0308，JobSandbox 后端无法构建 | 四处兜底条件补 `target_os = "windows"`；Windows 分支对无法强制的网络/只读策略补 `tracing::warn!` |
| C3 | P2 | npm 包版本写死 0.5.0，bump-version.sh 不同步，发布 tag 与 npm 版本脱节 | bump 脚本同步改写 `npm/deepseeknova/package.json`；release.yml 发布前用 tag 派生 `npm version` |
| C4 | P2 | install.js 校验实际失效：`fetchText` 不跟随 GitHub 302，且 checksums 缺失/条目缺失只警告不失败，与 install.sh 语义相悖 | fetchText 跟随重定向（5 跳上限）；拉取失败或条目缺失一律 fail |
| C5 | P2 | Windows Job Object 不限制网络/写路径，策略参数静默丢弃，且原显式警告被 `is_active()` 判定移除 | main.rs 恢复 Windows 显式警告（准确表述 Job Object 边界）；库侧构造时对未强制策略打 tracing warn |
| C6 | P3 | README/README_EN 仍写 Windows 用 NoOpSandbox 无隔离 | 两处同步为 JobSandbox 现状（进程树隔离 + 限制；网络/写路径不生效） |
| C7 | P3 | `TelemetryGuard::installed()` 无调用方，R7 目标未闭环 | CLI 初始化后检查 `installed()`，未生效时 stderr 提示 |

### 3. 验证

- `node --check` install.js / bin wrapper 通过；
- `bash -n` bump-version.sh 通过；
- 修复后的 cfg 兜底条件在 `x86_64-pc-windows-msvc` 目标下定向编译复现通过；
- 本机 `cargo check` / 聚焦测试通过；
- 全量 `make check` EXIT=0（fmt / clippy `-D warnings` / 1729 tests / doc 零警告）。

## 轮次：2026-08-10 全面体检后续轮（主对话 @-mention 接线）

### 1. 覆盖声明

- 处理上一轮遗留项：主对话 @-mention 入口接线、serve 会话 id 校验复用。
- 验证：runtime mention 3 条测试 + `delegate_agent_names` 1 条测试 + serve 25
  条测试全绿；`cargo check -p deepseeknova-cli` 通过；全量 `make check` EXIT=0。

### 2. 评论表（审查轮）

| # | 路径 | 内容 | 严重度 |
|---|------|------|--------|
| R1 | runtime/mention.rs（新增） | 主对话 @-mention 无入口拦截：prompt 含已知 @子代理仍走主 agent | medium |
| R2 | serve/lib.rs | `valid_session_id` 与 store 校验两套实现，长度上限语义不一致 | low |

### 3. 修复轮验证

- R1：新增 `MentionAwareRunner`（主 agent / SubAgentRunner 选择器）：
  `@name` 已知 → 子代理；零引用 → 主 agent；多引用 → 显式报错不降级。
  `SubAgentRunner` 暴露 `agent_names()`；runtime 暴露 `delegate_agent_names()`
  供预检与 TUI `@` 补全候选。CLI REPL 与 TUI 工厂在 `[delegate] enabled=true`
  时装配包装器。
- R2：serve `valid_session_id` 改为委托 `deepseeknova_store::is_valid_session_id`
  （同一契约，长度 ≤128）。
- 复跑：聚焦测试全绿 + 全量 `make check` EXIT=0。

### 4. 结论

**0 critical / 1 medium（R1 已修）/ 1 low（R2 已修）**。仍待裁决：DelegateEngine
递归深度贯通、README 截图占位、API key 环境变量命名、npm 安装器承诺、Windows
沙箱排期。

---

## 轮次：2026-08-10 遗留项收束轮

### 1. 覆盖声明

- 处理上一轮全部遗留项：DelegateEngine 递归贯通、README 截图占位、API key
  环境变量命名、npm 安装器承诺、Windows 沙箱排期。
- 验证：agent 引擎递归 3 条、runtime 装配 1 条、provider 回退 1 条聚焦测试
  全绿；全量 `make check` EXIT=0。

### 2. 评论表（审查轮）

| # | 路径 | 内容 | 严重度 |
|---|------|------|--------|
| R1 | agent/delegate.rs + runtime/delegate.rs | `allow_recursion` 在 DelegateEngine 路径未贯通：引擎子代理无递归工具，且引擎路径恒注入根深度 1，深度上限形同虚设 | high |
| R2 | provider | API key 默认变量名 `DEEPSEEK_API_KEY` 与品牌前缀 `DEEPSEEKNOVA_` 不一致，命名悬而未决 | medium |
| R3 | README/BLOCKED | 截图空占位、npm 安装器承诺、Windows 沙箱排期三项无结论，账本悬空 | low |

### 3. 修复轮验证

- R1：`Agent` 新增 `run_stream_with_extensions`（本次运行临时注入扩展）；
  `DelegateEngine` 改为可后置注册子代理（`register_agent`），运行时
  `build_delegate_engine` 在 `allow_recursion=true` 时给每个子代理挂
  `RecursiveDelegateTool`（sink = 引擎自身），并在 `delegate_once` 注入
  每层真实 `DelegateDepth`；深度上限仍在 `run_at_depth` 守门，超深优雅降级。
- R2：默认变量名统一为 `DEEPSEEKNOVA_API_KEY`，旧名 `DEEPSEEK_API_KEY`
  仅作兼容回退（新名优先，显式配置的其它变量名缺失时报错不回退）；provider
  两处默认值 + README/README_EN 同步，补回退测试。
- R3：README 截图落地为真实 TUI PNG（`docs/screenshots/` + 生成脚本）；
  npm 承诺落地为 `npm/deepseeknova` 包
  （postinstall 下载 + SHA-256 校验 + bin 转发，release.yml 增 npm-publish）；
  Windows 沙箱落地为 `windows::JobSandbox`（Job Object，交叉编译通过，
  CI windows-latest 跑真实 spawn 测试）；BLOCKED 逐条落地。
- 复跑：聚焦测试全绿 + 全量 `make check` EXIT=0。

### 4. 结论

**0 critical / 1 high（R1 已修）/ 1 medium（R2 已修）/ 1 low（R3 已修）**。
上一轮遗留项全部闭环；剩余“docs/superpowers 内部任务书移出/保留”仍属领导
裁决范围。

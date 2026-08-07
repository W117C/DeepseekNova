# CLOSEOUT — 二轮审查修复（gh api 隐式 POST / 建议规则通配符 / shell 组合降级 / coordinator history，2026-08-06）

---

# CLOSEOUT — 日常体验包 + 遗留全清（web_search / LSP / auto / serve durable / ACP / eval，2026-08-07）

## 六事实面状态

| 事实面 | 状态 | 证据 |
|--------|------|------|
| 代码 | changed-and-verified | 分支 feat/semantic-retrieval；日常体验包 + OCR 7 发现修复 + ACP stdio 适配器 + eval 子命令 + LSP 端到端测试；`make check` EXIT=0（fmt / clippy -D warnings / 全 workspace 测试 / doctest / doc 零警告） |
| 运行态 | verified-current（局部） | `serve --acp` 进程级冒烟通过（initialize / session/new / close 协议响应正确，按会话 cwd 建 agent 并初始化 memory store）；真实 LLM 冒烟因 `DEEPSEEK_API_KEY` 缺失 blocked，未伪造凭据 |
| 文档 | changed-and-verified | README / GUIDE / CHANGELOG / README_EN / serve+cli README 同步 ACP 与 eval；PROGRESS 与 CLOSEOUT 追加本轮；neat-freak 前序已同步 DESIGN / AGENTS / BUILDING / tool README |
| 规则 | verified-current | AGENTS.md 无新增漂移；跨 crate 改动（cli/serve/tools/agent/provider）遵守 §1 推理专家协议并全量验证 |
| 记忆 | not-applicable | 平台记忆为 generated-read-only，本轮无授权写入 |
| 工作区 | cleaned | `docs/experiments/` + `scripts/experiments/` + `docs/superpowers/mockups/` 按用户批准删除；无 stash、无额外 worktree；改动已提交并推送，PR #72 |

## 遗留清单执行情况（用户批准“删除候选都删除，遗留都做一遍”）

- LSP 端到端测试：已做。`LspSession` 重构为可注入 stdin/stdout 的会话，
  fake in-memory server 覆盖 initialize → didOpen → 空诊断，断言 1.5s
  宽限内返回（不再等满 `timeout_secs`）。
- 最小 evals：已做。`deepseeknova-cli eval`（JSONL + `#` 注释 +
  `must_contain` 断言，md/json 报告），3 个单测。
- ACP 适配器：已做。`serve --acp` 支持 initialize / session/new /
  session/prompt / session/cancel / session/close；会话按 cwd 重建 agent 并
  共享多轮历史；`Ask` 权限 fail-closed；prompt 失败走 JSON-RPC error 信封；
  stdin EOF 取消在途任务并排空输出。in-memory duplex 往返测试 1 个。
- 真实冒烟：协议面完成（见运行态）；真实 LLM 调用 blocked（无 API key），
  待用户提供凭据后补 `deepseeknova-cli run` / `eval` 冒烟。
- 删除：3 个 mockups 已 `git rm`；未跟踪的 experiments 目录与其中 .DS_Store
  已删除。
- 提交/推送/PR：已完成，分支 feat/semantic-retrieval 推送至 origin，PR #72。

---

## 六事实面状态

| 事实面 | 状态 | 证据 |
|--------|------|------|
| 代码 | changed-and-verified | 分支 feat/semantic-retrieval；未提交 68 项（60 修改 + 7 新增 + 1 删除）；`make check` EXIT=0（fmt/clippy/全 workspace 测试/doctest/doc 零警告）；新增回归测试覆盖 gh api 隐式 POST、file `-C`、printenv、建议规则精确匹配、shell 组合 NotReadOnly、coordinator history 上限 |
| 运行态 | verified-current（局部） | 无部署/服务面；TUI 已在本机 Terminal 启动冒烟（`chat --tui` 正常渲染并可用真实 API key 进入）；实验运行态产物已删除（用户批准，见 2026-08-07 收尾） |
| 文档 | changed-and-verified | GUIDE（权限/TUI 快捷键/折叠/审批浮层）、CHANGELOG Unreleased（Changed/Fixed）、README/README_EN/DESIGN/SECURITY 已同步本轮语义；crates/REVIEW.md 追加二轮审查分节 |
| 规则 | changed-and-verified | AGENTS.md §5 新增 3 条防错清单（gh api 隐式 POST、建议规则通配符、shell 组合硬拒）；无其他规则漂移 |
| 记忆 | not-applicable | 平台记忆为 Codex generated-read-only，本轮无授权写入 |
| 工作区 | pending | 全部改动未提交、未 push、无 PR；docs/experiments/ + scripts/experiments/ 与 docs/superpowers/mockups/ 已于 2026-08-07 按用户批准删除；无 stash、无额外 worktree；无其他删除候选（dbg_status_test.rs 已按修复删除） |

## 审查修复闭环

- 审查范围：`ocr delegate preview` 工作区模式 55 可审查文件（8.5k 插入）；
  AGENTS.md 强制路由 open-code-review-delegate；人工重点审安全/权限/沙箱/provider/
  子代理/coordinator/TUI 审批路径。
- 审查结论：2 critical（gh api 隐式 POST、建议规则通配符放大）+ 3 high
  （file `-C`、裸 printenv、shell 组合硬拒不可覆盖）+ 1 medium（coordinator
  history 无界）+ 1 low（dbg 死文件）。
- 修复：全部落地并补回归测试；3 个并行子代理按文件分区执行，父级补 coordinator
  单条截断并做最终验收；修复后 `make check` EXIT=0。

## 遗留（如实）

- 未提交、未 push、无 PR（提交/推送由用户决定）。
- docs/experiments/ + scripts/experiments/（未跟踪实验交付物）已按用户批准删除，
  不在版本库中。
- 会话级：协作子代理槽位仍有历史残留（test_spawn 等），属会话进程，不占工作区；
  重启会话即释放。

---

# CLOSEOUT — 安全边界收尾（readonly 分类器 + 权限裁决 + 子代理执行 + 沙箱档位，2026-08-06 审查修复轮）

## 六事实面状态

| 事实面 | 状态 | 证据 |
|--------|------|------|
| 代码 | changed-and-verified | 分支 feat/semantic-retrieval；未提交 20 修改 + 2 新增（readonly.rs / sanitize.rs）；`make check` EXIT=0（fmt/clippy -D warnings/全 workspace 1108 passed/doctest/doc 零警告）；新增回归测试覆盖 date/hostname 写形态、gh `--show-token=true`、路径 `..` 逃逸、bubblewrap FullAccess、journalctl 写操作、deny 建议 |
| 运行态 | not-applicable | 无部署/服务面；本轮为本地库变更，聚焦测试 + 独立 harness 实测复现修复前后行为 |
| 文档 | changed-and-verified | GUIDE（sandbox 节改 `[sandbox] enabled`、权限示例）、README/README_EN（权限模型、17 工具、1108 tests）、DESIGN §九（三档沙箱/CheckVerdict/只读分类器）、CHANGELOG Unreleased（Added/Fixed）、SECURITY.md（readonly/CheckVerdict/sanitize） |
| 规则 | changed-and-verified | AGENTS.md §5 新增 4 条防错清单（多词前缀写形态、布尔 flag `=value`、路径 `..` 丢弃 + 既有 H1/执行层条目）；无其他规则漂移 |
| 记忆 | not-applicable | 无平台记忆写入 |
| 工作区 | pending | 全部改动未提交；分支未 push、无 PR；无 stash、无额外 worktree；期间出现的未跟踪 PRODUCT.md 已消失（非本轮产物，未处理） |

## 审查修复闭环
- 审查范围：`ocr delegate preview` 工作区模式 17 可审查文件（3.2k 插入），规则取 system 默认组；人工逐文件审 diff + 全量上下文。
- 审查结论：3 high（date/hostname 写形态误放行、gh token 布尔绕过、路径 `..` 逃逸）+ 5 medium（deny 建议误导、bubblewrap FullAccess 语义、GUIDE 工作区可写、journalctl 漏拒、子代理无 gate fail-closed）+ 1 flaky（并行测试临时目录撞名）。
- 修复：全部落地并补回归测试；修复后复跑独立 harness 确认三类安全用例由 ReadOnly/Allow 翻转为 NotReadOnly/Deny。

## 遗留（如实）
- 无未修问题；未提交、未 push、无 PR（提交/推送由用户决定）。
- BACKEND_AUDIT.md 为 2026-08-01 历史审计快照（测试数等已过期），未改写；docs/superpowers/plans 按项目惯例存档保留。

---

# CLOSEOUT — 记忆语义检索（embedder）最小闭环（2026-08-05 dev-loop 轮）

## 六事实面状态

| 事实面 | 状态 | 证据 |
|--------|------|------|
| 代码 | changed-and-verified | 分支 feat/semantic-retrieval；`make check` EXIT=0、workspace 0 failed（2 既有 ignored）；core 137+2 / provider 40 / config 35+18 / runtime 53 / tools 67+12+7 / cli 32 |
| 运行态 | verified-current | CLI 冒烟：`memory stats` 输出 `embedded=0`；`memory embed-backfill` 无 provider 时 attempted=0 ok=0 不 panic；反向验证红（1 failed）→ 绿（1 passed）；RemoteEmbedder 用真实本地 HTTP 服务端到端测（路径/Bearer/body/错误/解析） |
| 文档 | changed-and-verified | GUIDE 记忆节补 embedder 配置/回填/CLI；CHANGELOG Added；BLOCKED 两处「语义检索」转已做（代码图侧仍待裁决）；PROGRESS 回执 + 自验收清单逐条打勾 |
| 规则 | not-applicable | AGENTS.md 未改动（无新增约定必要） |
| 记忆 | not-applicable | 无平台记忆写入 |
| 工作区 | changed-and-verified | 分支工作树仅本轮 14 文件（12 修改 + 2 新增）；无未跟踪残留、无 stash |

## 本轮交付物核对（dev-loop 六件证据）
1. 任务书：docs/superpowers/plans/2026-08-05-semantic-retrieval-plan.md（六节齐全，≤4000 字符）
2. 测试全绿：`make check` EXIT=0；core 132→137（+2 集成）、provider 35→40、
   config 51→53、runtime 52→53、tools 66→67（另 12+7 集成不动）
3. 审查报告：crates/REVIEW.md 本轮分节——M1/M2（锁内 HTTP + fallback 语义）已修、
   L1（未知后端静默）已修、L2（测试替身重复）接受；修复后重审 0 新问题
4. 收尾报告：本文件
5. BLOCKED.md：无执行阻塞；local 后端、代码图侧语义检索、TUI 协议面板 / 计划载体 /
   多模型反思 / 记忆清理 UI 留待裁决
6. 提交：分支 feat/semantic-retrieval（不 push，是否提交/推送由用户决定）

## 遗留（如实）
- local embedder 未实现：配置 `embedder="local"` 会显式 warn 并回落 FTS（不会静默）。
- 代码图侧语义检索仍未做（BLOCKED 已记）。
- `RemoteEmbedder::embed` 是同步 trait 内 `block_on`：在 async 工具路径会阻塞一个
  worker 线程至多一个超时（默认 30s）；有独立 runtime + 超时兜底，已在 GUIDE 记录，
  未来可改为 async trait + spawn_blocking。

---

# CLOSEOUT — Protocol 增强收尾 + Graph Go 语言（2026-08-05 dev-loop 双域轮）

## 六事实面状态

| 事实面 | 状态 | 证据 |
|--------|------|------|
| 代码 | verified-current | 分支 feat/memory-lifecycle @ 4d47a6a；`make check` EXIT=0，workspace 0 failed（2 既有 ignored）；graph 42 / metrics 21 / runtime 52 / agent 231 / cli 32 / core 132 |
| 运行态 | verified-current | 三轮反向验证全部真红→真绿（task_rate 断言、Go fixture 断言、R1 refs 归属断言）；G 域 grammar 节点名与 tree-sitter-go 0.25.0 node-types.json 逐项核对一致 |
| 文档 | changed-and-verified | GUIDE 协议节（task_rate/record_use）+ A3 节（Go 语言）；graph README 语言列表；CHANGELOG Added/Fixed；PROGRESS 双域回执 + 自验收清单已打勾；BLOCKED 已更新（两域转已做、阻塞解除、新增待裁决四项） |
| 规则 | not-applicable | AGENTS.md 未改动（无新增约定必要） |
| 记忆 | not-applicable | 无记忆库/记忆代码改动 |
| 工作区 | changed-and-verified | 分支工作树干净；crates/REVIEW.md 含第三/四轮审查 + 修复轮 ×2 记录（覆盖声明、真实行号评论、修复证据）；无未跟踪残留 |

## 本轮交付物核对（dev-loop 六件证据）
1. 任务书 ×2：docs/superpowers/plans/2026-08-05-protocol-followup-plan.md、2026-08-05-graph-go-plan.md（六节齐全，存档保留）
2. 测试全绿：make check EXIT=0；graph 32→42、metrics 17→21、runtime 48→52（+8 新测试，零放宽既有断言）
3. 审查报告：crates/REVIEW.md 第三轮（G 域：1 medium + 4 low）、第四轮（P 域：5 low）、修复轮 ×2 记录、第二轮复核（发现 R1 修复引入缺陷并定位）
4. 收尾报告：本文件
5. BLOCKED.md：无执行阻塞；待裁决四项（TUI 协议面板 / 结构化计划载体 / 多模型反思对比 / 记忆清理 UI）+ 既有（agent_loop 反思 UI、AST 全量持久化、MCP 外壳、语义检索）
6. 提交：285fe60（G 域）+ 95f695e（P 域）+ 7f49ffc（修复轮 1）+ 4d47a6a（修复轮 2），分支未 push

## 审查循环质量说明
- 第一轮两域审查 0 high；修复轮 1 修 4 项（G-M1 分组类型、G-L4 go.mod 尾注释、P-L2 回填语义、P-L4 warn 区分）
- 第二轮复核发现 G-M1 修复引入 refs 归属缺陷（R1，/tmp 实测铁证：伪边 B→A），修 R1/R2（成员逐个入栈 + doc/signature 恢复）
- 最终复核 4 项核对全过（push_entity 语义等价、push/pop 严格 1:1、type_alias 跳过、doc 回退两形态），42 passed，无新问题

## 遗留（如实）
- 无。已知待裁决项全部在 BLOCKED（见上），非本轮范围。

---

## CLOSEOUT — 依赖健康修复（2026-08-05 收尾轮）

### 六事实面状态

| 事实面 | 状态 | 证据 |
|--------|------|------|
| 代码 | changed-and-verified | 未提交工作区 13 文件；`make check` EXIT=0（fmt/clippy/全 workspace/doctest/doc 零警告）；`make audit` ok |
| 运行态 | not-applicable | 无部署/服务面，本轮仅本地构建与测试验证 |
| 文档 | changed-and-verified | CHANGELOG Fixed 条目；telemetry lib.rs 与 README 同步为 tracing-only；README/GUIDE/BUILDING 无版本级过期表述 |
| 规则 | changed-and-verified | AGENTS.md §5 新增「并行测试临时目录撞名」防错条目；deny.toml skip/ignore 与 CI security.yml 同步（lru/paste 豁免移除） |
| 记忆 | not-applicable | 无平台记忆写入 |
| 工作区 | pending | 13 文件未提交（分支 feat/memory-lifecycle），无未跟踪残留、无 stash、无临时/备份文件；提交与否待用户决定 |

### 本轮交付物核对

1. 依赖升级：OTel 0.27→0.32（trace-only）、ratatui 0.29→0.30、crossterm 0.28→0.29、rand 0.8→0.9、thiserror 1→2、criterion 0.5→0.8；重复依赖警告 16 组→0（剩余 5 组上游分叉登记 skip）。
2. 测试全绿：`make check` EXIT=0；scanner flaky（纳秒撞名）改 `tempfile::tempdir()` 后连跑 3 次全过。
3. 审查报告：crates/REVIEW.md 本轮分节（1 medium C1 → 已修 → 重审 0 问题）。
4. 安全：`make audit` ok；`cargo audit` 仅 1 项已登记允许警告（RUSTSEC-2025-0068）。

### 遗留（如实）
- 未提交：13 个文件（含 Cargo.lock），待用户决定 commit / push。
- 无未消除的构建或审计 warning；BLOCKED 待裁决项均非本轮范围。

---

# CLOSEOUT — 观测台前端 UI + TUI 演进（2026-08-07 dev-loop 轮）

## 六事实面状态

| 事实面 | 状态 | 证据 |
|--------|------|------|
| 代码 | changed-and-verified | 提交 `fdefbd9`；桌面端纯前端脚手架（Vite+SolidJS+TS+Tailwind4）首屏实现 A×B 合并构图；TUI 五项演进（浅色档 token / 夜次分组+星点 / 审批风险标签+mono 命令 / 测光评分卡+/scorecard / 欢迎卡圆顶字形）；permission 新增 `shell_readonly_kind`，agent Ask 描述携带风险标签；最终 `make check` EXIT=0（fmt / clippy -D warnings / 全 workspace / doctest / doc 零警告） |
| 运行态 | verified-current（前端） | `npm run build` EXIT=0；`vitest run` 14/14 绿；`vite preview` + Chrome 截图 `obs-comp-d-desktop-p1.png`（1536×1024），Agnes 视觉核对双带/侧栏/对话流正常 |
| 文档 | changed-and-verified | GUIDE（配色/上下文占用/斜杠命令）、CHANGELOG、BUILDING（前端构建）、AGENTS.md（桌面端非 cargo 说明）、BLOCKED、REVIEW、PROGRESS、任务书 `docs/superpowers/plans/2026-08-07-frontend-tui-plan.md` |
| 规则 | verified-current | AGENTS.md 无新增防错条目需求（跨 permission/agent/tui 改动按 §1 记录于 REVIEW 覆盖声明）；未新增依赖（前端 devDeps 属脚手架） |
| 记忆 | not-applicable | 平台记忆 generated-read-only，无写入 |
| 工作区 | changed-and-verified | 已提交 `fdefbd9`（含上一轮 P0 未提交改动 + 设计资产 + 本轮）；未 push（由用户决定）；`repro_tmp.rs` 用户调试文件未提交、保留；`obs-comp-d-combined-agnes-v2.png` 为删除候选（未确认不删）；无 stash |

## 本轮交付物核对（dev-loop 六件证据）
1. 任务书：`docs/superpowers/plans/2026-08-07-frontend-tui-plan.md`（六节齐全，≤4000 字符）。
2. 测试全绿：最终 `make check` EXIT=0（真实输出见 PROGRESS A2）；permission 35、
   agent 239、tui 154+1 doctest；前端 vitest 14；反向验证 TUI + 前端均真红→真绿。
3. 审查报告：`crates/REVIEW.md` 本轮分节——1 high（R1 TUI 审批风险标签接线）已修、
   1 medium（端到端断言盲区）记录接受、2 low 接受；修复后全量 `make check` EXIT=0。
4. 收尾报告：本文件。
5. BLOCKED.md：Tauri 壳 P1 / 桌面后续页 / 文案语言 / logo / 风险接线 e2e /
   repro_tmp.rs / agnes-v2 删除候选；无执行阻塞。
6. 提交：`fdefbd9`（不 push，是否推送由用户决定）。

## 遗留（如实）
- Tauri 壳（P1）与桌面后续页面（P3/P4）未做，BLOCKED 已记。
- `repro_tmp.rs` 为用户调试文件，保留未提交；`obs-comp-d-combined-agnes-v2.png`
  为被否决稿备份（删除候选，未确认不删）。
- 风险标签缺少 agent 集成级“responder 收到前缀”断言（R2，记录接受）。
- 未跑 `make audit`（cargo-deny 未预装时目标会提示；本轮以 `make check` 为验收）。

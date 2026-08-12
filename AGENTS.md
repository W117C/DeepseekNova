# AGENTS.md — DeepseekNova 项目 Agent 指令

> **本文件是项目级 Agent 工作指令。每次在此项目中工作时，必须优先遵守。**

---

## 1. 工作模式：推理专家协议

本项目以**推理专家协议**作为高风险任务的工作模式。协议**不是**对所有任务无条件生效，按以下范围划分：

### 1.1 适用范围（触发条件）

满足**任一**条件时，必须启用完整的推理专家协议：

- **跨 crate 变更**：改动涉及 2 个及以上 crate，或修改公开 API / 跨 crate 依赖关系
- **高风险区域**：涉及 `deepseeknova-security`、`deepseeknova-permission`、`deepseeknova-sandbox` 的安全边界、路径检查或策略逻辑
- **架构级决策**：新增 crate、调整模块边界、修改核心 trait / 事件流 / 运行器契约
- **复杂问题定位**：多次尝试未解决的 bug、并发/异步相关缺陷、根因不明的行为异常
- **不可逆或对外影响**：发布、迁移、破坏性变更、删除数据或文件

触发时的核心强制性要求：

| 阶段 | 要求 |
|------|------|
| **错误预扫描** | 进入任何问题前，先设立禁行区 |
| **思考透明化** | 最终答案前展示完整推理链路，不得跳跃 |
| **自我质疑** | 形成判断后主动寻找反例、漏洞、失效边界 |
| **多路径探索** | 至少构想两条本质区别的解决路径 |
| **错误复现拦截** | 输出前逐条审计，发现复现立即回退修正 |
| **置信度声明** | 每次结论必须给出高/中/低置信度及核心假设等级 |

### 1.2 豁免范围与轻量路径

以下简单任务**豁免**完整协议，走轻量路径：

- 单文件、单 crate 内的小改动（如修 typo、改注释、调格式、补文档）
- 纯信息性问答、代码阅读与解释、状态查询
- 机械性操作（运行既有命令、重命名、按明确指令做的直接修改）

轻量路径只需：理解意图 → 执行 → 用最小验证手段确认结果（如编译通过、命令输出符合预期），无需展示完整推理链路或多路径探索。

**升级规则**：轻量路径执行中一旦发现任务实际触及 1.1 中任一条件（如改动扩散到其他 crate、暴露安全隐患），立即升级为完整协议，不得继续按轻量路径处理。

---

## 2. 项目简介

DeepseekNova 是一个 Rust 编写的 AI Agent 框架，包含 21 个 crate。主要结构：

```
crates/
├── deepseeknova-cli/          # CLI 入口
├── deepseeknova-agent/        # Agent 运行时（协调器、子代理、记忆、质量钩子、失败诊断）
├── deepseeknova-core/         # 核心类型（事件、图谱、规划器、注册表、执行器、运行器、工具；身份/前缀/插件模块已于 2026-08-07 死代码清理移除）
├── deepseeknova-config/       # 配置管理（[protocol] 段：enabled/gates/adversarial_review）
├── deepseeknova-provider/     # LLM 提供商（Anthropic、OpenAI）
├── deepseeknova-tools/        # 工具集（fs、grep、shell、memory、web_fetch、todo）
├── deepseeknova-mcp/          # MCP 协议客户端
├── deepseeknova-metrics/      # 会话效能度量与评分卡（SessionMetrics / Scorecard，含 protocol/composite 协议维）
├── deepseeknova-context/      # 上下文管理
├── deepseeknova-runtime/      # 运行时编排
├── deepseeknova-permission/   # 权限系统
├── deepseeknova-graph/        # 代码图检索引擎（tree-sitter + SQLite FTS5 + PageRank + repo map）
├── deepseeknova-checkpoint/   # 检查点
├── deepseeknova-store/        # 存储层
├── deepseeknova-security/     # 安全审计、路径检查、策略、质量规则（QualityPolicy）、失败模式库（failure_pattern）、shell 只读分类（readonly）、子代理输出净化（sanitize）
├── deepseeknova-scanner/      # 安全扫描（deepsec 式静态规则 + AI 调查 + 报表）
├── deepseeknova-sandbox/      # 沙箱（bubblewrap、seatbelt、Windows Job Object）
├── deepseeknova-skills/       # 技能加载（含 fitness 生命周期：使用/成功记录、进化建议、deprecated 过滤）
├── deepseeknova-telemetry/    # 遥测
├── deepseeknova-serve/        # HTTP 服务（含会话诊断/评分卡端点）
├── deepseeknova-tui/          # TUI
```

> 另有 `npm/deepseeknova` npm 安装包（postinstall 从 GitHub Releases 下载
> 平台二进制 + SHA-256 校验，版本经 bump 脚本同步）与 `scripts/tui-screenshot.py`
> （README 截图生成脚本）。

> 桌面端前端工程（`crates/deepseeknova-desktop`）已于 2026-08-08 整体移除
> （非 cargo 脚手架，不在 21 crate 清单内；历史可经 git 追溯，先例
> `3ab55d7`）。当前仓库为纯 Rust 21-crate workspace。

---

## 3. 常用命令

```bash
make build          # 编译全部
make check          # CI 等价检查（fmt + clippy + test + doc）
make test           # cargo test --all
make fmt            # 格式化代码
make clippy-fix     # clippy 自动修复
make audit          # 安全审计（先检查 cargo-deny，再执行 cargo deny --all-features check）
make test-count     # 在 Linux 上运行 cargo test --all，按 passed 总数同步 README 测试数（非 Linux 拒绝覆盖）
make test-count-check  # 校验 README 测试数与 Linux CI 的 passed 总数一致（CI 已接入；非 Linux 本地跳过比对）
make bench-ci       # 运行 workspace 全部基准，criterion 以命名基线 "ci" 保存到 target/criterion
                    # 并打包 target/bench-ci/bench.tar.gz（CI bench job 复用并上传 artifact 供人工对比；
                    # 无自动门禁阈值，避免机器噪声 flaky）
```

> **云端安全审查不可用时的回退验收路径**：交付/推送前若云端安全审查（如 L3 深度安全审查）因外部资源不可用（如积分耗尽）暂时无法执行，先以项目内既有手段留存验收证据：运行 `make check` 与 `make audit`，记录两者结果与待补审查项，待服务恢复后补跑云端审查，不因此新增脚本、修改 CI 或引入新工具。注意 `make audit` 配方会先检查 `cargo-deny` 是否安装，然后直接执行 `cargo deny --all-features check`（与 CI `.github/workflows/security.yml` 的 cargo deny 任务对齐；本地缺 cargo-deny 时目标会打印安装提示并退出）。CI 侧另有带 RUSTSEC ignore 清单的 cargo-audit 任务，推送后由 security.yml 自动覆盖，ignore 理由见 `deny.toml` 的 `[advisories].ignore`。

> **核心变更影响分析（core-change-watch）**：本项目的核心代码边界定义在 `.better-harness/core-code`，该边界**仅在 `--languages auto` 下生效**。工具默认语言集不含 Rust，缺省运行会在边界匹配前过滤掉 `.rs` 文件，导致核心命中与 `reviewRecommended` 判定失真。因此任何核心变更审查必须使用 `core-change-watch evidence-pack --languages auto`；默认（无 `--languages` 参数）运行的结果不得作为核心审查依据。

> **core-change-watch 历史路径映射（rename 不跟随）**：该工具的历史扫描底层是 `git log -N --numstat`，**不跟随重命名**（CLI 无 `--follow` / rename 类选项，已核实 `evidence-pack` 全部参数：`--cwd` / `--languages` / `--base-ref` / `--max-commits` / `--history-windows` / `--ignore` / `--no-history` / `--core-code` / `--measure-source-lines` 等，均无此能力）。本仓库经历过三次全量重命名：`crates/reasonix-*` → `crates/dpronix-*`（69509d5）→ `crates/deepnova-*`（8a95226）→ `crates/deepseeknova-*`（c5336db），因此历史输出（`historyProfile.hotFiles`、`followUpActions` 等）中会出现旧前缀路径。消费这些输出前必须执行以下映射与校验：
> 1. **前缀映射**：将路径前缀 `crates/reasonix-`、`crates/dpronix-`、`crates/deepnova-` 统一替换为 `crates/deepseeknova-`；同一文件在新旧路径下的提交计数应按映射后路径**合并**统计。注意 `hotFiles` 是 top-N 截断列表，新前缀段的提交可能因未达入榜门槛而不在列表中，仅基于 `hotFiles` 合并会低估；需要精确计数时直接用 `git log --follow -- <当前路径>` 核对。
> 2. **存在性校验**：映射后仍需确认文件在当前工作树中真实存在；映射后依然缺失的条目属于历史输出相对当前结构的正常滞后（例如 `crates/deepseeknova-desktop` 已于某提交被整体移除），应改指其后继文件或丢弃，不得据此创建文件。

> **changeDrift 文档同步口径**：本项目中 `Makefile` / `Cargo.toml` 等构建配置的**文档伴随文件是 `AGENTS.md` 与 `BUILDING.md`**（`BUILDING.md` 的本地验证说明指回 `AGENTS.md`），而非各 crate 的 README。消费 `core-change-watch` 的 changeDrift 输出时，若 advisory 提示构建配置变更缺少文档同步，应核对 `AGENTS.md`（§3 常用命令）与 `BUILDING.md` 是否已与 Makefile 目标一致；一致即判定为**已同步**，不得据此去补写无关 README。构建配置变更时也应同步更新这两个文件。

### 3.1 深入文档与调试路由

需要更深背景或定位问题时，按用途路由到以下文档（本节仅作索引，不改变 §1 分档协议与上述命令面）：

- [DESIGN.md](DESIGN.md) — **架构设计记录（架构史）**，含命名现状权威说明与四层分层设计意图；**非 Agent 操作指令**，文中标注 [规划中/未实现] 的能力不得当作现行能力引用
- [BUILDING.md](BUILDING.md) — 编译环境与系统依赖（Linux/macOS 原生库安装），及 clone 后的 Git rename 跟踪配置
- [GUIDE.md](GUIDE.md) — 用户指南：核心概念（Runner/Tool）、配置、工具参考、HTTP API、TUI、MCP、沙箱等

最小调试入口：

- **CLI**：日志经 `tracing` 输出到终端（不读 `RUST_LOG`；TUI 模式 OFF、非 TUI chat 为 WARN、其余命令 INFO，见 `crates/deepseeknova-cli/src/main.rs`；启用 `[telemetry] enabled=true` 时改装 OTLP 管线、日志经 OTLP 导出，终端不再打印 INFO 文本，属刻意权衡）；运行时派生数据在工作区 `.deepseeknova/`（`graph.db` 代码图索引、`memory.db` 记忆库）；配置层级为 `~/.deepseeknova/config.toml`（用户）+ `./deepseeknova.toml`（项目）；release 产物在 `target/release/deepseeknova-cli`。任务质量闭环（ToolHook 链 + 写后策略评估 + 诊断/评分卡）由 `[quality] enabled`（默认 true）控制，见 GUIDE.md；评分卡与诊断报告落盘于工作区 `.deepseeknova/metrics/`。聚焦测试：`cargo test -p <crate> <测试名过滤词>`

---

## 4. 代码约定

- 使用 `cargo fmt` // Rust 标准格式
- 所有公开 API 必须有文档注释（`///` 或 `//!`）
- 新功能必须附带测试（单元测试或集成测试）
- 错误处理优先使用 `thiserror` / 自定义错误类型而非 `anyhow`
- 对跨 crate 变更，运行 `make check` 确保不引入破坏

---

## 5. 错误档案管理

如果在工作中发现本项目特有的重复错误模式，请将其加入防错清单，格式为：

```
- [错误描述]：<具体表现>
- [如何避免]：<可操作预防措施>
```

已归档条目（2026-08-05 收尾）：

- [worker 并发遗漏全局格式化]：多 worker 并行改代码后，未跑 `cargo fmt` 全文件的改动会导致 make check 的 fmt 阶段失败
- [如何避免]：父级收尾验收前统一 `cargo fmt`；worker 约定"不自查格式则父级兜底"
- [测试注入掩盖真实回归]：B 阶段为修复 A 引入的 review 短路回归，给测试注入 BlockingFindingHook 掩盖症状而非修正门条件
- [如何避免]：发现回归先修生产代码语义；测试注入桩只能作为验证手段，不能替代修复；修复后必须留"无桩场景"回归测试
- [决策前未核实依赖来源]：lru 升级任务假设可单独升级，实际是 ratatui 0.29 的传递依赖（^0.12 约束），升级被约束卡住
- [如何避免]：依赖升级前先 `cargo tree -i <crate>` 核实来源与约束
- [文档注释契约与实现漂移]：core trait 注释写 fail-open，实现改为 fail-closed，靠审查才发现契约已变更
- [如何避免]：契约变更必须同步注释；跨 crate 契约除 core-change-watch 意识外需显式人工核对
- [并行 worker 半成品阻塞全局]：worker 中断时可能留下编译错误的半成品文件，阻塞其他 worker 验证
- [如何避免]：worker 提交前必须自验编译；父级发现其他 worker 阻塞时立即协调
- [批量替换误伤上下文]：replace_all 模式替换会误伤同形不同义的调用点（如参数 vs 解引用）
- [如何避免]：批量替换前先核对每个命中点的上下文
- [并行测试临时目录撞名]：测试用纳秒时间戳拼临时目录名，并行执行时可能撞名，
  一个测试删除目录导致另一个测试扫描到空结果（flaky panic 在 unwrap）
- [如何避免]：测试临时目录统一用 `tempfile::tempdir()`（持有 TempDir 自动清理），
  不要用时间戳或固定路径
- [文档注释交叉引用缺前缀]：子模块 doc 注释里写 `[`SandboxTier::X`]` 而未 `use`
  该类型时，rustdoc 报 "no item named"，make check 的 doc 阶段不阻断但积累警告
- [如何避免]：doc 注释交叉引用一律用完整路径 `[`crate::Type::X`]`，改完跑
  `cargo doc --no-deps` 确认零警告
- [用户可见文案与测试断言耦合]：升级阻断/报错文案（如 permission deny 信息）时，
  既有测试断言旧字符串导致失败——本轮 `agent_permission_gate_denies_tool` 即中招
- [如何避免]：改动用户可见文案前先 grep 测试中的文案断言；断言应验证新契约
  （原因+建议），而非只改字符串
- [任意参数安全表混入命令执行器]：readonly 分类器把 `env`/`find`/`xargs` 列入
  "任意参数安全"，导致 `env rm -rf /`、`find . -exec rm {} +` 被免询问放行
  （双审查同时标 critical）
- [如何避免]：凡能启动其他程序/执行命令的工具（env/xargs/find -exec、git
  submodule foreach 等）一律降级为逐 flag 白名单或显式拒绝，绝不进
  "任意参数安全"表；分类器补"执行器"回归测试
- [注释声称的强制与实际实现脱节]：`with_frozen_denies` 注释写"执行层仍由共享
  PermissionGate 强制"，但 SubAgentRunner 路径根本没有执行层（工具调用被丢弃，
  子代理死循环到 max_steps），审查才暴露
- [如何避免]：跨层强制（prompt 层 + 执行层）必须两端都落地再写注释；写完注释
  立即 grep 确认执行路径真实存在 gate/强制点，而非只凭构建意图
- [新执行段漏 replay 不变量]：C2 给 SubAgentRunner 补工具执行后，只写 Role::Tool
  消息不写 assistant(tool_calls)，ValidatedRequest 把孤儿 Tool 结果判为
  OrphanToolResult → 子代理任何工具调用后硬失败（P0 级功能回归）
- [如何避免]：补工具执行循环时，先对照主 agent 路径确认消息序列不变量
  （assistant(tool_calls) → 逐条 Tool 结果）；新执行段必须带 replay 级测试，
  不能只测到 gate 决策
- [H1 修一方向漏另一方向]：readonly 免询问只对 deny 移到策略后，ask 规则仍被
  只读短路（用户 ask: [Bash("git *")] 对 git status 失效）——规则优先级语义
  必须对称处理 deny/ask 两个方向
- [如何避免]：调整权限判定顺序时枚举全部规则表（deny/ask/allow/mode）的交互，
  每个方向补正反两例测试；修复 H1 类问题后立即复查对称方向
- [多词"任意参数安全"前缀含写形态]：readonly 表把 `date -u`/`hostname -f`
  等带固定参数条目按 `starts_with` 放行，导致 `date -u -s ...`、
  `hostname -f newname` 被免询问执行——"固定参数"不等于"后续参数任意"
- [如何避免]：凡命令可因后续位置参数/flag 转为写操作（date/hostname/timedatectl），
  一律精确 argv 匹配或专项白名单，禁止前缀匹配进任意参数表
- [布尔 flag 的 `=value` 形态绕过精确拒绝]：`gh auth status --show-token=true`
  未被 `a == "--show-token"` 命中，token 泄露进 transcript
- [如何避免]：拒绝/放行布尔 flag 时先归一化 `--flag=value` 与短 flag `=value`
  形态，再按值判定；补 `=true`/`=false` 正反测试
- [路径祖先回溯丢弃 `..` 分量]：`is_within_workspace` 对
  `root/missing/../../outside` 向上回溯时 `file_name()` 对 `..` 返回 None，
  分量被丢弃后误判为工作区内；配合 create_dir_all 可真实逃逸
- [如何避免]：回溯剩余段必须显式保留 ParentDir 分量并在拼接后词法折叠；
  路径守卫补"不存在的中间目录 + `..` 逃逸"回归测试
- [gh api 隐式 POST 被当只读放行]：`gh api` 未显式 `--method` 时被分类器判为
  只读，但带 `-f`/`-F`/`--input` 时 gh 会自动切 POST，创建/更新资源免询问执行
- [如何避免]：gh api 出现写 payload flag 且无显式 GET 时一律归 NotReadOnly，
  补隐式 POST 正反测试
- [拒绝即教育建议把通配符当 glob 放大]：`rm *` 被 `simple_glob_match` 按前缀
  解释，建议规则可放行 `rm -rf /`；中间 glob（`rm *.tmp`）则匹配不上原命令
- [如何避免]：建议规则一律精确匹配（Rule.exact 字面量相等），用户配置规则
  保持 glob 语义；补 `rm *`/`rm *.tmp` 正反回归测试
- [常规 shell 组合硬拒且不可覆盖]：链式/重定向/命令替换判 Dangerous 后
  allow 规则无法放行，`cat a | head`、`echo x > file` 等常规命令不可执行
- [如何避免]：普通 shell 组合归 NotReadOnly 走权限审批/规则；Dangerous 仅保留
  工具级注入面（git -c/--config-env、格式串注入、UNC/URL/SMB 路径形态）
- [missing-docs lint 阻断 make check]：core/event/metrics 启用
  `#![warn(missing_docs)]` 后，新增 pub 项（struct/enum/fn/trait/字段/变体/方法/
  `pub mod`）未补 `///` 文档注释，`make check` 的 clippy 阶段（`-D warnings`）
  与 doc 阶段（`RUSTDOCFLAGS="-D warnings"`）会失败
- [如何避免]：新增/改动 pub 项时同步补全 `///` 中文文档注释；doc 注释中的交叉
  引用用完整路径 `[`crate::Type`]` 或纯反引号文字，避免 broken intra-doc link；
  本地 `make check` 通过后再提交
- [reqwest 测试被 HTTP_PROXY 环境变量劫持]：开发机设置了 `HTTP_PROXY`/
  `HTTPS_PROXY`（如 `127.0.0.1:7890`），reqwest 默认尊重这些变量，把对本地
  mock server（`127.0.0.1:随机端口`）的请求转发到代理，代理无法连本地端口导致
  `Connection refused (os error 61)` 或 hang 到超时。此问题跨 4 crate 出现：
  mcp（15 测试）、provider/openai（1）+provider/embeddings（9）、runtime（1）、
  tools/docs_tools（3）
- [如何避免]：凡测试用 `TcpListener::bind("127.0.0.1:0")` 起 mock server 且用
  reqwest::Client 连接的，测试开头必须 `let _guard = ENV_LOCK.lock().await;
  clear_proxy_env();`（同步测试用 `blocking_lock()`）；同一 crate 内的 ENV_LOCK
  必须共享同一把 `tokio::sync::Mutex`（不可一个模块用 `tokio::sync::Mutex`、
  另一个用 `std::sync::Mutex`——两把不同的锁不互相同步，`std::env` 并发修改
  是 UB）；新增 reqwest mock 测试时 grep 同 crate 是否已有 `clear_proxy_env`，
  有则复用，无则在 crate 级 test_util 模块新增
- [下游 `From<TheirTypedError> for DeepseeknovaError` 放错 crate]：把 impl 写在
  core 中会因 orphan rule 失败（core 不拥有 `TheirTypedError`），或写在错误的
  下游 crate 中导致 `?` 在调用点找不到 `From` 实现
- [如何避免]：所有 `From<TheirTypedError> for DeepseeknovaError` 必须放在
  **拥有 `TheirTypedError` 的 crate** 中（orphan rule：impl 可放在拥有 trait
  或任一类型的 crate，`From` 来自 std 所以只能放在拥有 `TheirTypedError` 的
  crate）；当前已落地：`graph`、`provider`、`permission`、`context`、`agent`
  crate 的 `From<TheirError>`
- [`?` 单步解析导致未注册的外部错误编译失败]：`Result<_, reqwest::Error>` 上
  用 `?` 转入 `Result<_, DeepseeknovaError>` 会编译失败，因为 `?` 仅查找一个
  `From` 实现，不进行多步链式转换（anyhow 已于 Phase 4 移除，不再有
  `reqwest::Error → anyhow::Error → DeepseeknovaError` 的桥接路径）
- [如何避免]：调用点对未在 `DeepseeknovaError` 注册 `From` 的外部错误类型，
  必须显式 `.map_err(DeepseeknovaError::from)?` 或归类到具体变体
  （如 `.map_err(|e| DeepseeknovaError::Tool(e.to_string()))?`）；不要假设
  `?` 会自动多步转换
- [GitHub Actions job 级 if 引用 secrets]：`secrets` context 在 job 级 `if`
  中不可用，表达式求值报 `Unrecognized named-value: 'secrets'`，tag 发布
  workflow 必失败（release.yml npm-publish 曾中招）
- [如何避免]：token 经 job 级 `env` 透传，用 step 级 `if: env.X != ''` 门控；
  不要把 `secrets` 写进 job/step 的 `if` 表达式
- [cfg 兜底分支未排除新增平台]：新增 `#[cfg(windows)]` 分支后，既有
  `#[cfg(not(any(macos, linux)))]` 兜底在 Windows 仍编译，两个块并存导致
  E0308（Windows JobSandbox 后端曾无法构建）
- [如何避免]：新增平台分支时同步把该平台加进所有兜底 `not(any(...))` 条件；
  用 `rustc --target <平台>` 交叉编译定向复现
- [发布产物版本双源不同步]：`npm/deepseeknova/package.json` 写死版本，bump
  脚本只改 Cargo.toml，发布 tag 与 npm 包版本脱节（npm publish 撞旧版本或
  下载错误资产）
- [如何避免]：bump 脚本同步改写 npm 包 manifest；CI 发布前用 tag 派生
  `npm version --no-git-tag-version`

---

## 6. 自检清单

自检清单的适用范围与 §1 一致，按任务类型分档执行：

**触发 §1.1 完整协议的任务**，回答前逐项检查：
1. 我是否在代码变更后实际运行了受影响的最小验证命令？
2. 我是否在思考轨迹中展示了完整推理？
3. 我是否找到了至少一个反例/失效场景？
4. 我是否给结论标注了置信度？

全部通过 → 输出。任何一项否 → 先修正再输出。

**§1.2 豁免范围内的轻量任务**，只需确认两点：
1. 改动/回答是否与用户意图一致？
2. 结果是否经过了最小验证（或明确说明未验证）（是否实际运行了 make check 或聚焦测试命令）？

轻量任务不强制反例搜寻与置信度声明；若执行中触发了升级规则，则改用完整清单。

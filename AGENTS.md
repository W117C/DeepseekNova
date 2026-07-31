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
├── deepseeknova-agent/        # Agent 运行时（协调器、子代理、记忆）
├── deepseeknova-core/         # 核心类型（事件、图谱、身份、规划器、前缀树、注册表、执行器、运行器、工具、插件）
├── deepseeknova-config/       # 配置管理
├── deepseeknova-provider/     # LLM 提供商（Anthropic、OpenAI）
├── deepseeknova-tools/        # 工具集（fs、grep、shell、memory、web_fetch、todo）
├── deepseeknova-mcp/          # MCP 协议客户端
├── deepseeknova-context/      # 上下文管理
├── deepseeknova-runtime/      # 运行时编排
├── deepseeknova-permission/   # 权限系统
├── deepseeknova-event/        # 事件系统
├── deepseeknova-graph/        # 代码图检索引擎（tree-sitter + SQLite FTS5 + PageRank + repo map）
├── deepseeknova-checkpoint/   # 检查点
├── deepseeknova-store/        # 存储层
├── deepseeknova-security/     # 安全审计、路径检查、策略
├── deepseeknova-sandbox/      # 沙箱（bubblewrap、seatbelt）
├── deepseeknova-skills/       # 技能加载
├── deepseeknova-telemetry/    # 遥测
├── deepseeknova-serve/        # HTTP 服务
├── deepseeknova-tui/          # TUI
├── deepseeknova-desktop/      # Tauri 桌面端
```

---

## 3. 常用命令

```bash
make build          # 编译全部
make check          # CI 等价检查（fmt + clippy + test + doc），不含 deepseeknova-desktop
make check-desktop  # 桌面端本地验证（前端 lint/test + 桌面端 Rust check/clippy/test）
make test           # cargo test --all
make fmt            # 格式化代码
make clippy-fix     # clippy 自动修复
```

> **注意**：`make check` 与 CI 的主要 Rust 检查任务（check / clippy / test / coverage / bench / docs）均通过 `--exclude deepseeknova-desktop` 排除桌面端 crate（CI 由独立的 `check-desktop` 与 `frontend` 任务单独覆盖）。改动 desktop 相关代码后，请在本地额外运行 `make check-desktop` 作为替代验证；该目标会按序执行前端 `npm run lint`（tsc --noEmit）、`npm test`（node --test test/），以及 `cargo check` / `cargo clippy --all-targets -- -D warnings` / `cargo test`（均 `-p deepseeknova-desktop`），与 CI 对齐。其中 Rust 侧编译需要前端产物 dist/，缺失时请先运行 `make frontend`。

> **核心变更影响分析（core-change-watch）**：本项目的核心代码边界定义在 `.better-harness/core-code`，该边界**仅在 `--languages auto` 下生效**。工具默认语言集不含 Rust，缺省运行会在边界匹配前过滤掉 `.rs` 文件，导致核心命中与 `reviewRecommended` 判定失真。因此任何核心变更审查必须使用 `core-change-watch evidence-pack --languages auto`；默认（无 `--languages` 参数）运行的结果不得作为核心审查依据。

> **core-change-watch 历史路径映射（rename 不跟随）**：该工具的历史扫描底层是 `git log -N --numstat`，**不跟随重命名**（CLI 无 `--follow` / rename 类选项，已核实 `evidence-pack` 全部参数：`--cwd` / `--languages` / `--base-ref` / `--max-commits` / `--history-windows` / `--ignore` / `--no-history` / `--core-code` / `--measure-source-lines` 等，均无此能力）。本仓库经历过三次全量重命名：`crates/reasonix-*` → `crates/dpronix-*`（69509d5）→ `crates/deepnova-*`（8a95226）→ `crates/deepseeknova-*`（c5336db），因此历史输出（`historyProfile.hotFiles`、`followUpActions` 等）中会出现旧前缀路径。消费这些输出前必须执行以下映射与校验：
> 1. **前缀映射**：将路径前缀 `crates/reasonix-`、`crates/dpronix-`、`crates/deepnova-` 统一替换为 `crates/deepseeknova-`；同一文件在新旧路径下的提交计数应按映射后路径**合并**统计。注意 `hotFiles` 是 top-N 截断列表，新前缀段的提交可能因未达入榜门槛而不在列表中，仅基于 `hotFiles` 合并会低估；需要精确计数时直接用 `git log --follow -- <当前路径>` 核对。
> 2. **存在性校验**：映射后仍需确认文件在当前工作树中真实存在；映射后依然缺失的条目属于历史输出相对当前结构的正常滞后（例如 `deepseeknova-desktop/src/commands.rs` 已在 60a579e 被拆分为 `commands/` 子模块目录），应改指其后继文件或丢弃，不得据此创建文件。

> **changeDrift 文档同步口径**：本项目中 `Makefile` / `Cargo.toml` 等构建配置的**文档伴随文件是 `AGENTS.md` 与 `BUILDING.md`**（`BUILDING.md` 的本地验证说明指回 `AGENTS.md`），而非各 crate 的 README。消费 `core-change-watch` 的 changeDrift 输出时，若 advisory 提示构建配置变更缺少文档同步，应核对 `AGENTS.md`（§3 常用命令）与 `BUILDING.md` 是否已与 Makefile 目标一致；一致即判定为**已同步**，不得据此去补写无关 README。构建配置变更时也应同步更新这两个文件。

### 3.1 深入文档与调试路由

需要更深背景或定位问题时，按用途路由到以下文档（本节仅作索引，不改变 §1 分档协议与上述命令面）：

- [DESIGN.md](DESIGN.md) — **架构设计记录（架构史）**，含命名现状权威说明与四层分层设计意图；**非 Agent 操作指令**，文中标注 [规划中/未实现] 的能力不得当作现行能力引用
- [BUILDING.md](BUILDING.md) — 编译环境与系统依赖（Linux/macOS 原生库安装），及 clone 后的 Git rename 跟踪配置
- [GUIDE.md](GUIDE.md) — 用户指南：核心概念（Runner/Tool）、配置、工具参考、HTTP API、TUI、MCP、沙箱等

最小调试入口：

- **CLI**：日志经 `tracing` 输出到终端（固定 INFO 级，见 `crates/deepseeknova-cli/src/main.rs`，不读 `RUST_LOG`；启用 `[telemetry] enabled=true` 时改装 OTLP 管线、日志经 OTLP 导出，终端不再打印 INFO 文本，属刻意权衡）；运行时派生数据在工作区 `.deepseeknova/`（`graph.db` 代码图索引、`memory.db` 记忆库）；配置层级为 `~/.deepseeknova/config.toml`（用户）+ `./deepseeknova.toml`（项目）；release 产物在 `target/release/deepseeknova-cli`。聚焦测试：`cargo test -p <crate> <测试名过滤词>`
- **桌面端**：前端产物在 `crates/deepseeknova-desktop/frontend/dist/`（Rust 侧编译依赖它，缺失先 `make frontend`）；前端类型检查/测试在 frontend 目录运行 `npm run lint` / `npm test`（node --test test/）；Rust 侧聚焦测试 `cargo test -p deepseeknova-desktop <测试名过滤词>`；一键验证 `make check-desktop`

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

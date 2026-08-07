# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

（桌面端为 Tauri 2 webview，设计语言按 web 处理；TUI 为终端字符界面，是同一视觉世界的字符版渲染。）

## Stack

- TUI：既有 `crates/deepseeknova-tui`，ratatui 0.30 + crossterm 0.29（现存代码库已确定）。
- 桌面端：delegated —— 选定 **Tauri 2 + SolidJS + Tailwind CSS 4**。理由：① git 历史中第二代桌面端（提交 `3ab55d7^` 之前）就是这套栈，有完整的 IPC 适配层与工程配置可考古复用；② Tauri 的 Rust 壳与 22-crate workspace 同语言，桌面端可直接依赖 `deepseeknova-serve`/`deepseeknova-runtime` 而非绕 HTTP；③ SolidJS 细粒度响应式适合 SSE 逐 token 流式渲染（无虚拟 DOM 重渲染开销）；④ 排除纯 Rust GUI（egui/iced/gpui）：markdown/代码高亮/CJK 排版生态不成熟，达不到"产品级完成度"的字体排印要求。

## Users

面向开源社区的开发者（主要用户），以及维护者本人。场景：在终端或桌面应用中运行 AI 编码 Agent 会话，实时审阅流式输出与工具调用，对权限请求做出批准/拒绝，事后查看运行质量（评分卡、诊断报告）。用户是重度终端使用者，日常与 Claude Code / opencode / Codex CLI 类工具共存。

## Product Purpose

DeepseekNova 是 Rust 编写的 AI Agent 框架（22 个 crate），计划开源。本轮工作目标：TUI 与桌面端达到**产品级完成度**——首次上手体验、视觉一致性、功能完整性都要经得起社区检验。成功 = 开源发布后能作为该品类中完成度可信的替代品被采用。

## Positioning

邻近产品（opencode、Claude Code 等）不能如实照抄的机制：**安全优先 + 任务质量闭环**。五层深度防御（沙箱/权限门控/安全策略/资源限额/审计日志）、fail-closed 审批、只读命令分类器，以及会话级质量闭环（ToolHook 治理链、失败诊断报告、六维评分卡）。界面设计应让"可审计、可信赖"成为可感知的产品气质，而非埋在文档里的特性。

## Operating Context

- 后端能力经 `deepseeknova-serve` 暴露：`POST /v1/chat`（SSE 流式）、`GET /v1/runs` + resume、`POST /v1/approval`（流断开即拒绝，fail-closed）、评分卡/诊断查询端点；另有 ACP stdio 模式。
- 已知接口缺口：会话级列表/恢复未 HTTP 化（TUI 走本地 `SessionController` 注入，serve 只有 run 粒度）；serve 仅限 127.0.0.1、无认证。桌面端全功能设计需将此列为配套后端工作。
- 桌面端历史：两代实现（Tauri+React → Tauri+SolidJS 对标 opencode），于提交 `3ab55d7`（2026-08-04）整体移除，git 历史可作工程参考；旧规划文档（`docs/superpowers/specs/2026-08-03-tui-v2-design.md`）曾提出"TUI 与 desktop 主题打通（跨端统一）"。
- 桌面端功能范围（本轮确认）：全功能——对话 + 会话管理 + run 恢复 + 评分卡/诊断可视化 + 设置，对标 opencode 桌面完成度。

## Capabilities and Constraints

- TUI 现状：对话流/状态行/输入区/提示行 + 五面板侧边栏、15 个斜杠命令、命令面板、语义化按键系统（keybindings.json 可改键 + 热重载）、审批浮层、消息折叠、diff 高亮、三档主题（`DEEPSEEKNOVA_THEME`）。
- 未提交改动方向（信息层级演进）：瞬态反馈走 6 秒 TTL notice 浮层，永久内容进对话流；Ctrl+T 鼠标捕获切换；ctx 占用口径改为"最近一次请求实际 tokens"。
- 明确未定的产品事实：**界面文案语言**——当前 TUI 文案为中文（"你"、"正在思考…"、"请求授权"），开源面向国际社区通常英文优先；是否引入 i18n 或切英文，留待用户决定，设计中不得擅自定死。

## Brand Commitments

- 名称 `DeepseekNova`（大写 N），crate 前缀 `deepseeknova-*`，环境变量前缀 `DEEPSEEKNOVA_`。
- 品牌色 `#4D6BFE`（DeepSeek 靛蓝，`theme.rs` 中注释"品牌蓝"），配套柔蓝紫 `#7A8CFF`、暗色亮化版 `#6E8CFF`、选中底色 `#263264`。
- 现有字符视觉母题：圆角单线框 `╭─╮` + accent 标题、`❯` 提示符、`●/○` 运行态、`✓/✗` 验证、`█░` 预算条、"安静 dim + 单一 accent"的克制风格。
- 无图形 logo、无 ASCII art（截图资产已随桌面端删除，README 截图段为空占位）。
- **视觉方向（2026-08-07 用户两轮拍板，以第二轮为准）**：桌面端与 TUI 采用**「新星观测台 Nova Observatory」**视觉世界——Agent 会话即一夜天文观测：流式事件是观测记录、安全审批是圆顶联锁、评分卡是测光曲线。夜空底 `#0B1020` + 星图刻线（graticule 细线）+ 星等刻度点阵 + 观测日志表格纪律；品牌蓝 `#4D6BFE` 是唯一星色/accent，琥珀 `#E8A33D` 作标注色。工艺基准线维持 **Claude Desktop / Claude Code**。（第一轮曾选品类标准 canon 并批准构图 C 工作台，随后用户改选观测台，canon 承诺作废；构图 C 的信息架构价值——runs 条带与评分卡一等公民——迁移进观测台语法。）

## Evidence on Hand

- 真实可演示的能力：SSE 流式对话、工具调用与审批流、评分卡 JSON（六维）、诊断报告 JSON——桌面端可视化有真数据可用，无需编造。
- 缺失且不得虚构：logo/图标资产、用户证言、性能基准数字、下载量等商业性声明。
- 文档：GUIDE.md（用户指南，其配色章节已落后于 `theme.rs` 代码，交付时需同步）、DESIGN.md（**架构设计记录，非视觉设计文档**，视觉决策不得写入该文件）。

## Product Principles

1. **可信赖是可见的**：安全边界、审批、质量闭环要在界面上有一等公民的表达，不藏在日志里。
2. **两端一个世界**：桌面端与 TUI 共享同一视觉世界，桌面是完整版渲染，TUI 是字符版渲染；改一处语义色，两端同义。
3. **流式是常态**：设计以"内容持续到达"为默认态，静态完成态是特例。
4. **键盘优先，鼠标不残废**：两端都以键盘为主操作路径，桌面端鼠标路径完整可用。
5. **开源门面即产品**：首次启动体验、空状态、错误文案与 README 截图同等重要。

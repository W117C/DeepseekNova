# 任务书：观测台前端 UI + TUI 演进（2026-08-07 dev-loop 轮）

## 1. 意图
把已定稿的「新星观测台」设计规范（2026-08-07-observatory-frontend-design.md）落地为可运行、可验收的前端：
- 桌面端：重建 `crates/deepseeknova-desktop` 纯前端工程（Vite + SolidJS + TypeScript + Tailwind CSS 4），首屏实现 A×B 合并拓扑——双顶带（圆顶状态带 + 观测之夜 runs 条带）、左观测日志栏（夜次分组 + 星点）、中央划线对话流 + composer、右「本次观测」星座图 + 「测光·评分卡」六维表；本轮不接 Tauri Rust 壳。
- TUI：按规范 §三 演进五项——浅色档 token 对齐、侧边栏夜次分组 + 星点三档、审批风险标签 + mono 命令、测光评分卡 + `/scorecard`、欢迎卡圆顶字形；保留既有键位与斜杠命令。

## 2. 拍板
- 桌面端本轮只做纯前端（Vite+Solid+Tailwind），Tauri 壳 / serve sidecar 下轮；前端结构与 API 层按规范留好接缝，Tauri 只需挂载 build 产物。
- 文案维持中文（未决项不拍板）。
- 演示数据沿用构图合成数据（规范已声明「不得字面照抄」）。
- 审批风险标签：在 `PermissionGate` 新增只读分类查询（additive API），agent Ask 分支把标签文本放进 description 前缀，TUI 渲染为风险标签行 + mono 命令块；不改 core `ApprovalResponder` trait。
- P0 未提交代码只做基线提交，不改造语义。
- 猜错代价：若用户期望本轮出 Tauri 壳，纯前端可被 Tauri 直接挂载，下轮多一步；若期望全英文，文案可后置 i18n。

## 3. 白名单
- 可改：`crates/deepseeknova-tui/**`、`crates/deepseeknova-agent/src/agent.rs`（Ask 描述加风险标签）、`crates/deepseeknova-permission/src/lib.rs`（新增 `shell_readonly_kind` + 测试）、`crates/deepseeknova-desktop/**`（新建）、`GUIDE.md`、`CHANGELOG.md`、`AGENTS.md`（crate 清单如需）、`BUILDING.md`（如需）、`PROGRESS.md`、`crates/REVIEW.md`、`crates/CLOSEOUT.md`、`BLOCKED.md`、`docs/superpowers/plans/**`。
- 只读：core / serve / runtime / config / security / store / 其余 crate。

## 4. 任务
0. 基线：提交当前未提交 P0 改动（不 push）；记录 `make check` 数字（实测 EXIT=0，TUI 测试数 = 基线）。
1. Desktop scaffold：`crates/deepseeknova-desktop/frontend`（package.json / vite / tsconfig / tailwind4 / `src/App.tsx` 首屏 / `src/lib/*.ts` 纯函数）；vitest ≥ 4 条（nightGrouping / constellationNodes / scorecardRows / tokenBudget）；`npm run build` EXIT=0；`vite preview` 截图 1536×1024 → `.impeccable/mocks/obs-comp-d-desktop-p1.png`。
2. TUI 浅色档：`light_theme` 按「印刷星图」token 对齐（user `#3B55D9`、agent `#5A4FB8`、ok `#0E7A42`、fail `#C0303A`、accent `#3B55D9`、selection `#DDE4FB`、border `#D8DDEC`）；测试断言 token。
3. TUI 侧边栏夜次分组：saved_sessions 按 id 日期分组头「MM-DD 夜」+ 星点三档（当前 ◉ / 本夜 ● / 更早 ·）；测试分组与星点纯函数。
4. TUI 审批：permission 新增 `shell_readonly_kind(tool, args)`；agent Ask 描述前缀 `[风险:只读|非只读|危险]`；approval 渲染风险标签行 + mono 命令块 + 危险说明；三处测试（permission / agent / tui render）。
5. TUI 测光评分卡：`AppState.scorecard: Option<Scorecard>`；`/scorecard` 读最新 `.deepseeknova/metrics/*.json` 输出六维 `█░` 横条；侧边栏 Cost 面板加「测光」六维摘要（无数据时「测光待完成」）；测试解析与渲染。
6. TUI 欢迎卡：标题加 `⌒` 圆顶字形；更新既有欢迎卡测试。
7. 文档：GUIDE 配色章节与 `theme.rs` 同步 + 上下文占用口径；CHANGELOG Added；AGENTS.md crate 清单注明 desktop frontend 为非 cargo crate（如需）。
8. 全量验收：`make check` EXIT=0；反向验证 TUI（改坏分组/风险标签断言 → 红 → 还原 → 绿）+ 前端（改坏 nightGrouping → vitest 红 → 还原 → 绿）。
9. OCR delegate preview / rule + 宿主审查；high/critical 修复后重跑全绿。

## 5. 防作弊
- 禁止 `|| true`、删测试、mock 被测对象、放宽断言、跳过 fmt/clippy/doc。
- 新行为必须有对应测试；既有测试数不降。
- 反向验证必须真实红 → 绿。
- 不得改 P0 代码语义；不得把 desktop 加入 cargo workspace（本轮）。

## 6. 完成条件
- 硬指标 1：`make check` EXIT=0（fmt / clippy -D warnings / 全 workspace / doctest / doc 零警告），TUI 测试数 ≥ 基线 + 新增。
- 硬指标 2：`npm run build` EXIT=0 + 截图存在；vitest 全绿。
- 止损：npm 安装连续 3 次失败或超 15 分钟 → 交付 TUI 全项 + desktop 源码（未构建），BLOCKED 记录；`make check` 出现与本书无关的红 → 停下报告。

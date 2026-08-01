# PROGRESS — TUI 设计功能完善（任务书执行）

## 开工回执（2026-08-01）
- 理解的目标：TUI 命令面与 REPL 对齐（/skills /mcp /raw /undo）、输入可编辑+可见光标、运行事件代际防串台、/resume 渲染历史、文档同步，全部带测试并过 make check。
- 顺序：任务 0 基线 → 1 输入编辑（纯单 crate）→ 2 代际 → 3 命令补齐（大头，跨 crate）→ 4 resume 渲染（跨 crate）→ 5 文档/反向验证/PR。
- 最大风险：/undo 与 resume 签名变更涉及跨 crate（tui+cli），按 AGENTS.md §1 推理专家协议执行；/undo 只许在 tempdir 测试，不许动仓库真实文件。
- 任务 0 证据：main@68fd58c；make check 全绿（tui 16 单测+1 doctest）；cargo metadata --locked 现为 LOCKED_OK。
- 存量失配：提交版 Cargo.lock 缺 async-trait→cli、deepseeknova-provider→tui 两条记录；cargo 运行已自动补录（git status 显示 M Cargo.lock）。书本预期 metadata --locked 先失败，因补录已发生改为通过——失配证据=提交版 lock 与 manifest 对比（git show 两文件核对）。

## 任务状态
- [x] 任务 0：基线核验
- [x] 任务 1：InputState + 光标（21 单测绿，clippy -D warnings 零告警）
- [x] 任务 2：RunSession 代际（23 单测绿）
- [x] 任务 3：/skills /mcp /raw /undo（TUI 31 单测绿；CLI 侧 tui_undo.rs 2 测试绿，tempdir 回滚验证 modified→unchanged）
- [x] 任务 4：/resume 渲染历史（SessionController::resume 返回 ResumedLine 列表，CLI 按 role 映射）
- [x] 任务 5 前半：GUIDE/TUI README/CHANGELOG 已同步；make check 全绿；cargo metadata --locked 通过（LOCKED_OK）；反向验证：改坏 skills 测试断言 → 红（0 passed; 1 failed）→ 还原 → 31 绿 + FMT_OK
- [x] 任务 5 后半：分支 feat/tui-complete，commit cbca2ef，push 成功，PR #53 已开（body 已修正）；CI/Desktop Build/Security 已在跑

## 交付物
- PR: https://github.com/W117C/DeepseekNova/pull/53
- 本地：feat/tui-complete @ cbca2ef，工作树干净；main 未动

## 任务 3/4 备注
- /undo 采用 trait 接法（备选 A）：TUI 定义 UndoController，CLI 在 crates/deepseeknova-cli/src/tui_undo.rs 用 CheckpointManager 实现，每次调用重新 load_from 磁盘，天然支持 &self 与多进程共享。
- /resume 的 StoredMessage.role 是 String（"user"/"assistant"/"system"/"tool"），按字符串映射，tool 归 System 展示。

## 跨 crate 协议记录（AGENTS.md §1 触发项）
- 错误预扫描（禁行区）：不改 core/provider/agent 公共 API；不改既有 16 条 TUI 测试断言（resume 签名变更的最小适配除外）；不对仓库真实文件执行 rollback；不新增外部依赖。
- 备选路径：/undo 接法 A=trait（TUI 定义 UndoController，CLI 用 CheckpointManager 实现，保持 TUI 不依赖 config）vs B=TUI 直接依赖 checkpoint+路径参数。选 A：依赖方向与 SessionController 一致，CLI 侧可单测。
- 自检：每项完成后 cargo test 单 crate，收尾 make check 全绿 + 反向验证红→绿证据。

## 决策记录（建议/偏离）
- 任务 0 的 metadata --locked 预期调整见上（补录已发生，非未做）。

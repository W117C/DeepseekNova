# 任务书 P：Protocol 增强收尾（task_rate + record_use 回填）

## 1. 意图
协议增强能力包（2026-08-05 已合入 main）设计文档 §13 记录了 2 个未落地项：task_rate 指标（first_pass/retry_rounds）与 fitness record_use 回填。本轮把它们收尾，让"守规度量"和"技能进化"两条闭环完整。

## 2. 我替领导拍的板
- 本轮 protocol 域=两个未落地项收尾；TUI 完整协议状态面板、主循环结构化计划载体（drift 完整版）、多模型反思对比写入 BLOCKED 留待后续。
- 基线=当前分支 feat/memory-lifecycle @ 68fb094（含上一轮记忆生命周期成果，未 push；交付后由领导定 push/PR）。
- task_rate 推导：从 DiagnoseReport.failures 推导——failures 为空=first_pass=true；有 failures=first_pass=false 且 retry_rounds=重试轮次计数；写入 scorecard 扩展字段（serde default，向后兼容）。
- record_use 回填：recall 注入侧收集本次会话实际注入的技能名集合，CLI 侧 session_skills（main.rs:875-886 现为空 Vec）改传该集合；fitness record_result 与 record_use 均真实记录。

## 3. 白名单（只改这些，其余只读）
- crates/deepseeknova-metrics/（scorecard 扩展 + 测试）
- crates/deepseeknova-agent/src/diagnose.rs（仅所需只读/字段访问）
- crates/deepseeknova-runtime/src/lib.rs（仅 fitness/scorecard/诊断装配点；**memory 装配区 :480-635 只读**）
- crates/deepseeknova-cli/src/main.rs（session_skills 收集替换）+ cli.rs（如需）
- crates/deepseeknova-core/src/memory/skill.rs（如需暴露注入技能名）
- GUIDE.md、CHANGELOG.md、PROGRESS.md、BLOCKED.md

## 4. 任务
- 任务 1：task_rate 落地。metrics Scorecard 增 first_pass: bool、retry_rounds: u32（serde default）；runtime 诊断落盘后从 DiagnoseReport 推导写入（fill_task_rate 或等价，scorecard 已有 fill_protocol 先例 :318-322）。测试：无失败=first_pass true、有失败=retry_rounds≥1、旧 scorecard JSON 反序列化兼容。
- 任务 2：record_use 回填。recall 注入侧收集会话注入技能名集合 → CLI 传 session_skills（替换空 Vec）；fitness record_result 后补 record_use（runtime :1295 附近）。测试：注入技能被记 use+result、无注入时优雅跳过（清掉 warn 噪声）。
- 任务 3：文档。GUIDE 协议节补 task_rate/record_use 一行各；CHANGELOG Added；PROGRESS/BLOCKED 更新。
- 任务 4：收尾。cargo fmt + make check 全绿 + 反向验证（改坏 task_rate 断言→红→还原→绿）；提交 feat/memory-lifecycle（不 push）。

## 5. 防作弊
- 测试只增不减：metrics ≥18、runtime ≥49、agent ≥231、cli ≥32、workspace 保持 0 failed。
- 不许 mock 被测对象、删测试、放宽断言、|| true、跳过 fmt。
- 新行为各 ≥1 条测试（task_rate 两分支、record_use、兼容性）。
- 组合/边界：无失败会话、有失败会话、旧文件兼容三态都覆盖。
- 反向验证必须真红真绿，贴输出。

## 6. 完成条件（两条硬指标 + 止损）
- 硬指标 A：`make check` EXIT=0；metrics ≥18、runtime ≥49。
- 硬指标 B：scorecard JSON 含 first_pass/retry_rounds 且值正确；fitness.json 出现真实 use 记录；CLI 无 record_use warn 噪声。
- 止损：同一验收连败 3 次换路径；结果比基线差就回滚如实报告；超 3 小时汇报进度停手。
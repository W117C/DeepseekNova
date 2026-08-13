# Deferred Minors 汇总归档（2026-08-13）

> 各方向 SDD 账本中的 deferred minor 统一归档于此，供后续决定是否修复。
> 来源：`docs/superpowers/plans/2026-08-13-{p0-security-fixes,p1-cli-consistency,doc-drift,p2-perf-correctness,p1-deadcode}.md` 的 SDD 账本（已随 worktree 清理，本文件为持久记录）。

## P0 安全修复（p0-security-fixes）

- [x] **B. shell 输出 soft bound**：`cap_output` overflow 分支经 `from_utf8_lossy` 后非严格上限（~3×cap）；overflow 分支只追加注记、不再截断。有界、非内存耗尽向量。`crates/deepseeknova-tools/src/shell.rs`
- [x] **C. collect 出错路径未 wait**：`Ok(Err(e))` 分支 `kill()` 后未 `wait()` 回收（`kill_on_drop` 只 kill 不 reap，瞬时 zombie；tokio 全局 reaper 兜底）。`crates/deepseeknova-tools/src/shell.rs:180-183`
- [x] **D. 测试断言过松**：`shell_caps_output_using_security_limit` 长度断言 `<500` 过松，应收紧到 `<= cap + note_len`。`crates/deepseeknova-tools/src/shell.rs:501`

> ✅ 已解决：check_node_action 忽略 `Action::Conditional`（方向3 c10f079 穷尽匹配修复）。

## 方向1 CLI 一致性（p1-cli-consistency）

- [x] **1. ReplCaps doc 措辞**：doc 说「字段为 Option」但 `mcp_servers` 是 `Vec`（cosmetic）。`crates/deepseeknova-cli/src/chat.rs:140`
- [x] **2. mcp_server_infos 三处重复**：`config.mcp_servers → McpServerInfo` 映射在 `main.rs` 三处（TUI + 两个 REPL 入口），可抽 `fn mcp_server_infos(&config)` helper。`crates/deepseeknova-cli/src/main.rs:750-757,941-950,1606-1615`
- [ ] **3. 降级分支不可达**：`undo=None`/`mcp_probe=None` 分支当前生产恒 `Some`（defensive，仅 `ReplCaps::default()` 可达）——观察项，非缺陷。

## 方向2 文档漂移（doc-drift）

- [x] **1. evals/results/ 漂移**：`evals/README.md:26` 提到 `evals/results/` 归档目录但不存在（范围外既有漂移）。
（2026-08-13 已核实：T1-T19 逐项完成，17 落地 / T12、T13 部分落地，状态行已更新）
- [x] **2. arch-review-fix-plan 复核**：状态标注「T1/T2/T5 已落地」系计划原文照抄，T3/T4/T6-T19 未逐项核实——需架构整改执行方复核。

## 方向3 P2 性能正确性（p2-perf-correctness）

- [x] **1. resolve CTE 语义拓宽**：递归 CTE 从「单父链」拓宽为「多父扇出」（更正确但非纯重构），建议 `ancestor_names` doc 注明。`crates/deepseeknova-graph/src/store.rs:1377-1398`
- [x] **2. avg_overall 双语义**：`avg_overall`（composite 均值）与 `Scorecard::overall()`（4 维均值）并存，建议文档说明关系。`crates/deepseeknova-metrics/src/lib.rs:617,404-410`
- [x] **3. to_usage 记账假设**：anthropic `to_usage` 的 prompt 公式假设 DeepSeek 口径（input 已含 cache 则双重计数），建议注释。`crates/deepseeknova-provider/src/anthropic.rs:646-660`
- [x] **4. for_each_sse_line 无直接单测**：`\r` 跳过、尾行冲刷仅被间接覆盖，建议补纯函数级测试（喂 `b"data: a\r\n\ndata: b"` 断言回调序列）。`crates/deepseeknova-provider/src/sse.rs`

> ✅ 已解决：A.2 的 FnMut→Fut 闭包签名偏差（按值传状态契约落地）。

## 方向4 死代码清理（p1-deadcode）

- [x] **1. CLI /skills 展示过滤依赖 persist.workspace**：`persist=None`（会话持久化关闭）时 `/skills` 展示不一致（deprecated 技能仍显示，但 runtime 已正确排除工具注册）——display-only。`crates/deepseeknova-cli/src/chat.rs:744-746`
- [x] **2. persist.workspace lossy 路径**：用 `display().to_string()` 非 UTF-8 工作区路径会 miss fitness 文件——display-only。`crates/deepseeknova-cli/src/main.rs:2061-2063`

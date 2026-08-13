# 修复任务书：架构审查 P0/P1 整改（2026-08-11）

> 依据：2026-08-11 全面架构审查报告（5 个 hard 难度并行子代理分域深查 + 父级定向核实，
> 报告含 file:line 与置信度声明）
> 执行模式：T1-T7 安全组并行 worker → T8-T10 数据/权限组并行 → T11-T15 正确性组
> （依赖前面组稳定）→ T16-T19 一致性/性能组 → 父级收尾 + `make check` 强制验收
> 约定：worker 只编辑自己分配的文件；改动前用 `read_file` 复核行号（审查报告行号为
> 快照，可能 ± 数行）；新增 pub 项必须补 `///` 中文文档注释（missing_docs lint）；
> 跨 crate 变更按 AGENTS.md §1.1 完整协议执行
> 状态：已逐项核实（2026-08-13）——已落地：T1（tar -I deny 集，readonly.rs:1041）、
> T2（is_path_allowed 接入 fs/grep/glob/ls，fs.rs:553）、T3（Sandbox 能力位+装配降级告警）、
> T4（tool_use/tool_result/thinking 回放）、T5（lsp resolve_path 走 sanitize）、
> T6（MCP server-request 应答，stdio+HTTP）、T7（tavily SSRF+5MB 上限）、
> T8（五段深合并）、T9（Snapshot Vec<u8>+existed）、T10（缓存契约四修）、
> T11（run 局部 findings 暂存区+并发隔离）、T14（serve Semaphore+审批 RAII）、
> T15（流式退出改返回码）、T16（schema 三态保守）、T17（content_id 换 sha2）、
> T18（锁外嵌入扫描）、T19（hook 异步化）；部分落地：T12（缺 RecursiveDelegateTool
> enforce_capability 能力门）、T13（缺生产路径 ModelResolver 装配，仅测试装配）；
> 无未落地项。详见 /tmp/arch-review-verification.md。

---

## Summary

把 2026-08-11 审查报告的 P0（安全/数据完整性，T1-T10）与 P1（正确性/一致性/性能，
T11-T19）共 19 项整改落地为可执行任务。P2 优化项（WireEvent 消费收敛、CLI 装配
收敛、TUI 重绘、死代码清理等）列后续轮，本任务书不含。

---

## P0 安全组（T1-T7）

### T1. tar 短 flag 白名单补 `-I`（审查 S1，critical）
**文件**：`crates/deepseeknova-security/src/readonly.rs`
**问题**：`tar_allowed` 短 flag deny 集（约 L1041）为 `'x'|'c'|'r'|'u'|'F'`，缺 `'I'`；
GNU tar `-I/--use-compress-program` 以 shell 执行外部压缩程序（含空格时经
`/bin/sh -c`），`tar -tfI 'sh -c "id"'` 被误判 ReadOnly 免询问放行 → 只读命令实为
任意命令执行。长 flag `--use-compress-program` 已拒绝（约 L1024），短 `-I` 是唯一缺口。
**改动**：
- 短 flag 扫描 deny 集加入 `'I'`（`-I` / `-I=*` 形态一并拒绝）
- 扩展 `tar_exec_class_flags_rejected` 测试覆盖 `-I`/`-tfI`；新增反例测试
  `tar -tfI 'sh -c "id"'` 必须判 NotReadOnly
- 顺带盘点其余"可触发外部程序"flag（rg `--pre` 已拒、find `-exec` 已拒、xargs 已拒、
  git `-c` 已拒），复核无遗漏则不改
**测试**：readonly 单测 + proptest `tar_write_char` 集合加 `'I'`；`cargo test -p deepseeknova-security readonly`

### T2. `SecurityPolicy` 路径表接线（审查 S2，high）
**文件**：`crates/deepseeknova-security/src/policy.rs`、`crates/deepseeknova-runtime/src/security.rs`、`crates/deepseeknova-tools/src/fs.rs`（含 read/write/edit/move 与 glob/grep 读路径）
**问题**：`is_path_allowed` 全仓零生产调用（仅 quickstart/测试），`[security] denied_paths`
内工作区子路径（如 `./secrets`）可被 write_file/edit_file/move_file 照写——静默 fail-open。
**改动**：
- fs 工具在 `sanitize_path`/`secure_resolve` 之后追加 `sec.policy.is_path_allowed(resolved)`
  检查（read/write/edit/move 一致）
- 命中 denied_paths → 拒绝并给出明确错误（含原因+建议，参照既有文案契约）
- 构建装配时对未接线的路径表打 `warn`（防后续新增路径表再漏接线）
**测试**：denied_paths 内子路径 write/edit/move 拒绝；allowed_paths 之外拒绝；
工作区内常规路径不受影响（既有 fs 测试全绿）

### T3. Windows/域名沙箱降级升级为能力位（审查 S8/S9，high）
**文件**：`crates/deepseeknova-sandbox/src/lib.rs`、`src/windows.rs`、`src/seatbelt.rs`、`crates/deepseeknova-tools/src/shell.rs`、`crates/deepseeknova-runtime/src/lib.rs`（装配点）
**问题**：Windows JobSandbox 对网络/FS 写路径零限制，`allow_network=false`/ReadOnly 档
仅 `tracing::warn!`；`NetworkPolicy.allow_domains` 域名白名单在 seatbelt/bwrap profile
均不渲染——"配置承诺与强制不符"的静默失效面。
**改动**：
- `Sandbox` trait 增加能力位查询（如 `enforced_network()` / `enforced_fs()`），
  seatbelt/bwrap/JobSandbox/NoOp 各后端如实返回
- 用户显式请求禁网/只读但后端无法强制时：装配点升级为显式告警（CLI 启动横幅同口径），
  ShellTool 对"显式禁网但后端无法强制"场景按 fail-closed 或拒绝执行（可用性评估后
  决策，至少告警必须可被程序检测，不再只是 tracing warn）
- `requests_domain_filtering()` 为真且后端不支持时在装配点打显式告警
**测试**：各后端能力位查询单测；Windows 分支降级告警测试；shell 禁网场景正反例

### T4. Anthropic 后端 content-block 回放（审查 S3/M5，high）
**文件**：`crates/deepseeknova-provider/src/anthropic.rs`
**问题**：`AnthropicMessage` 只有 `role + content: String`，assistant 的 `tool_calls`、
`Role::Tool` 消息的 `tool_call_id`、`reasoning_content` 全部被丢弃——anthropic/
deepseek-anthropic 后端多轮工具调用无法回放 tool_use/tool_result，工具循环必失败或 400。
**改动**：
- assistant 消息带 `tool_calls` → content 构造 `[{"type":"tool_use","id","name","input"}]` 块数组
- `Role::Tool` 消息 → `[{"type":"tool_result","tool_use_id","content"}]` 用户内容块
- `reasoning_block`（must_replay，带 tool_calls 时 V4 必回放）→ thinking block 回放
**测试**：`tests/deepseek_reasoning_protocol.rs` 补多轮工具调用回放用例（fixture 对齐
真实兼容端点）；OpenAI 路径行为不变（既有测试全绿）

### T5. lsp_diagnostics 路径守卫（审查 S4，high）
**文件**：`crates/deepseeknova-tools/src/lsp.rs`
**问题**：`resolve_path`（约 L175-185）直接用 `PathBuf::from(raw)` / `workspace_root.join(p)`，
只做词法规范化，不走 `sanitize_path`/`secure_resolve`——`../../etc/passwd` 或绝对路径
可读工作区外文件并发给 LSP 进程。
**改动**：`resolve_path` 改走 `deepseeknova_security::path::sanitize_path`（workspace 内
解析，拒绝 `..` 逃逸与绝对路径）；与其余 fs 工具同一事实源
**测试**：`../../etc/passwd` 拒绝；绝对路径拒绝；workspace 内正常（既有 lsp 测试全绿）

### T6. MCP 服务器→客户端请求应答（审查 S5，high）
**文件**：`crates/deepseeknova-mcp/src/connection.rs`、`src/http_client.rs`
**问题**：reader 任务把任何带 `id` 的消息一律当"对 pending 请求的响应"处理，无匹配即
丢弃；但客户端在 initialize 主动声明了 `roots` capability，合规服务器发 `roots/list`
（带 id+method）会被当未知响应丢弃，服务器等不到应答可挂死握手。HTTP 传输同缺口
（parse_sse_response 丢弃不匹配事件，无 GET 长连接流）。
**改动**：
- stdio reader 区分 request（id + method、无 result/error）/ response / notification，
  对 server-request 回 `result`（roots/list 至少返回空数组）
- HTTP 传输实现 SSE 事件循环，区分三类消息并应答 server-request
**测试**：mock 服务器发 `roots/list` → 客户端应答（stdio + HTTP 双传输）；握手不挂死

### T7. web_search tavily SSRF + 响应上限（审查 S6，high）
**文件**：`crates/deepseeknova-tools/src/web_search.rs`
**问题**：tavily 分支只做 `check_domain_allowed`，无 `validate_host_ssrf`（其余三后端
每跳都做）；`search_get` 与 tavily POST 都 `resp.text()` 整读无字节上限（web_fetch
有 5MB 上限）。
**改动**：
- tavily 端点补 `validate_host_ssrf`（与 bing/searxng/ddg 同源）
- 四后端响应体统一字节上限（对齐 web_fetch 5MB，超限截断或报错）
**测试**：内网 IP base_url 拒绝；超限响应截断；既有 web_search 测试全绿

---

## P0 数据完整性组（T8-T9）

### T8. config 分层 merge 深合并五段（审查 A1，high）
**文件**：`crates/deepseeknova-config/src/lib.rs`
**问题**：`Config::merge`（约 L1894-1898）对 `session/budget/review/verify/checkpoint`
五段无条件整体替换，项目层 TOML 缺失这些段时 serde 默认值覆盖用户层配置——只要存在
`deepseeknova.toml` 用户层 budget 上限/session root/开关即被重置。与 memory 段
"非默认值才覆盖"深合并语义不一致。
**改动**：
- 五段改为与 `MemoryConfig::merge` 同款逐字段深合并（`Option` 字段 None 不覆盖；
  非 Option 字段需"段在 TOML 中实际出现"才覆盖，或逐字段比较默认值）
- 补分层保留测试（对齐 `memory_merge_preserves_user_layer_for_unset_fields` 先例）
**测试**：项目层缺 budget 段 → 用户层 budget 上限保留；session root 保留；
review/verify/checkpoint 开关保留；既有 merge 测试全绿

### T9. checkpoint 二进制快照无损（审查 A2，high）
**文件**：`crates/deepseeknova-checkpoint/src/lib.rs`
**问题**：`snapshot_state`（约 L465-480）用 `String::from_utf8_lossy`，二进制文件快照
回滚写回替换符字节——文件被静默破坏；`restore_state`（约 L485-503）把空内容当
"原本不存在"，0 字节合法文件回滚时被删除。
**改动**：
- `Snapshot.content` 改为 `Vec<u8>`（JSONL 序列化 base64，或改用独立文件存储；
  需评估既有会话检查点数据兼容，必要时版本迁移）
- "原本不存在"用独立 `existed` 标志位，空文件不再误删
**测试**：二进制文件（PNG/PDF 样张）快照→回滚字节级一致；0 字节文件回滚后存在；
文本文件行为不变（既有测试全绿）

---

## P0 权限缓存组（T10）

### T10. 会话缓存契约修复（审查 S7，high）
**文件**：`crates/deepseeknova-permission/src/lib.rs`
**问题**：`check()` 顺序为 rate → preflight → cache → finalize，缓存放行绕过 deny 表
与 `PermissionMode` 切换（Auto 批准的命令切 Plan 后仍免询问）；`set_mode` 不清理缓存；
缓存无容量上限；cache key 用原始 args 串（空白差异重复弹窗）。
**改动**：
- cache 命中后仍对 deny 表做一次匹配（deny 不可被缓存绕过）
- `set_mode` / `set_trusted` 触发缓存清理
- 缓存容量上限（如 4096）+ 简单逐出（LRU/随机）
- cache key 规范化：`serde_json::from_str` 后重新 `to_string` 再哈希
**测试**：Auto 批准→切 Plan 同一命令变 Ask；deny 规则命中优先于缓存；
key 空白差异同 key；容量逐出行为

---

## P1 正确性组（T11-T15）

### T11. findings 容器 run 边界重置 + 并发切片污染（审查 A3/M3，high）
**文件**：`crates/deepseeknova-agent/src/agent/mod.rs`、`src/agent/loop_impl.rs`
**问题**：`quality_findings` 容器只增不减（跨 run 累积），触顶 10k 后新 findings 永久
丢弃；F4 差分切片 `[start_len..]` 只对顺序 run 成立，serve 共享同一 Agent 并发 run 时
scorecard/diagnose 被串扰，良性 run 可能因他人 Blocking finding 触发对抗审查烧 token。
**改动**：
- run 边界（`!seeded` 时）重置 findings 容器，或改环形缓冲（10k 淘汰最旧）
- 并发隔离：为 finding 附加 run 所有权（run id），emit/diagnose 只消费本 run 暂存区；
  或工具钩子写容器时同时写入 run 局部暂存区——并发场景不再用"容器长度差分"近似
**测试**：顺序 run 语义不变（既有测试全绿）；新增并发 run 交叉测试（serve 双流 →
scorecard/diagnose 各归各 run）；触顶后新 run 的 Blocking finding 仍可见

### T12. 子代理取消传播 + 资源限额 + 输出截断（审查 A4，high）
**文件**：`crates/deepseeknova-agent/src/sub_agent.rs`、`src/agent/loop_impl.rs`
**问题**：子代理循环不执行 `max_tool_calls`/`max_execution_time` 步级检查，非 shell 工具
输出无 `max_output_bytes` 截断；`build_ctx` 生成全新 CancellationToken 未关联父 run
取消——父取消后子代理仍跑满 max_steps；`RecursiveDelegateTool` 无能力门校验（L3 顺带）。
**改动**：
- 父 `CancellationToken` 传入 `dispatch_stream`/`run_sub_agent_loop`；工具执行包
  `tokio::select!`（父取消即中断）
- 子代理步级限额检查（对齐主循环 loop_impl.rs:269-288）；工具输出统一
  `max_output_bytes` 截断（含非 shell 工具）
- `RecursiveDelegateTool` 补 `enforce_capability`（对齐 DelegateTool）
**测试**：父取消→子代理立即中止；超限子代理步级中止；超长输出截断；
递归委派能力门生效

### T13. per-agent 模型 resolver 装配（审查 A5/H3，high）
**文件**：`crates/deepseeknova-runtime/src/delegate.rs`、`crates/deepseeknova-cli/src/main.rs`（装配点）、`crates/deepseeknova-agent/src/agent/loop_impl.rs`
**问题**：AgentManifest `model:` 被解析携带但主循环从不消费 `model_override` 选 provider；
`ModelResolver` 全仓只在测试出现，两条生产路径（DelegateEngine/SubAgentRunner）均未装配
——"per-agent 模型"是声明未接线。
**改动**：
- 在 `run_agent_loop` 消费 `model_override`（经 resolver 选 provider），或引擎按
  manifest model 构造对应 provider 的子 Agent
- runtime/CLI/serve 装配 `ModelResolver`（config 已有 agents 目录/深度/per-agent 配置）
- 能力白名单/权限交集同步在 DelegateEngine 路径生效（M5 顺带：构造子 Agent 时按
  `p.permission` 与 `AgentGateMode` 应用）
**测试**：manifest 声明不同 model → 断言实际使用的 provider 不同；无 resolver 回落
默认 provider（既有 warn 路径保留）；DelegateEngine 路径能力白名单生效

### T14. serve 并发上限 + 审批 RAII 清理（审查 A6/A7，high）
**文件**：`crates/deepseeknova-serve/src/lib.rs`
**问题**：`/v1/chat` 与 resume 共享同一 `Arc<dyn Runner>` 无并发上限；审批 oneshot 注册
进 pending map 后无限等待，客户端断开条目永久滞留（可被猜 id 应答死请求），客户端
永不调 `/v1/approval` 则 agent 永久阻塞。
**改动**：
- 单次端点加全局并发上限（`Semaphore`，可配置）
- 审批条目生命周期与 SSE 流绑定（RAII guard，future drop 时移除），或加超时按拒绝处理
**测试**：并发超限拒绝/排队；审批条目断开后清理（map 大小不增长）；超时拒绝路径

### T15. CLI 流式退出改返回码（审查 A8，high）
**文件**：`crates/deepseeknova-cli/src/main.rs`
**问题**：`stream_events`/`stream_coordinator`（约 L2281/L2332）与 eval 路径用
`std::process::exit` 直接退出，跳过 `_telemetry_guard` Drop flush——Paused 退出遥测
数据丢失。
**改动**：流式函数返回状态码（而非 exit），由 main 顶层在 guard 仍存活时退出；
eval 路径同口径
**测试**：Paused 退出后遥测文件已落盘（或 guard flush 断言）；退出码不变

---

## P1 一致性组（T16-T17）

### T16. graph schema 未来版本保守策略（审查 M1，medium）
**文件**：`crates/deepseeknova-graph/src/store.rs`
**问题**：`version != SCHEMA_VERSION` 时直接 `DELETE FROM files` 并把版本改写为当前值——
旧二进制打开未来版本库会清空全量索引并降级版本号，与 memory store 三态保守策略相反。
**改动**：仅已知旧版本才清表重索引；未知/未来版本保持原版本号、不降级、不破坏
（对齐 memory/store.rs 的 `ensure_schema_version`）
**测试**：未来版本库（如 "5"）打开不被清空、版本号不被改写；已知旧版本迁移行为不变

### T17. `content_id` 换 sha2（审查 M4，medium）
**文件**：`crates/deepseeknova-core/src/memory/engine.rs`
**问题**：`DefaultHasher` 算法未承诺跨 Rust 版本稳定，编译器升级后旧库条目去重 id
无法命中，产生重复记忆且跨入口去重失效。
**改动**：`content_id = prefix-{Sha256(text) hex[:16]}`（`sha2` 已是 workspace 依赖，
graph/store.rs 已用）
**测试**：同内容同 id；不同内容不同 id；既有去重测试全绿

---

## P1 性能组（T18-T19）

### T18. 记忆锁内嵌入扫描移出（审查 M6，medium）
**文件**：`crates/deepseeknova-core/src/memory/store.rs`
**问题**：召回时在全局 DB 锁内对全库 `node_embeddings` 线性扫描 + 余弦（锁持有 O(n)，
阻塞并发写入），归并阶段 O(n·m) `iter().find()`。
**改动**：锁外快照 id+blob 再计算；归并用 `HashMap<String, f32>`；SQL 层预筛可行时下推
**测试**：召回结果与现行为一致（既有 hybrid 测试全绿）；并发写不被长锁阻塞

### T19. `run_user_hook` 异步化（审查 M7，medium）
**文件**：`crates/deepseeknova-core/src/tool_hook.rs`、`crates/deepseeknova-agent/src/agent/loop_impl.rs`、`src/sub_agent.rs`
**问题**：`run_user_hook` 用 `std::process::Command` + 20ms 轮询 + `std::thread::sleep`
同步调用，最长 30s，阻塞 tokio worker（主循环与子代理路径均直接调用）。
**改动**：改 `tokio::process::Command`（或 `spawn_blocking`），调用点 await；
超时/裁决语义不变（fail-closed 契约保持）
**测试**：hook 调用不阻塞 worker（行为测试）；超时裁决不变（既有 hooks 测试全绿）

---

## 测试计划

| 分组 | 门禁 |
|---|---|
| T1-T7 安全组 | 每项聚焦测试 + 正反例；T1 必须有 `tar -tfI 'sh -c "id"'` 反例；T4 必须有工具调用回放用例 |
| T8-T10 数据/权限组 | T8 分层保留测试（对齐 memory 先例）；T9 二进制/空文件回滚；T10 缓存契约正反例 |
| T11-T15 正确性组 | T11 并发交叉测试；T12 取消传播；T13 模型断言；T14 审批清理；T15 遥测落盘 |
| T16-T19 一致性/性能组 | 行为等价断言（T17/T18/T19）+ 未来版本不破坏（T16） |
| 收尾（父级） | `cargo fmt` + `make check` 全量 EXIT=0（fmt+clippy+test+doc）；REVIEW.md 追加本任务书轮次 |

## 收尾（父级，不通过不算完成）

1. **依赖顺序**：T1-T10 完成后才启动 T11-T15（findings/子代理改造依赖安全组稳定基线）
2. **跨 crate 契约**：T3 能力位、T4 content-block、T9 Snapshot 契约变更必须同步
   相关文档（DESIGN.md 若涉及）+ 补 `///` 注释
3. **REVIEW.md**：追加 2026-08-11 轮次审查记录（评论表 + 修复验证）
4. **验证**：每项修复后先跑聚焦测试（`cargo test -p <crate> <过滤词>`），收尾全量
   `make check` EXIT=0；Windows 相关（T3）以 CI windows-latest 为准（本地交叉编译受
   libsqlite3-sys 限制）

## 假设

- 审查报告行号为快照，改动前必须 `read_file` 复核实际行号
- 工作区既有未提交改动不回退、不触碰（若有）
- P2 优化项（WireEvent 消费收敛、CLI 装配收敛、TUI 重绘、死代码面清理、regex/glob
  预编译等）列后续轮任务书，不在本任务书范围
- `make audit` 与云端安全审查按 AGENTS.md §3 回退路径执行（cargo-deny 未装则打印
  提示，不阻塞本任务书验收）

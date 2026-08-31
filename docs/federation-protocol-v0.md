# DeepseekNova Agent Federation 协议设计 v0（P5 · docs-first）

> 状态：**协议设计稿（未实现）**——本文是 master plan §4.10 / DESIGN.md §五 P5
> "需先出协议设计"的交付物。本文只描述 wire 协议与行为契约，不引入任何代码
> 变更；实现须另立 crate（暂名 `deepseeknova-federation`），不在本轮范围。
> 引用本文时保持 [规划中/未实现] 标注，直到首个实现 crate 落地。

## 1. 目标与非目标

**目标**：多个 DeepseekNova 实例（跨进程/跨机器）互相发现、声明能力、委派
任务、回传结构化结果；请求方合并。委托方（发起实例）对最终交付负责。

**非目标**：统一调度器/全局队列（违背去中心化）；跨实例共享会话状态或记忆
库；跨实例文件系统互访；鉴权体系（v0 假设传输层已互信，见 §7）。

## 2. 拓扑与发现

- 去中心化网状：实例通过 **Announce** 消息自报存在与能力；无全局注册中心。
- 发现通道 v0：**静态 peer 清单**（`[federation.peers]`，url + 拨号重试）。
  mDNS/DNS-SD 为 v1 扩展位。
- 每实例一个 **federation endpoint**（默认 `127.0.0.1:27190`，可配），承载
  下列双向消息。

## 3. 消息封套（wire 格式）

全部消息 = 单行 **NDJSON**（\n 分隔，每行一个 JSON 对象），与 serve crate 的
SSE/JSONL 风格一致，便于管道调试。封套字段：

```json
{"v":1,"kind":"<类型>","id":"<uuid>","from":"<agent_id>","to":null,
 "ts":"<RFC3339>","ttl":3,"payload":{...}}
```

| 字段 | 说明 |
|------|------|
| v | 协议版本，整数，不兼容变更必须递增 |
| kind | `announce`/`delegate`/`result`/`ping`/`pong`/`nack` |
| id | 消息 uuid；`delegate`/`result` 通过 `corr` 关联 |
| from/to | agent_id（实例启动生成 uuid 短码 + 可配置名）；to=null 广播 |
| ttl | 转发跳数上限；0 即不转发（v0 仅一跳，字段为 v1 预留） |
| corr | `result`/`nack` 必带：所回应 `delegate` 的 id |

## 4. 消息类型契约

### 4.1 announce（广播）
`payload: {"caps": {"tools": ["read_file", ...], "models": ["deepseek-chat", ...],
 "skills": ["frontend-developer", ...]}, "busy": false, "load": 0.42}`
- 能力声明 = 工具名白名单 + 模型 id + 已装 skill 名，全部为实例自报快照。
- 收到 announce 的实例更新本地 peer 表（含 30s 过期；过期 peer 不参与路由）。

### 4.2 delegate（单播）
`payload: {"task_id": "<uuid>", "goal": "<任务目标>",
 "constraints": {"readonly": true, "max_tool_calls": 64, "deadline_ms": 300000,
 "allowed_tools": [...]}, "context_b64": "<可选压缩上下文>"}`
- 委派即完整独立任务：目标文本 + 约束。约束字段必须逐项落实到子运行
  （readonly 对应 SubAgentRunner 既有只读冻结；max_tool_calls 对应
  security.limits；deadline 到点即 fail，不做优雅收尾）。
- `allowed_tools` 缺省 = 对端 announce 的工具集；显式给出则取交集，交集为空
  直接 nack（见 4.4）。

### 4.3 result（单播，corr=delegate.id）
`payload: {"task_id": "...", "status": "ok|failed|timeout|cancelled",
 "text": "<完成文本>", "evidence": {"files_touched": [...], "tool_calls": N,
 "verify_passed": true|false|null}, "usage": {"tokens_in": 0, "tokens_out": 0}}`
- `verify_passed: null` 表示对端未启用 verify 门；请求方不得当作 true 使用。
- 请求方负责合并多个 result（本协议不做服务端合并）。

### 4.4 nack（单播，corr=delegate.id）
`payload: {"task_id": "...", "reason": "busy|unsupported|caps_empty|deadline_too_short|protocol"}`
- 无法承接时**必须** nack，禁止静默丢弃 delegate。

### 4.5 ping / pong（单播或广播）
`payload: {"nonce": "<随机>"}`——pong 原样回 nonce。RTT 探活 + peer 表刷新。

## 5. 行为不变量（实现必须保证）

1. **委托不传染**：受托实例执行 delegate 时不得再对外发起 delegate（禁止
   递归联邦；delegate 内部用本地 SubAgentRunner 完成，深度受限且可观测）。
2. **只读默认**：delegate.constraints.readonly 缺省 true；受托方要放开写
   权限必须在 result.evidence 显式声明 files_touched（请求方可审计）。
3. **结果可信边界**：result.text 是对端自报，请求方合并时按外部输入对待
   （进 context 前经子代理输出净化管线 sanitize）。
4. **超时语义**：deadline 到点未完成 → status=timeout 的 result（能发则发），
   不能发时由请求方本地 deadline 兜底标记。
5. **幂等**：同一 task_id 重复 delegate → 直接重发上一次 result（受托方
   保留 task_id → result 缓存，LRU 64 条）。
6. **版本协商**：v 不匹配 → nack{reason:"protocol"}，双方不降级混跑。

## 6. 错误与恢复

- 传输错误（连接失败/写超时）：消息丢弃 + 指数退避重连（1s 起，×2，上限 30s）。
  delegate 无应用层重试（请求方负责用新 task_id 重发，避免双执行）。
- peer 表为空或全部过期：delegate 直接本地失败（错误文案指明"无可用联邦
  peer"），不降级为本地执行（请求方显式决定是否本地重跑）。

## 7. 安全边界（v0 假设与开放问题）

- v0 假设 peer 清单内实例互信（本机/内网）；**不做**鉴权与加密（文档明确
  留白，v1 才考虑 token/mtls）。远端 peer 的 result 一律视为不可信输入（§5.3）。
- 开放问题（实现前须回答）：agent_id 伪造如何审计？announce 洪泛限速？
  delegate 队列长度上限？

## 8. 与现有构造的衔接

- `SwarmAgent.provider: Arc<dyn Runner>` 指向远程实例 = federation runner 适配
  （实现期工作项，本设计只约定 wire 契约）。
- 受托方执行 = 现有 SubAgentRunner + readonly 冻结 + max_tool_calls 限制，
  复用现有 metrics/diagnose 钩子（evidence 字段即来源于此）。
- serve crate 可选暴露 /federation/status 只读端点（v1）。

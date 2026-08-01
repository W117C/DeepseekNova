# Compact 计量统一设计（指针优先，compact_model 降级回退）

- 日期：2026-07-31
- 状态：已确认（用户批准）
- 前置：一期模型指针（2026-07-29）、Compact 接线补丁、全路径接入 ModelRouter
  （2026-07-31）均已合入 main；B2 长任务续航已合入（引入 Agent L3 结构化压缩）

## 背景

B2 给 Agent 主循环引入了真实的 LLM 压缩（`L3Compactor`，带熔断；
`compact_provider` 为 None 时复用主 provider），并新增配置 `agent.compact_model`
（字符串）——runtime 在 `build_agent_with_task_provider` 内按该模型名手搓
clone+override 直连 factory 构建 provider（agent.rs L79-L125、runtime L451-L470）。
这产生两个问题：

1. **计量旁路**：L3 压缩调用不经 MeteredProvider，token 不入 CostLedger
2. **双旋钮**：`agent.compact_model` 与一期 `model_pointers.compact` 语义重叠

另：该直连段注释「工厂没有按模型名构造的入口」已过时——一期已提供
`factory::create_provider_with_model`。

此前「compact 参数是死参数」的结论随 B2 失效：Agent 现在是 compact provider
的真实消费者。

## 决策（已确认）

**指针优先，compact_model 降级为无 router 场景的回退**：
- CLI（有 router）：Compact 指针优先；指针未设而 compact_model 非空时，该模型
  改经 router 构建（B2 配置兼容且从此计量）；两者都设时指针胜
- runtime 直连回退仅服务无 router 的调用方（desktop 旧入口），待 desktop 接
  router 后自然消亡
- 不废弃 `agent.compact_model` 字段

## 方案选型

| 方案 | 结论 |
| --- | --- |
| A：新入口 + AgentRoleProviders 集合结构体（**选定**） | 终结逐参数追加的签名膨胀；未来 quick 角色零签名变更 |
| B：build_agent_with_task_provider 加第 8 参 | 函数名名不副实，下个角色再改签名 |
| C：runtime 持有 Arc<ModelRouter> | 耦合方向错误，弃 |

## runtime 层（deepseeknova-runtime/src/lib.rs）

新增角色 provider 集合与入口：

```rust
/// Role-based providers injected by callers that own a ModelRouter.
/// All fields optional; `None` falls back to legacy behaviour.
#[derive(Default)]
pub struct AgentRoleProviders {
    /// Delegate engine sub-agents (the `task` pointer).
    pub task: Option<Arc<dyn deepseeknova_provider::Provider>>,
    /// Agent L3 compaction (the `compact` pointer).
    pub compact: Option<Arc<dyn deepseeknova_provider::Provider>>,
}

pub fn build_agent_with_role_providers(
    config: &Config,
    workspace_root: PathBuf,
    provider: Arc<dyn deepseeknova_provider::Provider>,
    roles: AgentRoleProviders,
    max_steps: usize,
    gate: Option<Arc<PermissionGate>>,
    extra_tools: Vec<Arc<dyn deepseeknova_core::Tool>>,
) -> anyhow::Result<deepseeknova_agent::Agent>
```

- 现有 `build_agent`（roles 全 None）与 `build_agent_with_task_provider`
  （仅 task）改为委托新入口，签名与行为不变（desktop 零影响）
- **compact 优先级**：`roles.compact` 为 Some → `with_compact_provider(它)`，
  跳过直连段；None 且 `agent.compact_model` 非空 → 保留 B2 直连回退；
  两者皆无 → 不设（Agent L3 复用主 provider，B2 现状）
- 直连回退段改用 `factory::create_provider_with_model`（替换手搓
  clone+override），并修正过时注释；失败仅告警不阻断构建（保持 B2 语义）

## CLI 层（deepseeknova-cli/src/main.rs）

新增 helper（放在 `resolve_provider_cfg` 附近，带文档注释）：

```rust
/// Compact 角色 provider：指针优先；指针未设而 B2 的 agent.compact_model
/// 非空时，按该模型经 router 构建（照样计量）。L3 摘要是机械任务，按
/// Disabled 分类省 reasoning tokens（与 coordinator compact 决策一致）。
fn compact_provider_for(
    router: &deepseeknova_provider::router::ModelRouter,
    config: &deepseeknova_config::Config,
) -> anyhow::Result<Arc<dyn deepseeknova_provider::Provider>> {
    let override_model = if config.model_pointers.compact.is_none()
        && !config.agent.compact_model.is_empty()
    {
        Some(config.agent.compact_model.as_str())
    } else {
        None
    };
    router.provider_for_maybe_model(
        deepseeknova_provider::cost::ModelRole::Compact,
        override_model,
        Some(deepseeknova_provider::factory::ReasoningEffort::Disabled),
    )
}
```

5 个 build_agent 调用点（Run 单代理、Chat TUI、Serve、chat factory ×2）改传
`AgentRoleProviders { task: Some(...), compact: Some(compact_provider_for(...)?) }`；
本地 `build_agent` 包装的 `task_provider` 参数升级为 `roles: AgentRoleProviders`。

## 行为效果矩阵

| 配置 | CLI 行为 |
| --- | --- |
| 只设 pointers.compact | 走指针，Compact 计量 |
| 只设 agent.compact_model（B2 用户） | 该模型经 router，从不计量变为计量（兼容） |
| 两者都设 | 指针胜 |
| 都没设 | Compact 回落 main 指针模型 + Disabled（L3 摘要省 reasoning tokens 且计入 Compact 行） |

desktop 旧入口（`build_agent`）：行为与 B2 现状完全一致（直连回退仍生效）。

## 明确不做（YAGNI）

- desktop 接 router（后续任务③）
- 废弃/迁移 agent.compact_model 字段
- AgentRoleProviders 增加 quick/main 字段（等真实消费者出现）

## 测试计划

- runtime：优先级三分支（roles.compact 注入胜过 compact_model / compact_model
  回退生效 / 双无不设）——以可观测方式断言（构建成功 + 现有测试范式）
- CLI：`compact_provider_for` 的 override 判定单测（指针有/无 × compact_model
  有/无 四象限中关键三个）
- 既有测试无回归；`make check` 全量（跨 crate 强制）

## 假设与置信度

- 置信度：**高**
- 已验证：Agent::with_compact_provider 与 L3Compactor 消费链路（agent.rs
  L79-L125、L536+）；runtime 直连段位置（L440-L472）；build_agent 全部调用方
  （CLI L492、desktop core.rs L147、runtime 内部与测试）；create_provider_with_model
  可替换手搓路径
- 残余风险（低）：CLI helper 在「都没设」场景为 L3 引入 Disabled effort 的新
  实例（与主 provider 不同 thinking 配置）——这是有意决策（摘要省 reasoning），
  已在效果矩阵固化

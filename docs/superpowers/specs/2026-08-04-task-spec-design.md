# DeepseekNova 结构化任务书体系（TaskSpec）设计

- 日期：2026-08-04
- 状态：设计完成，待评审
- 范围：`deepseeknova-agent`（核心类型 + 两条委派路径接入）、`deepseeknova-tools`（delegate 工具 schema）、`deepseeknova-config`（inputs 覆盖字段）、`deepseeknova-runtime`（合并逻辑）。跨 4 crate，属架构级变更，执行时需 `make check` 全绿
- 设计依据：harness Cursor 插件 `create-agent` skill（task + inputs + RULES + allowed_tools + max_turns 五要素），映射到 DeepseekNova 现有 DelegatePreset / SubAgentConfig

## 背景与现状

DeepseekNova 有两条平行的委派路径，均只支持「角色 prompt + 工具白名单 + 步数上限」三元组：

1. **DelegateEngine**（`deepseeknova-agent/src/delegate.rs`）— 模型经 `delegate` 工具自主 spawn 子代理（explorer/coder/tester/reviewer 4 个内置 preset），`goal` 为模型自由生成文本，直接作为 `RunInput.prompt`
2. **SubAgentRunner**（`deepseeknova-agent/src/sub_agent.rs`）— 显式分发，prompt 协议 `sub_agent:<name>\ngoal:<text>`，System 消息 = 角色 prompt，User 消息 = goal

差距（对照 harness `create-agent`）：任务内容不可参数化（每次 spawn 都要模型现场组织完整任务文本）、无约束区（RULES 只能写死在 system_prompt 里，无法按任务注入）、无输入契约（required/default/类型校验缺失）、无模板复用（任务定义与调用点耦合）。

## 方案选型

| 方案 | 结论 |
| --- | --- |
| A：纯 Rust TaskSpec 类型，编译期注册（**选定**） | 与用户确认；两条路径统一接入一个核心类型，无文件系统、无第二个 DSL |
| B：文件化任务书库（`.deepseeknova/tasks/*.toml`） | 与 skills 体系对称但引入文件加载层；用户确认走 A，不采纳 |
| C：仅扩展 DelegatePreset | 双路径继续分叉，SubAgentRunner 无收益，不采纳 |

## 核心类型（新增 `deepseeknova-agent/src/task_spec.rs`）

```rust
/// 委派任务书：可参数化、带约束的任务定义
pub struct TaskSpec {
    pub name: String,
    pub task: String,           // 任务指令主体，支持 ${{ inputs.x }} 占位符
    pub rules: Vec<String>,     // RULES 约束区，渲染后追加
    pub inputs: Vec<InputSpec>, // 参数化输入声明
    pub tools: Vec<String>,     // 工具白名单（硬限制，沿用现有语义）
    pub max_steps: usize,       // 步数上限（沿用）
}

pub struct InputSpec {
    pub name: String,
    pub ty: InputType,          // String / Number / Boolean
    pub required: bool,
    pub default: Option<String>, // required=false 且无 default 时用空值
}

pub enum InputType { String, Number, Boolean }

/// 运行时提供的参数值（String 统一承载，render 只校验不转换）
pub struct InputValues(HashMap<String, String>);

pub struct RenderedTask {
    pub task: String,           // 占位符替换后的任务文本
    pub rules: String,          // RULES 块（空则空串）
}

impl TaskSpec {
    /// 纯函数、无 IO、幂等。校验顺序：未知输入引用 → required 缺失 → 类型合法性。
    /// 语义：UnknownInput 仅在 task 占位符引用了未声明的输入时触发（防拼写错误）；
    /// values 中未声明的多余键被忽略（调用方可复用同一 values map，config 传值
    /// 只对已有声明生效）。
    pub fn render(&self, values: &InputValues) -> Result<RenderedTask, TaskSpecError>;
}

pub enum TaskSpecError {
    MissingRequired(String),
    UnknownInput(String),
    InvalidType(String, InputType),
}
```

**模块归属推理**：TaskSpec 放 agent crate（而非 core）—— 两个消费方 DelegateEngine 与 SubAgentRunner 都在 agent crate，config/runtime 通过 agent 公开类型间接消费；上提 core 会制造「core 定义但只有 agent 使用」的孤儿契约。

**rules 软注入而非硬校验**：路径级语义校验属 security 层职责，扩散边界；工具白名单已是硬限制。harness 的 RULES 同样是注入 task 文本的提示区。

**InputType 仅三种标量**：YAGNI。secret 类输入将来需要时 `InputType::Secret` 是加法而非重构。

## 现有类型兼容策略

```rust
// delegate.rs — DelegatePreset 内嵌 TaskSpec，保留便捷构造
pub struct DelegatePreset {
    pub name: String,
    pub spec: TaskSpec,
}
impl DelegatePreset {
    /// 无参数化任务的便捷构造（内置 4 preset 走此路径，零行为变化）
    pub fn simple(name, system_prompt, tools, max_steps) -> Self;
}
```

```rust
// sub_agent.rs — SubAgentConfig 同样持有 TaskSpec
pub struct SubAgentConfig {
    pub name: String,
    pub spec: TaskSpec,
}
// builder（new/with_tools/with_max_steps）保留，内部委托 TaskSpec；
// 新增 with_task_spec(spec) 进入参数化路径
```

| 现有 API | 处理方式 |
| --- | --- |
| `DelegatePreset { name, system_prompt, tools, max_steps }` 字段直构 | 改为 `DelegatePreset::simple(...)`；`builtin_presets()` 4 个预设零行为变化 |
| `SubAgentConfig::new(...).with_tools(...).with_max_steps(...)` | builder 保留，内部委托 TaskSpec |
| `DelegateAgentOverride`（config） | 新增可选 `inputs: Vec<InputOverride>`（name + value）。**config 只负责传值，不负责定义任务书**（任务书是编译期资产；config 新增的 preset 视为 simple，无 inputs 声明，传值只对已有声明生效） |
| `merged_delegate_presets()`（runtime） | 覆盖逻辑加 inputs 合并进 preset 的 `InputValues` |
| `system_prompt` 字段 | 保留，不并入 TaskSpec.task：角色身份与任务内容分开，prompt 结构清晰，减少测试破坏面 |

## 数据流与渲染

**修正 1：inputs 来源绑定路径**（核实 `DelegateTool` 代码后修正）——DelegateEngine 的 goal 是模型自由文本、无 prompt 协议，因此「两种来源」不可套用于每条路径，改为各绑定一种：

| 路径 | inputs 来源 | 机制 |
| --- | --- | --- |
| DelegateEngine（模型自主 spawn） | delegate 工具 schema 新增可选 `inputs` 参数（JSON 对象） | `DelegateArgs` 加 `inputs: Option<HashMap<String, String>>`；模型显式传值 |
| SubAgentRunner（显式分发） | prompt 协议扩展 `input:<name>=<value>` 行 | `parse_input` 扩展；无 `input:` 行时行为不变（旧协议兼容） |

**修正 2：RULES 注入位置由调用方决定**——`render()` 返回 `RenderedTask { task, rules }`：

- DelegateEngine 路径：子 Agent 构造期注入 system_prompt，无法按次渲染 → `RunInput.prompt = rendered.task + rendered.rules`（与 harness RULES 放 task 尾部一致）
- SubAgentRunner 路径：System 消息 = system_prompt + rendered.rules；User 消息 = rendered.task

**渲染流程**：纯函数，无 IO。校验顺序（先快后慢，错误含输入名）：未知输入引用 → required 缺失 → 类型合法性（Number 可解析、Boolean 为 true/false）。UnknownInput 仅在 task 占位符引用未声明输入时触发，values 中多余键忽略。只校验不转换 —— 值始终以 String 传递。

**错误传播**：

- DelegateEngine 路径：`run()` 返回 Err → `DelegateTool` 转友好文本回给模型（与现有 unknown sub-agent 模式一致，模型可自愈重试）
- SubAgentRunner 路径：`run_stream()` 直接 Err

**已知限制（不修，超出范围）**：`DelegateTool::schema` 的 `agent` enum 硬编码 4 个内置名，config 新增 preset 不反映在 enum 中。

## 测试计划

| 层 | 测试 |
| --- | --- |
| `task_spec.rs` 单测 | 占位符替换、MissingRequired、UnknownInput、Number/Boolean 非法值、default 兜底、无 inputs 的 simple 模式 |
| `delegate.rs` 适配 | 现有 4 个 preset 测试改走 `p.spec` 字段；新增：带 inputs 的 preset 渲染后 prompt 含替换值 |
| `delegate.rs`（tools） | schema 传 `inputs` → 引擎收到渲染文本；inputs 缺失时回退 default |
| `sub_agent.rs` | `input:` 行解析、与 `goal:` 行共存、旧协议行为不变 |
| `runtime` 集成 | `DelegateAgentOverride.inputs` 合并进 preset 的 InputValues |

## 验收标准

1. `make check` 全绿（fmt + clippy + test + doc）
2. 内置 4 preset 行为零变化（既有 delegate 测试原样通过）
3. 新能力端到端可用：模型可经 delegate 工具传 inputs；SubAgentRunner 可经 prompt 传 inputs
4. 旧协议（无 inputs 的调用）完全兼容
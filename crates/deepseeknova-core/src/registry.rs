use indexmap::IndexMap;
use std::sync::Arc;

use crate::graph::ExecutionGraph;
use crate::tool::Tool;
use crate::types::ToolSchema;

// ---------------------------------------------------------------------------
// RegistryHub — unified registry for all named resources
// ---------------------------------------------------------------------------

/// 统一注册中心：聚合工具、provider、planner、skill、command 各子注册表。
pub struct RegistryHub {
    /// 工具注册表。
    pub tools: ToolRegistry,
    /// provider 工厂注册表。
    pub providers: ProviderRegistry,
    /// planner 注册表。
    pub planners: PlannerRegistry,
    /// skill 注册表。
    pub skills: SkillRegistry,
    /// command 注册表。
    pub commands: CommandRegistry,
}

impl RegistryHub {
    /// 创建一个所有子注册表均为空的 `RegistryHub`。
    pub fn new() -> Self {
        Self {
            tools: ToolRegistry::new(),
            providers: ProviderRegistry::new(),
            planners: PlannerRegistry::new(),
            skills: SkillRegistry::new(),
            commands: CommandRegistry::new(),
        }
    }

    /// Register a tool regardless of source (builtin, MCP, plugin, skill).
    pub fn register_tool(&mut self, tool: Arc<dyn Tool>) {
        self.tools.register(tool);
    }

    /// Look up a tool by name across all sub-registries.
    pub fn lookup_tool(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.lookup(name)
    }
}

impl Default for RegistryHub {
    /// 返回等价于 `RegistryHub::new()` 的空实例。
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tool Registry
// ---------------------------------------------------------------------------

/// 工具注册表：按工具名索引的有序集合。
pub struct ToolRegistry {
    tools: IndexMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// 创建空工具注册表。
    pub fn new() -> Self {
        Self {
            tools: IndexMap::new(),
        }
    }

    /// 注册一个工具（按 schema 名索引；同名覆盖并 warn）。
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.schema().name.clone();
        if self.tools.contains_key(&name) {
            tracing::warn!(
                tool = %name,
                "tool registered with duplicate name; previous registration silently overwritten"
            );
        }
        self.tools.insert(name, tool);
    }

    /// 按名查找工具。
    pub fn lookup(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// 返回所有已注册工具的 schema 列表。
    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.tools.values().map(|t| t.schema()).collect()
    }

    /// 返回所有已注册工具的名列表（按注册顺序）。
    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }
}

impl Default for ToolRegistry {
    /// 返回等价于 `ToolRegistry::new()` 的空实例。
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Provider Registry
// ---------------------------------------------------------------------------

/// P2 Fix #12: ProviderFactory uses Box-dyn-Fn instead of fn pointer
/// to allow closures with captured state (e.g. for provider configuration).
pub type ProviderFactory = Box<
    dyn Fn(ProviderConfigData) -> Result<Arc<dyn crate::runner::Runner>, crate::DeepseeknovaError>
        + Send
        + Sync,
>;

/// 构造 provider 所需的配置数据（由装配层从配置文件解析后传入工厂）。
#[derive(Debug, Clone)]
pub struct ProviderConfigData {
    /// provider 名称。
    pub name: String,
    /// provider 类型标识（用于查找工厂）。
    pub kind: String,
    /// 自定义 base URL（可选）。
    pub base_url: Option<String>,
    /// 模型名（可选）。
    pub model: Option<String>,
    /// 存放 API key 的环境变量名（可选）。
    pub api_key_env: Option<String>,
}

/// provider 工厂注册表：按 kind 索引的工厂闭包集合。
pub struct ProviderRegistry {
    factories: IndexMap<String, ProviderFactory>,
}

impl ProviderRegistry {
    /// 创建空 provider 注册表。
    pub fn new() -> Self {
        Self {
            factories: IndexMap::new(),
        }
    }

    /// 注册一个 provider 工厂（按 kind 索引）。
    pub fn register(&mut self, kind: impl Into<String>, factory: ProviderFactory) {
        self.factories.insert(kind.into(), factory);
    }

    /// P2 Fix #12: Added lookup method for ProviderRegistry.
    pub fn lookup(&self, kind: &str) -> Option<&ProviderFactory> {
        self.factories.get(kind)
    }
}

impl Default for ProviderRegistry {
    /// 返回等价于 `ProviderRegistry::new()` 的空实例。
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Planner Registry
// ---------------------------------------------------------------------------

/// 计划器 trait：将目标文本转为执行图。
#[async_trait::async_trait]
pub trait Planner: Send + Sync {
    /// planner 名称（注册键）。
    fn name(&self) -> &str;
    /// Produce an execution graph for a given goal.
    async fn plan(&self, goal: &str) -> Result<ExecutionGraph, crate::DeepseeknovaError>;
}

/// planner 注册表：按名索引。
pub struct PlannerRegistry {
    planners: IndexMap<String, Arc<dyn Planner>>,
}

impl PlannerRegistry {
    /// 创建空 planner 注册表。
    pub fn new() -> Self {
        Self {
            planners: IndexMap::new(),
        }
    }

    /// 注册一个 planner（按 `name()` 索引）。
    pub fn register(&mut self, planner: Arc<dyn Planner>) {
        self.planners.insert(planner.name().to_string(), planner);
    }

    /// 按名查找 planner。
    pub fn lookup(&self, name: &str) -> Option<Arc<dyn Planner>> {
        self.planners.get(name).cloned()
    }
}

impl Default for PlannerRegistry {
    /// 返回等价于 `PlannerRegistry::new()` 的空实例。
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Skill Registry
// ---------------------------------------------------------------------------

/// 技能来源层级，同名冲突时高优先级覆盖低优先级：`Project > User > Builtin`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SkillScope {
    /// crate 内置技能（随 deepseeknova-skills 分发，优先级最低）。
    Builtin,
    /// 用户级技能（`~/.deepseeknova/skills/`）。
    User,
    /// 项目级技能（`.deepseeknova/skills/` / `.agents/skills/`，优先级最高）。
    #[default]
    Project,
}

impl SkillScope {
    /// 展示标签（`/skills` 列表用）。
    pub fn label(self) -> &'static str {
        match self {
            SkillScope::Builtin => "builtin",
            SkillScope::User => "user",
            SkillScope::Project => "project",
        }
    }
}

/// 一个 skill 的定义（名称、描述、允许工具、系统提示等）。
#[derive(Debug, Clone)]
pub struct Skill {
    /// skill 名称（注册键）。
    pub name: String,
    /// skill 描述。
    pub description: String,
    /// 指定使用的模型（可选）。
    pub model: Option<String>,
    /// 允许该 skill 使用的工具名列表。
    pub tools_allowed: Vec<String>,
    /// 该 skill 的系统提示。
    pub system_prompt: String,
    /// 来源层级（用于同名覆盖与展示）。
    pub scope: SkillScope,
}

/// skill 注册表：按名索引。
pub struct SkillRegistry {
    skills: IndexMap<String, Skill>,
}

impl SkillRegistry {
    /// 创建空 skill 注册表。
    pub fn new() -> Self {
        Self {
            skills: IndexMap::new(),
        }
    }

    /// 注册一个 skill（按 `name` 索引，同名覆盖）。
    pub fn register(&mut self, skill: Skill) {
        self.skills.insert(skill.name.clone(), skill);
    }

    /// 按名查找 skill。
    pub fn lookup(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }
}

impl Default for SkillRegistry {
    /// 返回等价于 `SkillRegistry::new()` 的空实例。
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Command Registry
// ---------------------------------------------------------------------------

/// 一条命令的定义（用户面斜杠命令）。
#[derive(Debug, Clone)]
pub struct Command {
    /// 命令名。
    pub name: String,
    /// 命令描述。
    pub description: String,
    /// 是否为内置命令。
    pub builtin: bool,
}

/// command 注册表：按名索引。
pub struct CommandRegistry {
    commands: IndexMap<String, Command>,
}

impl CommandRegistry {
    /// 创建空 command 注册表。
    pub fn new() -> Self {
        Self {
            commands: IndexMap::new(),
        }
    }

    /// 注册一条命令（按 `name` 索引，同名覆盖）。
    pub fn register(&mut self, command: Command) {
        self.commands.insert(command.name.clone(), command);
    }

    /// 按名查找命令。
    pub fn lookup(&self, name: &str) -> Option<&Command> {
        self.commands.get(name)
    }
}

impl Default for CommandRegistry {
    /// 返回等价于 `CommandRegistry::new()` 的空实例。
    fn default() -> Self {
        Self::new()
    }
}

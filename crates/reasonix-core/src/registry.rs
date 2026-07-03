use std::collections::HashMap;
use std::sync::Arc;

use crate::graph::ExecutionGraph;
use crate::tool::Tool;
use crate::types::ToolSchema;

// ---------------------------------------------------------------------------
// RegistryHub — unified registry for all named resources
// ---------------------------------------------------------------------------

pub struct RegistryHub {
    pub tools: ToolRegistry,
    pub providers: ProviderRegistry,
    pub planners: PlannerRegistry,
    pub skills: SkillRegistry,
    pub commands: CommandRegistry,
}

impl RegistryHub {
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
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tool Registry
// ---------------------------------------------------------------------------

pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.schema().name.clone();
        self.tools.insert(name, tool);
    }

    pub fn lookup(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.tools.values().map(|t| t.schema()).collect()
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Provider Registry (thin — ProviderManager does heavy lifting)
// ---------------------------------------------------------------------------

pub type ProviderFactory =
    fn(crate::config::ProviderConfigData) -> anyhow::Result<Arc<dyn crate::runner::Runner>>;

pub struct ProviderRegistry {
    factories: HashMap<String, ProviderFactory>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            factories: HashMap::new(),
        }
    }

    pub fn register(&mut self, kind: impl Into<String>, factory: ProviderFactory) {
        self.factories.insert(kind.into(), factory);
    }
}

/// Placeholder types for config — will be replaced by reasonix-config structs.
pub mod config {
    #[derive(Debug, Clone)]
    pub struct ProviderConfigData {
        pub name: String,
        pub kind: String,
        pub base_url: Option<String>,
        pub model: Option<String>,
        pub api_key_env: Option<String>,
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Planner Registry
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
pub trait Planner: Send + Sync {
    fn name(&self) -> &str;
    /// Produce an execution graph for a given goal.
    async fn plan(&self, goal: &str) -> anyhow::Result<ExecutionGraph>;
}

pub struct PlannerRegistry {
    planners: HashMap<String, Arc<dyn Planner>>,
}

impl PlannerRegistry {
    pub fn new() -> Self {
        Self {
            planners: HashMap::new(),
        }
    }

    pub fn register(&mut self, planner: Arc<dyn Planner>) {
        self.planners.insert(planner.name().to_string(), planner);
    }

    pub fn lookup(&self, name: &str) -> Option<Arc<dyn Planner>> {
        self.planners.get(name).cloned()
    }
}

impl Default for PlannerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Skill Registry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub model: Option<String>,
    pub tools_allowed: Vec<String>,
    pub system_prompt: String,
}

pub struct SkillRegistry {
    skills: HashMap<String, Skill>,
}

impl SkillRegistry {
    pub fn new() -> Self {
        Self {
            skills: HashMap::new(),
        }
    }

    pub fn register(&mut self, skill: Skill) {
        self.skills.insert(skill.name.clone(), skill);
    }

    pub fn lookup(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }
}

impl Default for SkillRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Command Registry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Command {
    pub name: String,
    pub description: String,
    pub builtin: bool,
}

pub struct CommandRegistry {
    commands: HashMap<String, Command>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self {
            commands: HashMap::new(),
        }
    }

    pub fn register(&mut self, command: Command) {
        self.commands.insert(command.name.clone(), command);
    }

    pub fn lookup(&self, name: &str) -> Option<&Command> {
        self.commands.get(name)
    }
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

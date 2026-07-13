use std::sync::Arc;

use dpronix_config::Config;
use dpronix_context::ContextProvider;
use dpronix_core::registry::RegistryHub;
use dpronix_core::runner::{RunEventStream, RunInput, Runner};
use dpronix_event::EventBus;
use dpronix_permission::{Decision, PermissionGate, Policy};

/// Runtime is the composition root. It wires registry, context, events,
/// and permission together. Agent, Planner, SubAgent, Server all share
/// one Runtime.
pub struct Runtime {
    pub registry: Arc<std::sync::RwLock<RegistryHub>>,
    pub context: Arc<dyn ContextProvider>,
    pub events: Arc<EventBus>,
    pub permission: Arc<PermissionGate>,
    pub config: Arc<Config>,
}

impl Runtime {
    /// Create a Runtime with a given context provider.
    pub fn new(config: Config, context: Arc<dyn ContextProvider>) -> anyhow::Result<Self> {
        let permission = build_permission_gate(&config);

        Ok(Self {
            registry: Arc::new(std::sync::RwLock::new(RegistryHub::new())),
            context,
            events: Arc::new(EventBus::new(256)),
            permission: Arc::new(permission),
            config: Arc::new(config),
        })
    }

    /// Execute a Runner and return a stream of events.
    /// Events emitted during execution are published on the shared EventBus.
    pub async fn run(
        &self,
        runner: &dyn Runner,
        input: RunInput,
    ) -> anyhow::Result<RunEventStream> {
        self.events
            .publish(dpronix_event::AgentEvent::ModelStarted {
                provider: "default".to_string(),
                model: input
                    .model_override
                    .clone()
                    .unwrap_or_else(|| "default".to_string()),
            });

        runner.run_stream(input).await
    }

    /// Check whether a tool call is allowed by the permission policy.
    pub fn check_permission(&self, tool: &dyn dpronix_core::Tool, args: &str) -> Decision {
        self.permission.check(tool, args)
    }
}

/// Build a PermissionGate from Config.
fn build_permission_gate(config: &Config) -> PermissionGate {
    let mut allow = Vec::new();
    let mut ask = Vec::new();
    let mut deny = Vec::new();

    for rule in &config.permissions.rules {
        let r = if let Some(ref subject) = rule.subject {
            dpronix_permission::Rule::with_subject(&rule.tool, subject)
        } else {
            dpronix_permission::Rule::new(&rule.tool)
        };

        match rule.mode {
            dpronix_config::PermissionMode::Allow => allow.push(r),
            dpronix_config::PermissionMode::Ask => ask.push(r),
            dpronix_config::PermissionMode::Deny => deny.push(r),
        }
    }

    let mode = match config.permissions.default_mode {
        dpronix_config::PermissionMode::Allow => Decision::Allow,
        dpronix_config::PermissionMode::Ask => Decision::Ask,
        dpronix_config::PermissionMode::Deny => Decision::Deny,
    };

    PermissionGate::new(Policy {
        mode,
        allow,
        ask,
        deny,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dpronix_config::Config;
    use dpronix_context::ContextEngine;

    #[test]
    fn runtime_builds_with_default_config() {
        let config = Config::default();
        // Use a temp dir to avoid scanning the full project tree
        let dir = std::env::temp_dir()
            .join(format!("dpronix-rt-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let context = ContextEngine::new(dir.clone()).unwrap();
        let context: Arc<dyn ContextProvider> = Arc::new(context);

        let runtime = Runtime::new(config, context).unwrap();
        assert_eq!(runtime.events.receiver_count(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}

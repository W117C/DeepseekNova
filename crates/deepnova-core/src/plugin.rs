//! # Plugin System
//!
//! Extensible plugin framework for deepnova. Inspired by Ruflo's 35+ plugin
//! ecosystem and ECC's cross-tool plugin system.
//!
//! ## Architecture
//!
//! ```text
//! Plugin trait
//!   ├─ BuiltinPlugin (compile-time)
//!   ├─ McpPlugin (MCP server integration)
//!   └─ ScriptPlugin (shell scripts, custom tools)
//!
//! PluginRegistry
//!   ├─ register(plugin) → plugin_id
//!   ├─ lookup(id) → plugin
//!   └─ list() → plugin info
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// Information about a plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: Option<String>,
    pub homepage: Option<String>,
    /// Tool names this plugin provides.
    pub tools: Vec<String>,
    /// Whether this plugin is enabled.
    pub enabled: bool,
}

/// The Plugin trait — all plugins must implement this.
pub trait Plugin: Send + Sync {
    /// Unique plugin name.
    fn name(&self) -> &str;

    /// Plugin version.
    fn version(&self) -> &str;

    /// Human-readable description.
    fn description(&self) -> &str;

    /// Initialize the plugin. Called once at registration.
    fn init(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    /// Get the plugin's info for the registry.
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: self.name().to_string(),
            version: self.version().to_string(),
            description: self.description().to_string(),
            author: None,
            homepage: None,
            tools: vec![],
            enabled: true,
        }
    }
}

/// Registry that holds all registered plugins.
pub struct PluginRegistry {
    plugins: HashMap<String, Arc<dyn Plugin>>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self {
            plugins: HashMap::new(),
        }
    }

    /// Register a plugin.
    pub fn register(&mut self, plugin: Arc<dyn Plugin>) {
        let name = plugin.name().to_string();
        self.plugins.insert(name, plugin);
    }

    /// Look up a plugin by name.
    pub fn lookup(&self, name: &str) -> Option<Arc<dyn Plugin>> {
        self.plugins.get(name).cloned()
    }

    /// List all registered plugin info.
    pub fn list(&self) -> Vec<PluginInfo> {
        self.plugins.values().map(|p| p.info()).collect()
    }

    /// Number of registered plugins.
    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    /// Check if any plugins are registered.
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}

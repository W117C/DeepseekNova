//! Role → model → provider routing (Kode-style model pointers).
//!
//! [`ModelRouter`] resolves the `[model_pointers]` config section into
//! lazily-built, cached provider instances. Every returned provider is
//! wrapped in a [`MeteredProvider`] so token
//! accounting happens transparently.

use crate::cost::{CostLedger, MeteredProvider, ModelPrices, ModelRole, PriceTable};
use crate::factory::{self, ReasoningEffort};
use crate::Provider;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

/// Cache key: (provider name, model name, effort label). Effort participates
/// because it changes provider construction (thinking mode / effort string).
type CacheKey = (String, String, &'static str);

fn effort_key(effort: Option<ReasoningEffort>) -> &'static str {
    match effort {
        None => "config",
        Some(ReasoningEffort::Disabled) => "disabled",
        Some(ReasoningEffort::High) => "high",
        Some(ReasoningEffort::Max) => "max",
    }
}

/// Resolves model pointers to cached provider instances.
pub struct ModelRouter {
    config: deepseeknova_config::Config,
    pointers: RwLock<HashMap<ModelRole, String>>,
    cache: Mutex<HashMap<CacheKey, Arc<dyn Provider>>>,
    ledger: Arc<CostLedger>,
}

impl ModelRouter {
    /// Build from config. Re-runs [`Config::validate`](deepseeknova_config::Config::validate)
    /// so programmatically constructed configs get the same dangling-pointer
    /// guarantees as `Config::load`.
    pub fn from_config(
        config: &deepseeknova_config::Config,
        ledger: Arc<CostLedger>,
    ) -> Result<Self, deepseeknova_core::DeepseeknovaError> {
        config.validate()?;
        let mut pointers = HashMap::new();
        for (role_name, ptr) in config.model_pointers.entries() {
            if let (Some(role), Some(model)) = (ModelRole::parse(role_name), ptr.as_ref()) {
                pointers.insert(role, model.clone());
            }
        }
        Ok(Self {
            config: config.clone(),
            pointers: RwLock::new(pointers),
            cache: Mutex::new(HashMap::new()),
            ledger,
        })
    }

    /// The shared cost ledger fed by all providers this router hands out.
    pub fn ledger(&self) -> Arc<CostLedger> {
        Arc::clone(&self.ledger)
    }

    /// Current model for a role: role pointer → `main` pointer → `None`
    /// (caller falls back to legacy default-provider resolution).
    pub fn pointer(&self, role: ModelRole) -> Option<String> {
        let ptrs = self.pointers.read().unwrap_or_else(|e| e.into_inner());
        ptrs.get(&role)
            .or_else(|| {
                if role != ModelRole::Main {
                    ptrs.get(&ModelRole::Main)
                } else {
                    None
                }
            })
            .cloned()
    }

    /// In-session hot switch (memory only, never persisted). Fails when the
    /// model is not defined in `[[models]]`.
    pub fn set_pointer(
        &self,
        role: ModelRole,
        model: &str,
    ) -> Result<(), deepseeknova_core::DeepseeknovaError> {
        if self.config.find_model(model).is_none() {
            let known: Vec<&str> = self.config.models.iter().map(|m| m.name.as_str()).collect();
            return Err(deepseeknova_core::DeepseeknovaError::Config(format!(
                "unknown model '{model}' for pointer '{}' (known models: {})",
                role.label(),
                known.join(", ")
            )));
        }
        self.pointers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(role, model.to_string());
        Ok(())
    }

    /// Metered provider for a role. Pointer-less roles use the legacy
    /// default (first provider entry, its own default model).
    pub fn provider_for(
        &self,
        role: ModelRole,
        effort: Option<ReasoningEffort>,
    ) -> Result<Arc<dyn Provider>, deepseeknova_core::DeepseeknovaError> {
        match self.pointer(role) {
            Some(model) => self.provider_for_model(&model, role, effort),
            None => self.default_provider(role, effort),
        }
    }

    /// Metered provider for an explicit model name (e.g. `/model switch`),
    /// still accounted under `role`. The `[[models]].temperature` entry is
    /// threaded into the provider so the request body carries it.
    pub fn provider_for_model(
        &self,
        model_name: &str,
        role: ModelRole,
        effort: Option<ReasoningEffort>,
    ) -> Result<Arc<dyn Provider>, deepseeknova_core::DeepseeknovaError> {
        let pcfg = self
            .config
            .resolve_provider_for_model(model_name)
            .ok_or_else(|| {
                deepseeknova_core::DeepseeknovaError::Config(format!(
                    "no provider found for model '{model_name}'"
                ))
            })?;
        // Per-model sampling temperature from [[models]]; `None` (unset) keeps
        // the provider default — no temperature field in the request body.
        let temperature = self
            .config
            .find_model(model_name)
            .and_then(|m| m.temperature);
        let key: CacheKey = (
            pcfg.name.clone(),
            model_name.to_string(),
            effort_key(effort),
        );
        let raw = self.cached_or_build(key, || {
            Ok(factory::create_provider_with_model_temperature(
                pcfg,
                model_name,
                temperature,
                effort,
            )?
            .into())
        })?;
        Ok(Arc::new(MeteredProvider::new(
            raw,
            role,
            model_name,
            Arc::clone(&self.ledger),
        )))
    }

    /// Provider for a role with an optional explicit model override:
    /// `Some(model)` routes via [`Self::provider_for_model`], `None` via
    /// [`Self::provider_for`]. Accounting stays under `role` either way.
    pub fn provider_for_maybe_model(
        &self,
        role: ModelRole,
        model_override: Option<&str>,
        effort: Option<ReasoningEffort>,
    ) -> Result<Arc<dyn Provider>, deepseeknova_core::DeepseeknovaError> {
        match model_override {
            Some(model) => self.provider_for_model(model, role, effort),
            None => self.provider_for(role, effort),
        }
    }

    fn default_provider(
        &self,
        role: ModelRole,
        effort: Option<ReasoningEffort>,
    ) -> Result<Arc<dyn Provider>, deepseeknova_core::DeepseeknovaError> {
        let pcfg = self.config.providers.first().ok_or_else(|| {
            deepseeknova_core::DeepseeknovaError::Config("no providers configured".into())
        })?;
        let model_label = pcfg
            .model
            .clone()
            .unwrap_or_else(|| "(default)".to_string());
        let key: CacheKey = (pcfg.name.clone(), model_label.clone(), effort_key(effort));
        let raw = self.cached_or_build(key, || {
            Ok(factory::create_provider_for_task(pcfg, effort)?.into())
        })?;
        Ok(Arc::new(MeteredProvider::new(
            raw,
            role,
            model_label,
            Arc::clone(&self.ledger),
        )))
    }

    fn cached_or_build(
        &self,
        key: CacheKey,
        build: impl FnOnce() -> Result<Arc<dyn Provider>, deepseeknova_core::DeepseeknovaError>,
    ) -> Result<Arc<dyn Provider>, deepseeknova_core::DeepseeknovaError> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = cache.get(&key) {
            return Ok(Arc::clone(p));
        }
        let built = build()?;
        cache.insert(key, Arc::clone(&built));
        Ok(built)
    }

    /// Number of distinct cached raw provider instances (for tests/insight).
    pub fn cached_instances(&self) -> usize {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Price table for [`CostLedger::report`], built from `[[models]]`.
    pub fn price_table(&self) -> PriceTable {
        self.config
            .models
            .iter()
            .map(|m| {
                (
                    m.name.clone(),
                    ModelPrices {
                        input_per_mtok: m.input_price_per_mtok,
                        output_per_mtok: m.output_price_per_mtok,
                        cache_hit_per_mtok: m.cache_hit_price_per_mtok,
                    },
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cost::{CostLedger, ModelRole};
    use std::sync::Arc;

    fn router() -> ModelRouter {
        std::env::set_var("DPNOVA_ROUTER_TEST_KEY", "test");
        let cfg: deepseeknova_config::Config = toml::from_str(
            r#"
            [[providers]]
            name = "deepseek"
            kind = "openai"
            api_key_env = "DPNOVA_ROUTER_TEST_KEY"

            [[models]]
            name = "big"
            provider = "deepseek"
            input_price_per_mtok = 2.0
            output_price_per_mtok = 8.0

            [[models]]
            name = "small"
            provider = "deepseek"

            [model_pointers]
            main = "big"
            task = "small"
        "#,
        )
        .unwrap();
        ModelRouter::from_config(&cfg, Arc::new(CostLedger::new())).unwrap()
    }

    #[test]
    fn pointer_resolution_and_fallback() {
        let r = router();
        assert_eq!(r.pointer(ModelRole::Main).as_deref(), Some("big"));
        assert_eq!(r.pointer(ModelRole::Task).as_deref(), Some("small"));
        // 未配置的角色回落 main
        assert_eq!(r.pointer(ModelRole::Compact).as_deref(), Some("big"));
        assert_eq!(r.pointer(ModelRole::Quick).as_deref(), Some("big"));
    }

    #[test]
    fn cache_isolates_models_and_shares_same_model() {
        let r = router();
        r.provider_for(ModelRole::Main, None).unwrap();
        r.provider_for(ModelRole::Task, None).unwrap();
        assert_eq!(r.cached_instances(), 2, "big 与 small 各一实例");
        // Compact 回落 main → 命中 big 缓存，不新增
        r.provider_for(ModelRole::Compact, None).unwrap();
        assert_eq!(r.cached_instances(), 2);
    }

    #[test]
    fn hot_switch_validates_and_takes_effect() {
        let r = router();
        assert!(r.set_pointer(ModelRole::Quick, "no-such").is_err());
        r.set_pointer(ModelRole::Quick, "small").unwrap();
        assert_eq!(r.pointer(ModelRole::Quick).as_deref(), Some("small"));
    }

    #[test]
    fn maybe_model_override_and_fallback() {
        let r = router();
        // None → 走角色指针（Task→small）
        r.provider_for_maybe_model(ModelRole::Task, None, None)
            .unwrap();
        assert_eq!(r.cached_instances(), 1, "small 一个实例");
        // Some → 显式覆盖（big），仍按该角色计量
        r.provider_for_maybe_model(ModelRole::Task, Some("big"), None)
            .unwrap();
        assert_eq!(r.cached_instances(), 2, "big 新增一个实例");
    }

    #[test]
    fn price_table_from_config() {
        let r = router();
        let t = r.price_table();
        assert_eq!(t.get("big").unwrap().input_per_mtok, Some(2.0));
        assert!(t.get("small").unwrap().input_per_mtok.is_none());
    }

    /// A model with `temperature` configured must flow through
    /// `provider_for_model` into the factory (request-body assertions live in
    /// the factory tests — this guards the router→factory plumbing itself).
    #[test]
    fn provider_for_model_threads_model_temperature() {
        std::env::set_var("DPNOVA_ROUTER_TEMP_KEY", "test");
        let cfg: deepseeknova_config::Config = toml::from_str(
            r#"
            [[providers]]
            name = "deepseek"
            kind = "openai"
            api_key_env = "DPNOVA_ROUTER_TEMP_KEY"

            [[models]]
            name = "warm"
            provider = "deepseek"
            temperature = 0.5
            "#,
        )
        .unwrap();
        let r = ModelRouter::from_config(&cfg, Arc::new(CostLedger::new())).unwrap();
        let p = r.provider_for_model("warm", ModelRole::Main, None);
        assert!(p.is_ok(), "{:?}", p.err());
    }
}

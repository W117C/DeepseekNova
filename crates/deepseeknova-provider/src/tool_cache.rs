//! Bounded per-provider cache for built tool-schema payloads.
//!
//! Within a session the live tool set is stable (a registry is populated once
//! and only changes on register/disable), yet the request builders used to
//! re-`collect()` every [`Tool::schema`], sort by name and serialise the
//! ~4.6 KB payload on *every* request — the hottest allocation on the provider
//! path.
//!
//! The cache keys on the sorted, de-duplicated set of tool object addresses:
//! an unchanged registry reuses the previous serialisation, while any
//! register/disable changes the live set and naturally misses (rebuild). The
//! map is capped; when the cap is exceeded the whole map is cleared, which is
//! fine because a session rarely exercises more than a handful of distinct
//! tool sets.

use deepseeknova_core::Tool;
use std::collections::HashMap;
use std::sync::Mutex;

/// Bounded cache mapping a tool identity set → its serialised request payload.
pub(crate) struct ToolSchemaCache<V> {
    inner: Mutex<HashMap<Vec<usize>, V>>,
    max_entries: usize,
}

impl<V> ToolSchemaCache<V> {
    /// Create a cache that evicts the whole map once it holds more than
    /// `max_entries` distinct tool sets.
    pub(crate) fn with_capacity(max_entries: usize) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            max_entries,
        }
    }

    /// Return the cached payload for `tools`, or build it via `build` on a
    /// miss and store it. Returns `None` when `tools` is empty — callers
    /// treat that as "no tools advertised" and omit the field entirely.
    ///
    /// A poisoned lock is recovered (`into_inner`) so a panicking caller can
    /// never wedge the request path.
    pub(crate) fn get_or_build(
        &self,
        tools: &[&dyn Tool],
        build: impl FnOnce(&[&dyn Tool]) -> V,
    ) -> Option<V>
    where
        V: Clone,
    {
        let key = identity_key(tools)?;
        let mut guard = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(cached) = guard.get(&key) {
            return Some(cached.clone());
        }
        let value = build(tools);
        if guard.len() >= self.max_entries {
            guard.clear();
        }
        guard.insert(key, value.clone());
        Some(value)
    }
}

/// Sorted, de-duplicated set of tool object data addresses — a stable identity
/// for "the same tool registry". Returns `None` when the set is empty.
fn identity_key(tools: &[&dyn Tool]) -> Option<Vec<usize>> {
    let mut addrs: Vec<usize> = tools
        .iter()
        .map(|t| *t as *const dyn Tool as *const () as usize)
        .collect();
    if addrs.is_empty() {
        return None;
    }
    addrs.sort_unstable();
    addrs.dedup();
    Some(addrs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_core::tool::ToolContext;
    use deepseeknova_core::types::ToolSchema;
    use deepseeknova_core::Tool;

    struct DummyTool;

    #[async_trait::async_trait]
    impl Tool for DummyTool {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: "dummy".into(),
                description: "does nothing".into(),
                parameters: serde_json::json!({"type": "object"}),
            }
        }

        async fn execute(&self, _ctx: &ToolContext, _args: &str) -> anyhow::Result<String> {
            Ok("ok".into())
        }
    }

    /// The load-bearing property: a second call with the same tool set must
    /// not re-run the build closure.
    #[test]
    fn same_tool_set_hits_cache() {
        let cache = ToolSchemaCache::with_capacity(8);
        let tool = DummyTool;
        let tools: [&dyn Tool; 1] = [&tool];

        let mut builds = 0;
        let v1 = cache
            .get_or_build(&tools, |_| {
                builds += 1;
                "built-once".to_string()
            })
            .unwrap();
        let v2 = cache
            .get_or_build(&tools, |_| {
                builds += 1;
                "rebuilt".to_string()
            })
            .unwrap();

        assert_eq!(v1, "built-once");
        assert_eq!(v2, "built-once", "second call must hit the cache");
        assert_eq!(builds, 1, "build closure must run exactly once");
    }

    /// A different tool set (a register/disable event) must miss and rebuild.
    #[test]
    fn different_tool_set_misses_cache() {
        let cache = ToolSchemaCache::with_capacity(8);
        let tool_a = DummyTool;
        let tool_b = DummyTool; // distinct object → distinct address
        let set_a: [&dyn Tool; 1] = [&tool_a];
        let set_b: [&dyn Tool; 1] = [&tool_b];

        let v1 = cache.get_or_build(&set_a, |_| "A".to_string()).unwrap();
        let v2 = cache.get_or_build(&set_b, |_| "B".to_string()).unwrap();

        assert_eq!(v1, "A");
        assert_eq!(v2, "B", "a different tool set must rebuild");
    }

    /// Order within the slice must not matter — the key is a set.
    #[test]
    fn reordered_slice_hits_cache() {
        let cache = ToolSchemaCache::with_capacity(8);
        let tool_a = DummyTool;
        let tool_b = DummyTool;
        let fwd: [&dyn Tool; 2] = [&tool_a, &tool_b];
        let rev: [&dyn Tool; 2] = [&tool_b, &tool_a];

        let mut builds = 0;
        let _ = cache
            .get_or_build(&fwd, |_| {
                builds += 1;
                "set".to_string()
            })
            .unwrap();
        let v = cache
            .get_or_build(&rev, |_| {
                builds += 1;
                "rebuilt".to_string()
            })
            .unwrap();

        assert_eq!(v, "set");
        assert_eq!(builds, 1, "reordering the same tools must hit the cache");
    }

    /// An empty tool set maps to `None` (callers omit the tools field).
    #[test]
    fn empty_tool_set_is_none() {
        let cache = ToolSchemaCache::<String>::with_capacity(8);
        let tools: [&dyn Tool; 0] = [];
        assert!(cache
            .get_or_build(&tools, |_| "unused".to_string())
            .is_none());
    }
}

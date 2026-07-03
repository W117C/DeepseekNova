use criterion::{black_box, criterion_group, criterion_main, Criterion};
use reasonix_core::registry::RegistryHub;
use reasonix_core::{Tool, ToolContext, ToolSchema};
use std::sync::Arc;

/// Minimal tool for benchmarking.
struct BenchTool {
    name: &'static str,
}

#[async_trait::async_trait]
impl Tool for BenchTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: self.name.to_string(),
            description: "benchmark tool".to_string(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }
    }

    async fn execute(&self, _ctx: &ToolContext, _args: &str) -> anyhow::Result<String> {
        Ok("ok".to_string())
    }
}

fn bench_registry_register(c: &mut Criterion) {
    let mut hub = RegistryHub::new();

    c.bench_function("registry/register_1_tool", |b| {
        b.iter(|| {
            hub.register_tool(Arc::new(BenchTool { name: "bench" }));
        })
    });
}

fn bench_registry_register_many(c: &mut Criterion) {
    c.bench_function("registry/register_100_tools", |b| {
        b.iter(|| {
            let mut hub = RegistryHub::new();
            for i in 0..100 {
                let name = Box::leak(format!("tool_{i}").into_boxed_str());
                hub.register_tool(Arc::new(BenchTool { name }));
            }
            black_box(hub);
        })
    });
}

fn bench_registry_lookup(c: &mut Criterion) {
    let mut hub = RegistryHub::new();
    for i in 0..100 {
        let name = Box::leak(format!("tool_{i}").into_boxed_str());
        hub.register_tool(Arc::new(BenchTool { name }));
    }

    c.bench_function("registry/lookup_hit", |b| {
        b.iter(|| {
            black_box(hub.lookup_tool("tool_50"));
        })
    });

    c.bench_function("registry/lookup_miss", |b| {
        b.iter(|| {
            black_box(hub.lookup_tool("nonexistent"));
        })
    });
}

fn bench_registry_schemas(c: &mut Criterion) {
    let mut hub = RegistryHub::new();
    for i in 0..100 {
        let name = Box::leak(format!("tool_{i}").into_boxed_str());
        hub.register_tool(Arc::new(BenchTool { name }));
    }

    c.bench_function("registry/schemas_100", |b| {
        b.iter(|| {
            black_box(hub.tools.schemas());
        })
    });
}

criterion_group!(
    registry_benches,
    bench_registry_register,
    bench_registry_register_many,
    bench_registry_lookup,
    bench_registry_schemas,
);
criterion_main!(registry_benches);

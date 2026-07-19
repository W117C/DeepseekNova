# OpenTelemetry integration for deepseeknova

Provides distributed tracing and metrics via the OpenTelemetry protocol (OTLP).
Traces are automatically collected from existing `tracing` spans and exported
to any OTLP-compatible backend (Jaeger, Honeycomb, Grafana Tempo, etc.).

## Quick start

```rust,no_run
fn main() -> anyhow::Result<()> {
    // Export to an OTLP collector (e.g. local Jaeger, Grafana Agent)
    let _guard = deepseeknova_telemetry::TelemetryGuard::init(
        "deepseeknova",
        Some("http://localhost:4317"),
    )?;

    // Or use stdout for local debugging
    // let _guard = deepseeknova_telemetry::TelemetryGuard::init_stdout("deepseeknova")?;

    // All tracing::info_span! / tracing::debug! calls are automatically
    // bridged to OpenTelemetry spans and exported.
    Ok(())
}
```

## Span conventions

| Span name | Attributes | Description |
|---|---|---|
| `agent.turn` | `agent.input`, `agent.model` | Full agent invocation |
| `agent.think` | `agent.model`, `agent.tokens` | Single LLM API call |
| `tool.execute` | `tool.name`, `tool.call_id` | Tool execution |
| `agent.plan` | `plan.steps`, `plan.model` | Plan generation |
| `agent.compact` | `memory.tokens_before`, `memory.tokens_after` | Memory compaction |

## License

Licensed under the same terms as deepseeknova.

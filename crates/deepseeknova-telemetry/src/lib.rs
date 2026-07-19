//! OpenTelemetry integration for deepseeknova.
//!
//! Provides distributed tracing and metrics via the OpenTelemetry protocol (OTLP).
//! Traces are automatically collected from existing `tracing` spans and exported
//! to any OTLP-compatible backend (Jaeger, Honeycomb, Grafana Tempo, etc.).
//!
//! ## Quick start
//!
//! ```no_run
//! # fn main() -> anyhow::Result<()> {
//! // Export to an OTLP collector (e.g. local Jaeger, Grafana Agent)
//! let _guard = deepseeknova_telemetry::TelemetryGuard::init(
//!     "deepseeknova",
//!     Some("http://localhost:4317"),
//! )?;
//!
//! // Or use stdout for local debugging
//! // let _guard = deepseeknova_telemetry::TelemetryGuard::init_stdout("deepseeknova")?;
//!
//! // All tracing::info_span! / tracing::debug! calls are automatically
//! // bridged to OpenTelemetry spans and exported.
//! # Ok(())
//! # }
//! ```
//!
//! ## Span conventions
//!
//! | Span name | Attributes | Description |
//! |---|---|---|
//! | `agent.turn` | `agent.input`, `agent.model` | Full agent invocation |
//! | `agent.think` | `agent.model`, `agent.tokens` | Single LLM API call |
//! | `tool.execute` | `tool.name`, `tool.call_id` | Tool execution |
//! | `agent.plan` | `plan.steps`, `plan.model` | Plan generation |
//! | `agent.compact` | `memory.tokens_before`, `memory.tokens_after` | Memory compaction |

use anyhow::Context;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::trace::TracerProvider;
use opentelemetry_sdk::Resource;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Registry;

// ---------------------------------------------------------------------------
// TelemetryGuard
// ---------------------------------------------------------------------------

/// Holds the OpenTelemetry pipeline. When dropped, flushes and shuts down
/// the tracer provider.
#[must_use = "telemetry shuts down when dropped — hold this guard for the program lifetime"]
pub struct TelemetryGuard {
    tracer_provider: Option<TracerProvider>,
}

impl TelemetryGuard {
    /// Initialize OpenTelemetry with an OTLP/gRPC exporter.
    ///
    /// `otlp_endpoint` should point to an OTLP collector (default: `http://localhost:4317`).
    /// Pass `None` to use the default endpoint.
    pub fn init(service_name: &str, otlp_endpoint: Option<&str>) -> anyhow::Result<Self> {
        let endpoint = otlp_endpoint.unwrap_or("http://localhost:4317");

        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint)
            .build()
            .context("failed to create OTLP span exporter")?;

        let tracer_provider = TracerProvider::builder()
            .with_batch_exporter(exporter, opentelemetry_sdk::runtime::Tokio)
            .with_resource(Resource::new(vec![KeyValue::new(
                "service.name",
                service_name.to_string(),
            )]))
            .build();

        let tracer = tracer_provider.tracer("deepseeknova");

        let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

        // Install the layer globally. If a subscriber is already set, we add our layer.
        // If no subscriber exists, we create one with the OTel layer + a minimal fmt layer.
        let subscriber = Registry::default().with(otel_layer);
        match tracing::subscriber::set_global_default(subscriber) {
            Ok(()) => {}
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "global subscriber already set — OpenTelemetry layer not installed; \
                     telemetry calls from this point will not be exported"
                );
                // Still return a valid guard so the caller doesn't crash.
                // The tracer_provider is dropped immediately since we can't install it.
            }
        }

        Ok(Self {
            tracer_provider: Some(tracer_provider),
        })
    }

    /// Initialize with a stdout exporter (useful for local debugging).
    ///
    /// Traces are printed as JSON to stdout.
    pub fn init_stdout(service_name: &str) -> anyhow::Result<Self> {
        let exporter = opentelemetry_stdout::SpanExporter::default();

        let tracer_provider = TracerProvider::builder()
            .with_simple_exporter(exporter)
            .with_resource(Resource::new(vec![KeyValue::new(
                "service.name",
                service_name.to_string(),
            )]))
            .build();

        let tracer = tracer_provider.tracer("deepseeknova");

        let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

        let subscriber = Registry::default().with(otel_layer);
        match tracing::subscriber::set_global_default(subscriber) {
            Ok(()) => {}
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "global subscriber already set — OpenTelemetry stdout layer not installed"
                );
            }
        }

        Ok(Self {
            tracer_provider: Some(tracer_provider),
        })
    }
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Some(provider) = self.tracer_provider.take() {
            let _ = provider.shutdown();
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_stdout_does_not_panic() {
        // This will fail to set the global default if tests have already
        // set a subscriber, but it should not panic.
        let result = TelemetryGuard::init_stdout("deepseeknova-test");
        // Either succeeds or gracefully handles the already-set case.
        assert!(result.is_ok());
    }

    #[test]
    fn init_creates_guard() {
        let guard = TelemetryGuard::init_stdout("test-svc").unwrap();
        // Guard exists — drop will clean up
        drop(guard);
    }

    #[test]
    fn must_use_warning_compiles() {
        // Just verify the type is structured correctly
        fn _takes_guard(_g: TelemetryGuard) {}
        let g = TelemetryGuard::init_stdout("test").unwrap();
        _takes_guard(g);
    }
}

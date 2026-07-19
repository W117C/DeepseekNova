use super::schema::OTelExportSchema;
use tracing::{info, span, Level};

pub fn export_telemetry(data: OTelExportSchema) {
    let span = span!(Level::INFO, "deepseeknova.cognitive_state",
        request_id = %data.request_id,
        prefix_epoch = %data.prefix.epoch,
        prefix_hash = %data.prefix.hash,
        prefix_health = %data.prefix.health,
        mutation_type = %data.mutation.mutation_type,
        mem_permanent = %data.memory.permanent,
        mem_dynamic = %data.memory.dynamic,
        tokens_input = %data.tokens.input,
        tokens_budget = %data.tokens.budget
    );
    let _enter = span.enter();

    // In a real OpenTelemetry setup, tracing-opentelemetry will automatically
    // pick this up and export it to Grafana/Jaeger/Tempo.
    info!("Emitted cognitive state telemetry payload.");
}

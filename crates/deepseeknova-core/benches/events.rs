use criterion::{black_box, criterion_group, criterion_main, Criterion};
use deepseeknova_core::chunk::Usage;
use deepseeknova_core::{RunEvent, RunInput, RunOutput};

fn bench_event_clone(c: &mut Criterion) {
    let text_delta = RunEvent::TextDelta("some text delta".to_string());
    let tool_start = RunEvent::ToolCallStart {
        id: "call_abc123".to_string(),
        name: "read_file".to_string(),
    };
    let tool_end = RunEvent::ToolCallEnd {
        id: "call_abc123".to_string(),
        name: "read_file".to_string(),
        arguments: r#"{"path":"src/main.rs"}"#.to_string(),
    };
    let done = RunEvent::Done(RunOutput {
        text: "result text".to_string(),
        tool_calls: vec![],
        usage: Some(Usage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            cache_hit_tokens: 0,
            cache_miss_tokens: 0,
            reasoning_tokens: 0,
        }),
    });

    c.bench_function("event/clone_text_delta", |b| {
        b.iter(|| black_box(text_delta.clone()));
    });

    c.bench_function("event/clone_tool_start", |b| {
        b.iter(|| black_box(tool_start.clone()));
    });

    c.bench_function("event/clone_tool_end", |b| {
        b.iter(|| black_box(tool_end.clone()));
    });

    c.bench_function("event/clone_done", |b| {
        b.iter(|| black_box(done.clone()));
    });
}

fn bench_run_input_clone(c: &mut Criterion) {
    let input = RunInput {
        prompt: "a".repeat(10_000),
        images: vec!["data:image/png;base64,abc123".to_string()],
        model_override: Some("gpt-4o".to_string()),
    };

    c.bench_function("event/clone_run_input_10k", |b| {
        b.iter(|| black_box(input.clone()));
    });
}

/// Benchmark stream construction overhead (no actual I/O).
fn bench_stream_construction(c: &mut Criterion) {
    c.bench_function("event/stream_from_vec", |b| {
        b.iter(|| {
            let events: Vec<anyhow::Result<RunEvent>> = (0..100)
                .map(|i| Ok(RunEvent::TextDelta(format!("chunk_{i} "))))
                .collect();
            let stream = tokio_stream::iter(events);
            let _ = black_box(stream);
        });
    });
}

criterion_group!(
    event_benches,
    bench_event_clone,
    bench_run_input_clone,
    bench_stream_construction,
);
criterion_main!(event_benches);

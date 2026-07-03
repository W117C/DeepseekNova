//! Runner trait cross-implementation consistency tests.
//!
//! Every Runner implementation must pass these behavioral contracts.

use reasonix_agent::Agent;
use reasonix_agent::test_utils::{MockProvider, MockRunner};
use reasonix_core::runner::{RunEvent, RunInput, RunOutput, Runner};
use std::sync::Arc;
use tokio_stream::StreamExt;

// ---------------------------------------------------------------------------
// Runner contract tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mock_runner_streams_events() {
    let runner = MockRunner::text("hello");
    let mut stream = runner
        .run_stream(RunInput {
            prompt: "hi".into(),
            images: vec![],
            model_override: None,
        })
        .await
        .unwrap();

    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev.unwrap());
    }

    assert!(!events.is_empty());
    assert!(events.iter().any(|e| matches!(e, RunEvent::Done(_))));
}

#[tokio::test]
async fn agent_runner_streams_events() {
    let agent = Agent::new(Arc::new(MockProvider::text("hello")), 3);
    let mut stream = agent
        .run_stream(RunInput {
            prompt: "hi".into(),
            images: vec![],
            model_override: None,
        })
        .await
        .unwrap();

    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev.unwrap());
    }

    assert!(events.len() >= 2);
    assert!(events
        .iter()
        .any(|e| matches!(e, RunEvent::TextDelta(_))));
    assert!(events.iter().any(|e| matches!(e, RunEvent::Done(_))));
}

#[tokio::test]
async fn done_event_contains_output() {
    let runner = MockRunner::text("the answer");
    let mut stream = runner
        .run_stream(RunInput {
            prompt: "?".into(),
            images: vec![],
            model_override: None,
        })
        .await
        .unwrap();

    let mut done_output: Option<RunOutput> = None;
    while let Some(ev) = stream.next().await {
        if let Ok(RunEvent::Done(output)) = ev {
            done_output = Some(output);
        }
    }

    assert!(done_output.is_some());
    assert_eq!(done_output.unwrap().text, "the answer");
}

#[tokio::test]
async fn text_delta_event_carries_content() {
    let runner = MockRunner::text("specific text");
    let mut stream = runner
        .run_stream(RunInput {
            prompt: "go".into(),
            images: vec![],
            model_override: None,
        })
        .await
        .unwrap();

    let first = stream.next().await.unwrap().unwrap();
    assert!(matches!(first, RunEvent::TextDelta(ref t) if t == "specific text"));
}

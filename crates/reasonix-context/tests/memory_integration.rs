use reasonix_context::*;
use reasonix_core::types::{Message, Role, ToolSchema};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// WorkingMemory
// ---------------------------------------------------------------------------

#[test]
fn working_memory_add_and_get() {
    let mut wm = WorkingMemory::new();
    let msg = Message {
        role: Role::User,
        content: "hello".into(),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    };
    wm.add_message(msg);
    let all = wm.get_all();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].content, "hello");
}

#[test]
fn working_memory_clear_removes_all() {
    let mut wm = WorkingMemory::new();
    wm.add_message(Message {
        role: Role::User,
        content: "test".into(),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    });
    wm.clear();
    assert!(wm.get_all().is_empty());
}

#[test]
fn working_memory_rewind_trims_tail() {
    let mut wm = WorkingMemory::new();
    wm.add_message(Message {
        role: Role::User,
        content: "first".into(),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    });
    wm.add_message(Message {
        role: Role::Assistant,
        content: "second".into(),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    });
    wm.rewind(1);
    let all = wm.get_all();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].content, "first");
}

#[test]
fn working_memory_pin_is_stored() {
    let mut wm = WorkingMemory::new();
    wm.pin(Message {
        role: Role::System,
        content: "pinned".into(),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    });
    assert_eq!(wm.pinned.len(), 1);
    assert_eq!(wm.pinned[0].content, "pinned");
}

// ---------------------------------------------------------------------------
// ProjectMemory
// ---------------------------------------------------------------------------

#[test]
fn project_memory_loads_reasonix_md() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("REASONIX.md"), "# Project Context").unwrap();

    let mut pm = ProjectMemory::new();
    pm.load_reasonix_md(dir.path());

    assert!(pm.reasonix_md.is_some());
    assert!(pm.reasonix_md.unwrap().contains("Project Context"));
}

#[test]
fn project_memory_missing_reasonix_md_stays_none() {
    let dir = TempDir::new().unwrap();
    let mut pm = ProjectMemory::new();
    pm.load_reasonix_md(dir.path());
    assert!(pm.reasonix_md.is_none());
}

// ---------------------------------------------------------------------------
// PromptBuilder
// ---------------------------------------------------------------------------

#[test]
fn prompt_builder_basic_output() {
    let schemas = vec![ToolSchema {
        name: "read_file".into(),
        description: "Reads a file".into(),
        parameters: serde_json::json!({"type": "object", "properties": {}}),
    }];

    let wm = WorkingMemory::new();
    let pm = ProjectMemory::new();
    let messages = PromptBuilder::build("you are a helpful bot", &schemas, &wm, &pm);

    assert!(!messages.is_empty());
    let system = &messages[0];
    assert_eq!(system.role, Role::System);
    assert!(system.content.contains("you are a helpful bot"));
    assert!(system.content.contains("read_file"));
}

#[test]
fn prompt_builder_injects_project_memory() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("REASONIX.md"), "# My Project\nVersion 2.0").unwrap();
    let mut pm = ProjectMemory::new();
    pm.load_reasonix_md(dir.path());

    let wm = WorkingMemory::new();
    let messages = PromptBuilder::build("you are a bot", &[], &wm, &pm);

    let system = &messages[0];
    assert!(system.content.contains("My Project"));
}

#[test]
fn prompt_builder_includes_conversation_history() {
    let mut wm = WorkingMemory::new();
    wm.add_message(Message {
        role: Role::User,
        content: "user question".into(),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    });
    wm.add_message(Message {
        role: Role::Assistant,
        content: "assistant answer".into(),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    });

    let pm = ProjectMemory::new();
    let messages = PromptBuilder::build("system prompt", &[], &wm, &pm);

    assert!(messages.iter().any(|m| m.content == "user question"));
    assert!(messages.iter().any(|m| m.content == "assistant answer"));
}

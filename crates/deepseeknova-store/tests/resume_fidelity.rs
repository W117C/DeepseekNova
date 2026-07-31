//! 集成：schema v2 会话跨"重启"保真恢复，且恢复历史通过 DeepSeek-V4
//! replay 校验（B2 断点续跑的正确性根基）。

use deepseeknova_context::history::validate_replay_invariant;
use deepseeknova_core::types::{FunctionCall, ToolCall};
use deepseeknova_core::{Message, Role, RunInput};
use deepseeknova_store::SessionStore;

fn tmp_root() -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("dnv-b2-it-{}-{}", std::process::id(), nanos))
}

fn tool_turn_messages() -> Vec<Message> {
    vec![
        Message {
            role: Role::User,
            content: "read src/lib.rs".into(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        },
        Message {
            role: Role::Assistant,
            content: String::new(),
            name: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_9".into(),
                ty: "function".into(),
                function: FunctionCall {
                    name: "read_file".into(),
                    arguments: "{\"path\":\"src/lib.rs\"}".into(),
                },
            }]),
            tool_call_id: None,
            reasoning_content: Some("need the file content first".into()),
        },
        Message {
            role: Role::Tool,
            content: "pub fn x() {}".into(),
            name: None,
            tool_calls: None,
            tool_call_id: Some("call_9".into()),
            reasoning_content: None,
        },
    ]
}

#[test]
fn resumed_session_preserves_tool_fidelity_and_passes_replay_check() {
    let root = tmp_root();
    let sid = "chat-b2-fidelity";
    {
        let store = SessionStore::new(root.clone()).unwrap();
        let input = RunInput {
            prompt: "read src/lib.rs".into(),
            images: vec![],
            model_override: None,
        };
        let turn = SessionStore::build_turn(&input, 1, tool_turn_messages(), None);
        store.append(sid, &turn).unwrap();
    } // drop = 模拟进程退出
    {
        let store = SessionStore::new(root.clone()).unwrap();
        let turns = store.load(sid).unwrap();
        let restored: Vec<Message> = turns
            .iter()
            .flat_map(|t| t.messages.iter().map(Message::from))
            .collect();
        assert_eq!(restored.len(), 3);
        // 保真：assistant 的 tool_calls 与 reasoning 都在。
        let a = &restored[1];
        assert!(a.tool_calls.as_ref().is_some_and(|t| t.len() == 1));
        assert!(a.reasoning_content.is_some());
        // 正确性根基：恢复历史必须通过 replay 校验（tool 结果非孤儿、
        // load-bearing reasoning 未丢）。旧版有损恢复恰恰在这里挂。
        validate_replay_invariant(&restored)
            .expect("restored history must satisfy the V4 replay invariant");
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn legacy_v1_lines_coexist_with_v2_lines_in_one_session() {
    // 同一 turns.jsonl 混合旧版（无新字段）与 v2 行，load 必须全部成功。
    let root = tmp_root();
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("chat-mixed.jsonl");
    // v1 行按旧版序列化输出照抄：空 images / None model_override / None output
    // 均被 skip_serializing_if 省略，message 亦无 tool_calls / reasoning_content。
    let v1 = "{\"turn\":1,\"timestamp\":\"t\",\"input\":{\"prompt\":\"hi\"},\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}";
    std::fs::write(&path, format!("{v1}\n")).unwrap();
    let store = SessionStore::new(root.clone()).unwrap();
    let input = RunInput {
        prompt: "again".into(),
        images: vec![],
        model_override: None,
    };
    let turn = SessionStore::build_turn(&input, 2, tool_turn_messages(), None);
    store.append("chat-mixed", &turn).unwrap();
    let turns = store.load("chat-mixed").unwrap();
    assert_eq!(turns.len(), 2);
    assert!(turns[0].messages[0].tool_calls.is_none()); // v1 行
    assert!(turns[1].messages[1].tool_calls.is_some()); // v2 行
    let _ = std::fs::remove_dir_all(&root);
}

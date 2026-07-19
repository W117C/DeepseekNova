//! 手动验证：真实调用 DeepSeek V4 (Anthropic 兼容端点)，
//! 确认 reasoning block 的真实 JSON 结构，存成 fixture 供解析逻辑比对。
//!
//! 运行：DEEPSEEK_API_KEY=xxx cargo test --test deepseek_reasoning_protocol -- --ignored --nocapture
//! 验证：cargo test --test deepseek_reasoning_protocol  (ignored，不跑；只编译)

#[tokio::test]
#[ignore]
async fn capture_real_reasoning_replay_payload() {
    let api_key =
        std::env::var("DEEPSEEK_API_KEY").expect("set DEEPSEEK_API_KEY to run this capture");
    let client = reqwest::Client::new();
    let base = "https://api.deepseek.com/anthropic/v1/messages";

    let tool_def = serde_json::json!({
        "name": "get_weather",
        "description": "Get current weather for a city",
        "input_schema": {
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"]
        }
    });

    // Turn 1：触发一次工具调用
    let turn1 = serde_json::json!({
        "model": "deepseek-v4-pro",
        "max_tokens": 1024,
        "thinking": { "type": "enabled" },
        "tools": [tool_def],
        "messages": [{ "role": "user", "content": "杭州今天天气怎么样？" }]
    });

    let resp1: serde_json::Value = client
        .post(base)
        .bearer_auth(&api_key)
        .json(&turn1)
        .send()
        .await
        .expect("turn1 请求失败")
        .json()
        .await
        .expect("turn1 响应非 JSON");

    let fixture_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    std::fs::create_dir_all(fixture_dir).ok();
    std::fs::write(
        format!("{fixture_dir}/deepseek_turn1_raw.json"),
        serde_json::to_string_pretty(&resp1).unwrap(),
    )
    .unwrap();

    // 直接原样摘出 content blocks（含 thinking block），
    // 不经过我们自己的 ReasoningBlock 转换——这一步就是要看它原始长什么样
    let content_blocks = resp1["content"]
        .as_array()
        .expect("turn1 响应里没有 content 数组")
        .clone();

    let tool_use_id = content_blocks
        .iter()
        .find(|b| b["type"] == "tool_use")
        .and_then(|b| b["id"].as_str())
        .expect("没找到 tool_use block —— 检查 thinking 配置是否生效")
        .to_string();

    // Turn 2：把原始 assistant content（含 thinking block）
    // 和一条伪造 tool_result 一起发回去，验证协议要求
    let turn2 = serde_json::json!({
        "model": "deepseek-v4-pro",
        "max_tokens": 1024,
        "thinking": { "type": "enabled" },
        "tools": [tool_def],
        "messages": [
            { "role": "user", "content": "杭州今天天气怎么样？" },
            { "role": "assistant", "content": content_blocks },
            { "role": "user", "content": [{
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": "24°C，多云"
            }]}
        ]
    });

    let resp2 = client
        .post(base)
        .bearer_auth(&api_key)
        .json(&turn2)
        .send()
        .await
        .expect("turn2 请求失败");
    let status = resp2.status();
    let body: serde_json::Value = resp2.json().await.expect("turn2 响应非 JSON");

    std::fs::write(
        format!("{fixture_dir}/deepseek_turn2_raw.json"),
        serde_json::to_string_pretty(&body).unwrap(),
    )
    .unwrap();

    assert!(
        status.is_success(),
        "turn2 失败（status={status}）—— 原样回传 content blocks 不够，\
         真实协议比预期更严格，看 fixtures/deepseek_turn2_raw.json 里的错误信息"
    );
}

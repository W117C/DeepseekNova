/// Sub-agents list — currently returns placeholder data.
/// TODO: connect to deepseeknova-orch coordinator for real agent status.
#[tauri::command]
pub async fn list_subagents() -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "mock": true,
        "agents": []
    }))
}

/// Knowledge base — currently returns placeholder data.
/// TODO: connect to core's wiki generator and knowledge card system.
#[tauri::command]
pub async fn get_wiki_pages() -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "mock": true,
        "pages": []
    }))
}

#[tauri::command]
pub async fn get_knowledge_cards() -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "mock": true,
        "cards": []
    }))
}

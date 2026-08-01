use super::*;

#[tauri::command]
pub async fn list_sessions() -> Result<Vec<SessionInfo>, String> {
    Ok(load_sessions())
}

#[tauri::command]
pub async fn create_session(title: Option<String>) -> Result<SessionInfo, String> {
    let mut sessions = load_sessions();
    let id = generate_id();
    let session = SessionInfo {
        id: id.clone(),
        title: title.unwrap_or_else(|| "Untitled".into()),
        message_count: 0,
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs().to_string())
            .unwrap_or_default(),
    };
    sessions.push(session.clone());
    save_sessions(&sessions);
    info!("created session {id}");
    Ok(session)
}

#[tauri::command]
pub async fn delete_session(id: String) -> Result<(), String> {
    let mut sessions = load_sessions();
    sessions.retain(|s| s.id != id);
    save_sessions(&sessions);
    info!("deleted session {id}");
    Ok(())
}

#[tauri::command]
pub async fn rename_session(id: String, title: String) -> Result<(), String> {
    let title = title.trim().to_string();
    if title.is_empty() {
        return Err("title must not be empty".into());
    }
    let mut sessions = load_sessions();
    let Some(session) = sessions.iter_mut().find(|s| s.id == id) else {
        return Err(format!("session not found: {id}"));
    };
    session.title = title;
    save_sessions(&sessions);
    info!("renamed session {id}");
    Ok(())
}

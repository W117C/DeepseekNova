use super::*;

/// Path to the SQLite memory database.
fn memory_db_path() -> std::path::PathBuf {
    let dir = dirs::data_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    dir.join("deepseeknova").join("memory.db")
}

/// Get or create the memory store.
fn get_store() -> Result<deepseeknova_core::memory::store::MemoryStore, String> {
    let path = memory_db_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    deepseeknova_core::memory::store::MemoryStore::open(&path)
        .map_err(|e| format!("failed to open memory store: {e}"))
}

#[tauri::command]
pub async fn get_memories() -> Result<Vec<serde_json::Value>, String> {
    let store = get_store()?;
    let entries = store
        .list_category(deepseeknova_core::memory::store::MemoryCategory::Task)
        .map_err(|e| format!("list error: {e}"))?;
    let user_entries = store
        .list_category(deepseeknova_core::memory::store::MemoryCategory::UserProfile)
        .map_err(|e| format!("list error: {e}"))?;
    let skill_entries = store
        .list_category(deepseeknova_core::memory::store::MemoryCategory::Skill)
        .map_err(|e| format!("list error: {e}"))?;

    let mut results = Vec::new();
    for e in entries
        .iter()
        .chain(user_entries.iter())
        .chain(skill_entries.iter())
    {
        results.push(serde_json::json!({
            "id": e.id,
            "memory_type": match e.category {
                deepseeknova_core::memory::store::MemoryCategory::Task => "task",
                deepseeknova_core::memory::store::MemoryCategory::Skill => "skill",
                deepseeknova_core::memory::store::MemoryCategory::UserProfile => "user",
                deepseeknova_core::memory::store::MemoryCategory::ShortTerm => "short_term",
            },
            "text": e.content,
            "tags": e.tags,
            "source": e.source,
            "created_at": e.created_at,
            "importance": e.importance,
        }));
    }
    Ok(results)
}

#[tauri::command]
pub async fn add_memory(memory_type: String, text: String) -> Result<serde_json::Value, String> {
    let store = get_store()?;
    let category = match memory_type.as_str() {
        "task" | "project" => deepseeknova_core::memory::store::MemoryCategory::Task,
        "skill" => deepseeknova_core::memory::store::MemoryCategory::Skill,
        "user" | "user_profile" => deepseeknova_core::memory::store::MemoryCategory::UserProfile,
        _ => deepseeknova_core::memory::store::MemoryCategory::Task,
    };
    let entry = deepseeknova_core::memory::store::make_entry(
        &text,
        category,
        Vec::new(),
        "desktop-ui",
        0.5,
    );
    store
        .store(&entry)
        .map_err(|e| format!("store error: {e}"))?;
    info!("memory added via SQLite FTS5");
    Ok(serde_json::json!({
        "id": entry.id,
        "memory_type": memory_type,
        "text": entry.content,
        "created_at": entry.created_at,
    }))
}

#[tauri::command]
pub async fn delete_memory(id: String) -> Result<bool, String> {
    let store = get_store()?;
    let deleted = store
        .delete(&id)
        .map_err(|e| format!("delete error: {e}"))?;
    info!("memory {id} deleted");
    Ok(deleted)
}

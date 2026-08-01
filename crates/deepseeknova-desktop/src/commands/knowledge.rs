/// Path to the SQLite memory database (shared with memory.rs).
fn knowledge_db_path() -> std::path::PathBuf {
    let dir = dirs::data_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    dir.join("deepseeknova").join("memory.db")
}

/// Get wiki pages — scans workspace for knowledge/documentation files.
#[tauri::command]
pub async fn get_wiki_pages() -> Result<serde_json::Value, String> {
    let workspace = std::env::current_dir().unwrap_or_default();
    let mut pages: Vec<serde_json::Value> = Vec::new();

    // Scan for markdown knowledge files in common locations
    let scan_dirs = [
        workspace.join("docs"),
        workspace.join(".deepseeknova").join("knowledge"),
        workspace.join(".deepseeknova").join("skills"),
    ];

    for dir in &scan_dirs {
        if !dir.is_dir() {
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path
                    .extension()
                    .map(|e| e == "md" || e == "toml")
                    .unwrap_or(false)
                {
                    let name = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    pages.push(serde_json::json!({
                        "id": path.to_string_lossy(),
                        "title": name,
                        "path": path.to_string_lossy(),
                        "size_bytes": size,
                        "source": dir.file_name()
                            .map(|d| d.to_string_lossy().to_string())
                            .unwrap_or_default(),
                    }));
                }
            }
        }
    }

    Ok(serde_json::json!({
        "mock": false,
        "count": pages.len(),
        "pages": pages,
    }))
}

/// Get knowledge cards from the SQLite FTS5 memory store (Skill category).
#[tauri::command]
pub async fn get_knowledge_cards() -> Result<serde_json::Value, String> {
    let db_path = knowledge_db_path();
    if !db_path.exists() {
        return Ok(serde_json::json!({
            "mock": false,
            "count": 0,
            "cards": [],
            "note": "记忆库尚未创建，使用 Agent 后自动生成",
        }));
    }

    let store = deepseeknova_core::memory::store::MemoryStore::open(&db_path)
        .map_err(|e| format!("failed to open memory store: {e}"))?;

    let skill_entries = store
        .list_category(deepseeknova_core::memory::store::MemoryCategory::Skill)
        .map_err(|e| format!("list error: {e}"))?;

    let task_entries = store
        .list_category(deepseeknova_core::memory::store::MemoryCategory::Task)
        .map_err(|e| format!("list error: {e}"))?;

    let mut cards: Vec<serde_json::Value> = Vec::new();
    for e in skill_entries.iter().chain(task_entries.iter()) {
        cards.push(serde_json::json!({
            "id": e.id,
            "title": if e.content.len() > 60 { &e.content[..60] } else { &e.content },
            "content": e.content,
            "category": match e.category {
                deepseeknova_core::memory::store::MemoryCategory::Skill => "skill",
                deepseeknova_core::memory::store::MemoryCategory::Task => "task",
                deepseeknova_core::memory::store::MemoryCategory::UserProfile => "user",
                deepseeknova_core::memory::store::MemoryCategory::ShortTerm => "short_term",
            },
            "tags": e.tags,
            "importance": e.importance,
            "created_at": e.created_at,
        }));
    }

    Ok(serde_json::json!({
        "mock": false,
        "count": cards.len(),
        "cards": cards,
    }))
}

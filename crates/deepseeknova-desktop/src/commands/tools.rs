use super::*;

// ===========================================================================
// Commands — 工具管理 (Tools)
//
// 列出内置工具并支持启用/禁用开关。开关持久化到 `.deepseeknova/tools.json`；
// 实际生效由 agent 装配层读取该 overrides（后续接线）。
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub enabled: bool,
}

fn tools_config_path() -> std::path::PathBuf {
    let mut p = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    p.push(".deepseeknova");
    p.push("tools.json");
    p
}

/// 读取工具开关 overrides（也供 settings::apply_desktop_overrides 接线使用）
pub fn load_tool_overrides() -> std::collections::HashMap<String, bool> {
    std::fs::read_to_string(tools_config_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_tool_overrides(overrides: &std::collections::HashMap<String, bool>) -> Result<(), String> {
    let path = tools_config_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let data =
        serde_json::to_string_pretty(overrides).map_err(|e| format!("serialize error: {e}"))?;
    std::fs::write(&path, data).map_err(|e| format!("write error: {e}"))
}

/// Built-in tool descriptors (mirrors deepseeknova-tools modules).
const BUILTIN_TOOLS: &[(&str, &str)] = &[
    ("fs", "文件读写"),
    ("glob", "文件通配匹配"),
    ("grep", "代码检索"),
    ("ls", "目录列举"),
    ("shell", "命令执行（经权限网关与沙箱）"),
    ("web_fetch", "网页抓取（受网络开关约束）"),
    ("todo", "任务清单"),
    ("memory", "长期记忆读写"),
    ("snippet", "代码片段提取"),
];

#[tauri::command]
pub async fn list_tools() -> Result<Vec<ToolInfo>, String> {
    let overrides = load_tool_overrides();
    Ok(BUILTIN_TOOLS
        .iter()
        .map(|(name, desc)| ToolInfo {
            name: (*name).into(),
            description: (*desc).into(),
            enabled: overrides.get(*name).copied().unwrap_or(true),
        })
        .collect())
}

#[tauri::command]
pub async fn set_tool_enabled(name: String, enabled: bool) -> Result<(), String> {
    if !BUILTIN_TOOLS.iter().any(|(n, _)| *n == name) {
        return Err(format!("unknown tool: {name}"));
    }
    let mut overrides = load_tool_overrides();
    overrides.insert(name.clone(), enabled);
    save_tool_overrides(&overrides)?;
    info!("tool {name} enabled={enabled}");
    Ok(())
}

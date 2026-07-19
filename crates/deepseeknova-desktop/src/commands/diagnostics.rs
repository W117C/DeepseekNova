/// Diagnostics — currently returns placeholder data.
/// TODO: connect to real system checks (API health, MCP status, disk, memory).
#[tauri::command]
pub async fn run_diagnostics() -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "mock": true,
        "results": [
            {"name": "Node.js 运行时", "status": "pass", "detail": "v22.22.1"},
            {"name": "Tauri 框架", "status": "pass", "detail": "v2.0"},
            {"name": "DeepSeek API 连接", "status": "pending", "detail": "未实际检测"},
            {"name": "API Key 配置", "status": "pending", "detail": "未实际检测"},
            {"name": "MCP: filesystem", "status": "pending", "detail": "未实际检测"},
            {"name": "MCP: git", "status": "pending", "detail": "未实际检测"},
            {"name": "MCP: web-search", "status": "pending", "detail": "未实际检测"},
            {"name": "缓存系统", "status": "pending", "detail": "查看账单页获取真实统计"},
            {"name": "记忆系统", "status": "pass", "detail": "SQLite FTS5 已连接"},
            {"name": "沙箱配置", "status": "pending", "detail": "未实际检测"},
            {"name": "磁盘空间", "status": "pending", "detail": "未实际检测"},
            {"name": "内存使用", "status": "pending", "detail": "未实际检测"},
        ]
    }))
}

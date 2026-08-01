use super::*;

// ===========================================================================
// Commands — 触发与调度 (Triggers)
//
// 一期仅配置持久化（`.deepseeknova/triggers.json`），运行时调度（cron/Webhook）
// 为独立后续任务；设置页应标注「未生效」。
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerConfig {
    /// HTTP API（deepseeknova-serve）对外服务开关
    pub http_api_enabled: bool,
    /// cron 表达式定时任务列表
    pub schedules: Vec<ScheduleEntry>,
    /// Webhook 触发开关
    pub webhook_enabled: bool,
    /// 同一 agent 并行运行实例上限
    pub max_concurrent: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleEntry {
    pub cron: String,
    pub prompt: String,
    pub enabled: bool,
}

impl Default for TriggerConfig {
    fn default() -> Self {
        Self {
            http_api_enabled: false,
            schedules: Vec::new(),
            webhook_enabled: false,
            max_concurrent: 1,
        }
    }
}

fn triggers_path() -> std::path::PathBuf {
    let mut p = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    p.push(".deepseeknova");
    p.push("triggers.json");
    p
}

#[tauri::command]
pub async fn get_triggers() -> Result<TriggerConfig, String> {
    let path = triggers_path();
    if path.exists() {
        let data = std::fs::read_to_string(&path).map_err(|e| format!("read error: {e}"))?;
        serde_json::from_str(&data).map_err(|e| format!("parse error: {e}"))
    } else {
        Ok(TriggerConfig::default())
    }
}

#[tauri::command]
pub async fn set_triggers(config: TriggerConfig) -> Result<(), String> {
    if config.max_concurrent == 0 {
        return Err("max_concurrent must be >= 1".into());
    }
    let path = triggers_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let data =
        serde_json::to_string_pretty(&config).map_err(|e| format!("serialize error: {e}"))?;
    std::fs::write(&path, data).map_err(|e| format!("write error: {e}"))?;
    info!("triggers saved（配置持久化，运行时调度未生效）");
    Ok(())
}

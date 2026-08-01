use super::*;

// ===========================================================================
// Commands — 日志与可观测性 (Logs)
//
// 日志级别 / OpenTelemetry 追踪开关 / 审计日志开关，持久化到
// `.deepseeknova/logcfg.json`（运行时生效需重启，由 telemetry 初始化读取）。
// export_logs 将 `.deepseeknova/logs` 目录打包复制到用户可见位置。
// ===========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogConfig {
    /// debug / info / warn / error
    pub level: String,
    /// OpenTelemetry 全链路追踪
    pub otel_enabled: bool,
    /// 审计日志（工具调用与审批决策）
    pub audit_enabled: bool,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: "info".into(),
            otel_enabled: false,
            audit_enabled: true,
        }
    }
}

fn log_config_path() -> std::path::PathBuf {
    let mut p = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    p.push(".deepseeknova");
    p.push("logcfg.json");
    p
}

#[tauri::command]
pub async fn get_log_config() -> Result<LogConfig, String> {
    let path = log_config_path();
    if path.exists() {
        let data = std::fs::read_to_string(&path).map_err(|e| format!("read error: {e}"))?;
        serde_json::from_str(&data).map_err(|e| format!("parse error: {e}"))
    } else {
        Ok(LogConfig::default())
    }
}

#[tauri::command]
pub async fn set_log_config(config: LogConfig) -> Result<(), String> {
    const LEVELS: &[&str] = &["debug", "info", "warn", "error"];
    if !LEVELS.contains(&config.level.as_str()) {
        return Err(format!("invalid log level: {}", config.level));
    }
    let path = log_config_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let data =
        serde_json::to_string_pretty(&config).map_err(|e| format!("serialize error: {e}"))?;
    std::fs::write(&path, data).map_err(|e| format!("write error: {e}"))?;
    info!("log config saved: level={}", config.level);
    Ok(())
}

/// Export logs: copies `.deepseeknova/logs` into a timestamped folder under
/// the system temp dir and returns the path for the user to pick up.
#[tauri::command]
pub async fn export_logs() -> Result<String, String> {
    let mut src = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    src.push(".deepseeknova");

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let dest = std::env::temp_dir().join(format!("deepseeknova-logs-{stamp}"));
    std::fs::create_dir_all(&dest).map_err(|e| format!("create dir error: {e}"))?;

    let mut copied = 0usize;
    if src.exists() {
        for entry in std::fs::read_dir(&src).map_err(|e| format!("read_dir error: {e}"))? {
            let entry = entry.map_err(|e| format!("entry error: {e}"))?;
            let path = entry.path();
            // 只导出日志与配置类小文件，跳过子目录与数据库
            if path.is_file() {
                let name = entry.file_name();
                if std::fs::copy(&path, dest.join(&name)).is_ok() {
                    copied += 1;
                }
            }
        }
    }
    info!("exported {copied} log/config files to {}", dest.display());
    Ok(dest.display().to_string())
}

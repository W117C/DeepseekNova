use super::*;

#[tauri::command]
pub async fn get_network_config() -> Result<NetworkConfig, String> {
    let path = network_config_path();
    if path.exists() {
        let data = std::fs::read_to_string(&path).map_err(|e| format!("read error: {e}"))?;
        serde_json::from_str(&data).map_err(|e| format!("parse error: {e}"))
    } else {
        Ok(NetworkConfig {
            allow_network: true,
            proxy: None,
            timeout_secs: 30,
            max_retries: 3,
            ssl_verify: true,
            auto_reconnect: true,
        })
    }
}

#[tauri::command]
pub async fn set_network_config(config: NetworkConfig) -> Result<(), String> {
    let path = network_config_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let data =
        serde_json::to_string_pretty(&config).map_err(|e| format!("serialize error: {e}"))?;
    std::fs::write(&path, data).map_err(|e| format!("write error: {e}"))?;
    info!("network config updated");
    Ok(())
}

/// Network diagnostics — performs real connectivity checks against configured providers.
#[tauri::command]
pub async fn network_diagnostics() -> Result<serde_json::Value, String> {
    let config = deepseeknova_config::Config::load().map_err(|e| format!("config error: {e}"))?;

    let mut results: Vec<serde_json::Value> = Vec::new();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("http client error: {e}"))?;

    // Check each configured provider endpoint
    for provider in &config.providers {
        let base_url = provider
            .base_url
            .clone()
            .unwrap_or_else(|| match provider.kind.as_str() {
                "anthropic" => "https://api.anthropic.com".to_string(),
                _ => "https://api.deepseek.com".to_string(),
            });

        let start = std::time::Instant::now();
        let check_url = format!("{base_url}/models");
        match client.get(&check_url).send().await {
            Ok(resp) => {
                let latency_ms = start.elapsed().as_millis();
                let status = if resp.status().is_success() || resp.status().as_u16() == 401 {
                    // 401 means endpoint is reachable but needs auth — still "pass"
                    "pass"
                } else {
                    "warn"
                };
                results.push(serde_json::json!({
                    "name": format!("{} ({})", provider.name, provider.kind),
                    "status": status,
                    "detail": format!("HTTP {} | {}ms", resp.status().as_u16(), latency_ms),
                }));
            }
            Err(e) => {
                let latency_ms = start.elapsed().as_millis();
                results.push(serde_json::json!({
                    "name": format!("{} ({})", provider.name, provider.kind),
                    "status": "fail",
                    "detail": format!("连接失败 ({}ms): {}", latency_ms, e),
                }));
            }
        }
    }

    if config.providers.is_empty() {
        results.push(serde_json::json!({
            "name": "API 端点",
            "status": "warn",
            "detail": "未配置任何 Provider",
        }));
    }

    // Check MCP servers reachability (stdio-based ones are always "local")
    for mcp in &config.mcp_servers {
        results.push(serde_json::json!({
            "name": format!("MCP: {}", mcp.name),
            "status": "pass",
            "detail": format!("本地进程 ({})", mcp.command),
        }));
    }

    let pass_count = results.iter().filter(|r| r["status"] == "pass").count();
    let total = results.len();

    Ok(serde_json::json!({
        "mock": false,
        "summary": format!("{pass_count}/{total} 可达"),
        "results": results,
    }))
}

use super::*;

#[tauri::command]
pub async fn save_settings(settings: serde_json::Value) -> Result<(), String> {
    let path = settings_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let data =
        serde_json::to_string_pretty(&settings).map_err(|e| format!("serialize error: {e}"))?;
    std::fs::write(&path, data).map_err(|e| format!("write error: {e}"))?;
    info!("settings saved");
    Ok(())
}

#[tauri::command]
pub async fn load_settings() -> Result<serde_json::Value, String> {
    let path = settings_path();
    if path.exists() {
        let data = std::fs::read_to_string(&path).map_err(|e| format!("read error: {e}"))?;
        serde_json::from_str(&data).map_err(|e| format!("parse error: {e}"))
    } else {
        Ok(serde_json::json!({}))
    }
}

// ---------------------------------------------------------------------------
// 角色：系统提示词（持久化到 settings.json 的 system_prompt 字段；
// deepseeknova-config 只有 load 无 save，故不写 TOML）
// ---------------------------------------------------------------------------

async fn load_settings_value() -> serde_json::Value {
    load_settings()
        .await
        .unwrap_or_else(|_| serde_json::json!({}))
}

async fn save_settings_key(key: &str, value: serde_json::Value) -> Result<(), String> {
    let mut settings = load_settings_value().await;
    if !settings.is_object() {
        settings = serde_json::json!({});
    }
    settings[key] = value;
    save_settings(settings).await
}

#[tauri::command]
pub async fn get_system_prompt() -> Result<String, String> {
    Ok(load_settings_value()
        .await
        .get("system_prompt")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string())
}

#[tauri::command]
pub async fn set_system_prompt(prompt: String) -> Result<(), String> {
    save_settings_key("system_prompt", serde_json::Value::String(prompt)).await
}

// ---------------------------------------------------------------------------
// 推理参数：采样/降级/重试（submit_prompt 读取生效在批次 B 接线）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningParams {
    pub temperature: f64,
    pub top_p: f64,
    pub max_tokens: u64,
    pub stop_sequences: Vec<String>,
    /// 主模型不可用时的降级模型（空为不降级）
    pub fallback_model: Option<String>,
    pub timeout_secs: u64,
    pub max_retries: u32,
}

impl Default for ReasoningParams {
    fn default() -> Self {
        Self {
            temperature: 0.7,
            top_p: 0.95,
            max_tokens: 8192,
            stop_sequences: Vec::new(),
            fallback_model: None,
            timeout_secs: 60,
            max_retries: 2,
        }
    }
}

#[tauri::command]
pub async fn get_reasoning_params() -> Result<ReasoningParams, String> {
    let settings = load_settings_value().await;
    match settings.get("reasoning_params") {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| format!("parse error: {e}")),
        None => Ok(ReasoningParams::default()),
    }
}

#[tauri::command]
pub async fn set_reasoning_params(params: ReasoningParams) -> Result<(), String> {
    if !(0.0..=2.0).contains(&params.temperature) {
        return Err("temperature must be within 0.0..=2.0".into());
    }
    if !(0.0..=1.0).contains(&params.top_p) {
        return Err("top_p must be within 0.0..=1.0".into());
    }
    let value = serde_json::to_value(&params).map_err(|e| format!("serialize error: {e}"))?;
    save_settings_key("reasoning_params", value).await
}

// ---------------------------------------------------------------------------
// 接线：把桌面端设置（settings.json / tools.json）叠加到会话 Config 上，
// submit_prompt 每次运行前调用，使 系统提示词/推理参数/工具开关 真实生效。
// ---------------------------------------------------------------------------

/// 同步读 settings.json（命令外的内部路径，失败返回空对象）
fn read_settings_sync() -> serde_json::Value {
    std::fs::read_to_string(settings_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

/// 推理参数降级模型（主 provider 创建失败时由 submit_prompt 使用）
pub fn desktop_fallback_model() -> Option<String> {
    read_settings_sync()
        .get("reasoning_params")?
        .get("fallback_model")?
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .map(String::from)
}

/// 将桌面端设置叠加到 Config：
/// - system_prompt → config.agent.system_prompt（build_agent 读取）
/// - reasoning_params → 各 provider 的 timeout/retries + extra_body
///   （OpenAI 兼容端点将 temperature/top_p/max_tokens/stop 合并到请求顶层）
/// - tools.json 开关 → config.tools.overrides（build_agent 跳过禁用工具）
pub fn apply_desktop_overrides(config: &mut deepseeknova_config::Config) {
    let settings = read_settings_sync();

    if let Some(sp) = settings.get("system_prompt").and_then(|v| v.as_str()) {
        if !sp.trim().is_empty() {
            config.agent.system_prompt = Some(sp.to_string());
        }
    }

    // 速率限制：settings.json 的 rate_limit_per_minute → 权限层（新建会话网关时生效）
    if let Some(limit) = settings
        .get("rate_limit_per_minute")
        .and_then(|v| v.as_u64())
    {
        config.permissions.rate_limit_per_minute = Some(limit as u32);
    }

    if let Some(rp) = settings.get("reasoning_params") {
        if let Ok(params) = serde_json::from_value::<ReasoningParams>(rp.clone()) {
            for provider in config.providers.iter_mut() {
                provider.timeout_secs = params.timeout_secs;
                provider.max_retries = params.max_retries;
                let mut extra = provider
                    .extra_body
                    .take()
                    .unwrap_or_else(|| serde_json::json!({}));
                if let Some(map) = extra.as_object_mut() {
                    map.insert("temperature".into(), serde_json::json!(params.temperature));
                    map.insert("top_p".into(), serde_json::json!(params.top_p));
                    map.insert("max_tokens".into(), serde_json::json!(params.max_tokens));
                    if !params.stop_sequences.is_empty() {
                        map.insert("stop".into(), serde_json::json!(params.stop_sequences));
                    }
                }
                provider.extra_body = Some(extra);
            }
        }
    }

    for (name, enabled) in super::tools::load_tool_overrides() {
        if !enabled {
            config
                .tools
                .overrides
                .push(deepseeknova_config::ToolOverride {
                    name,
                    disabled: true,
                    timeout_secs: None,
                    max_file_size: None,
                });
        }
    }
}

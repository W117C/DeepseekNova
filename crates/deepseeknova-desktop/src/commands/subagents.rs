/// List available sub-agent roles and their capabilities from the orchestration layer.
/// Reports the swarm architecture (Queen-led hierarchy) and configured model routing.
#[tauri::command]
pub async fn list_subagents() -> Result<serde_json::Value, String> {
    let config = deepseeknova_config::Config::load().map_err(|e| format!("config error: {e}"))?;

    // Report the available agent roles from the orch system
    let routing = deepseeknova_orch::ModelRouting::default();

    let agents = vec![
        serde_json::json!({
            "id": "queen",
            "name": "协调器 (Queen)",
            "role": "Queen",
            "description": "规划 + 任务分解 + 结果综合",
            "model": routing.planner_model,
            "status": "ready",
        }),
        serde_json::json!({
            "id": "worker-code",
            "name": "代码工作者 (Worker)",
            "role": "Worker",
            "description": "执行代码编写、文件操作等任务",
            "model": routing.worker_model,
            "status": "ready",
        }),
        serde_json::json!({
            "id": "reviewer",
            "name": "审查员 (Reviewer)",
            "role": "Reviewer",
            "description": "验证工作产物、代码审查",
            "model": routing.planner_model,
            "status": "ready",
        }),
        serde_json::json!({
            "id": "researcher",
            "name": "研究员 (Researcher)",
            "role": "Researcher",
            "description": "信息收集、文档搜索、上下文分析",
            "model": routing.worker_model,
            "status": "ready",
        }),
    ];

    let swarm_config = deepseeknova_orch::SwarmConfig::default();

    Ok(serde_json::json!({
        "mock": false,
        "architecture": "Queen-led Swarm (GOAP)",
        "max_workers": swarm_config.max_workers,
        "thinking_enabled": swarm_config.thinking_enabled,
        "reasoning_effort": swarm_config.reasoning_effort,
        "model_routing": {
            "planner": routing.planner_model,
            "worker": routing.worker_model,
            "trivial": routing.trivial_model,
        },
        "agents": agents,
        "provider_count": config.providers.len(),
    }))
}

/// Return the current multi-agent orchestration progress report.
///
/// Backed by the shared `ProgressTracker` in `AppState`; the swarm coordinator
/// records milestones into it. The UI polls this during an active run. When no
/// orchestration has run this reports the idle snapshot.
#[tauri::command]
pub async fn get_orch_progress(
    state: tauri::State<'_, crate::AppState>,
) -> Result<deepseeknova_orch::OrchProgressReport, String> {
    Ok(state.progress.report())
}

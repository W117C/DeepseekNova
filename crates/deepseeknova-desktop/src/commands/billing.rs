use super::*;

#[tauri::command]
pub async fn get_billing_stats(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let usage = state.usage.lock().await;

    let cache_total = usage.cache_hit_tokens + usage.cache_miss_tokens;
    let cache_rate = if cache_total > 0 {
        (usage.cache_hit_tokens as f64 / cache_total as f64) * 100.0
    } else {
        0.0
    };

    // DeepSeek pricing (per 1M tokens, USD)
    // input: $0.27 (full), $0.07 (cached), output: $1.10
    let input_cost = (usage.prompt_tokens as f64 - usage.cache_hit_tokens as f64) * 0.27
        / 1_000_000.0
        + usage.cache_hit_tokens as f64 * 0.07 / 1_000_000.0;
    let output_cost = usage.completion_tokens as f64 * 1.10 / 1_000_000.0;
    let total_cost = input_cost + output_cost;

    Ok(serde_json::json!({
        "session": {
            "prompt_tokens": usage.prompt_tokens,
            "completion_tokens": usage.completion_tokens,
            "total_tokens": usage.total_tokens,
            "cache_hit_tokens": usage.cache_hit_tokens,
            "cache_miss_tokens": usage.cache_miss_tokens,
            "reasoning_tokens": usage.reasoning_tokens,
            "cache_rate": (cache_rate * 10.0).round() / 10.0,
            "run_count": usage.run_count,
        },
        "cost": {
            "input_full": (input_cost * 10000.0).round() / 10000.0,
            "input_cached": (usage.cache_hit_tokens as f64 * 0.07 / 1_000_000.0 * 10000.0).round() / 10000.0,
            "output": (output_cost * 10000.0).round() / 10000.0,
            "total": (total_cost * 10000.0).round() / 10000.0,
        },
        "history": [],
        "mock": false,
    }))
}

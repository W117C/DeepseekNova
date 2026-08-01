/// Run real system diagnostics — checks config, API keys, MCP, memory, sandbox, disk.
#[tauri::command]
pub async fn run_diagnostics() -> Result<serde_json::Value, String> {
    let mut results: Vec<serde_json::Value> = Vec::new();

    // 1. Config loading
    let config = match deepseeknova_config::Config::load() {
        Ok(cfg) => {
            results.push(check("配置文件加载", "pass", "deepseeknova.toml 已加载"));
            Some(cfg)
        }
        Err(e) => {
            results.push(check("配置文件加载", "fail", &format!("{e}")));
            None
        }
    };

    // 2. API Key configuration
    if let Some(ref cfg) = config {
        if cfg.providers.is_empty() {
            results.push(check("API Provider 配置", "fail", "未配置任何 Provider"));
        } else {
            let mut keys_ok = 0;
            let mut keys_missing = 0;
            for p in &cfg.providers {
                let has_key = p.api_key.is_some()
                    || p.api_key_env
                        .as_deref()
                        .map(|env| std::env::var(env).is_ok())
                        .unwrap_or(false);
                if has_key {
                    keys_ok += 1;
                } else {
                    keys_missing += 1;
                }
            }
            if keys_missing == 0 {
                results.push(check(
                    "API Key 配置",
                    "pass",
                    &format!("{keys_ok} 个 Provider 均已配置密钥"),
                ));
            } else {
                results.push(check(
                    "API Key 配置",
                    "warn",
                    &format!("{keys_ok} 个已配置, {keys_missing} 个缺少密钥"),
                ));
            }
        }

        // 3. MCP servers
        if cfg.mcp_servers.is_empty() {
            results.push(check("MCP 服务器", "warn", "未配置 MCP 服务器"));
        } else {
            results.push(check(
                "MCP 服务器",
                "pass",
                &format!("已配置 {} 个 MCP 服务器", cfg.mcp_servers.len()),
            ));
        }

        // 4. Sandbox config
        let sandbox_enabled = cfg.sandbox.enabled;
        results.push(check(
            "沙箱配置",
            "pass",
            if sandbox_enabled {
                "已启用"
            } else {
                "未启用（直接执行）"
            },
        ));
    } else {
        results.push(check("API Key 配置", "skip", "配置加载失败，跳过"));
        results.push(check("MCP 服务器", "skip", "配置加载失败，跳过"));
        results.push(check("沙箱配置", "skip", "配置加载失败，跳过"));
    }

    // 5. Memory store (SQLite FTS5)
    let memory_path = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("deepseeknova")
        .join("memory.db");
    if memory_path.exists() {
        match deepseeknova_core::memory::store::MemoryStore::open(&memory_path) {
            Ok(store) => {
                let count = store
                    .list_category(deepseeknova_core::memory::store::MemoryCategory::Task)
                    .map(|v| v.len())
                    .unwrap_or(0);
                results.push(check(
                    "记忆系统",
                    "pass",
                    &format!("SQLite FTS5 已连接, {count} 条任务记忆"),
                ));
            }
            Err(e) => {
                results.push(check("记忆系统", "fail", &format!("数据库打开失败: {e}")));
            }
        }
    } else {
        results.push(check(
            "记忆系统",
            "warn",
            "数据库尚未创建（首次使用后自动创建）",
        ));
    }

    // 6. Disk space (via statvfs on unix, skip on others)
    #[cfg(unix)]
    {
        let workspace = std::env::current_dir().unwrap_or_default();
        match disk_available_gb(&workspace) {
            Some(gb) => {
                let status = if gb > 1.0 { "pass" } else { "warn" };
                results.push(check("磁盘空间", status, &format!("可用 {gb:.1} GB")));
            }
            None => {
                results.push(check("磁盘空间", "skip", "无法检测"));
            }
        }
    }
    #[cfg(not(unix))]
    {
        results.push(check("磁盘空间", "skip", "仅支持 Unix 系统检测"));
    }

    // 7. Tauri framework version
    results.push(check("Tauri 框架", "pass", &format!("v{}", tauri::VERSION)));

    // 8. Agent kernel version
    results.push(check(
        "Agent 内核",
        "pass",
        &format!("deepseeknova v{}", env!("CARGO_PKG_VERSION")),
    ));

    let pass_count = results.iter().filter(|r| r["status"] == "pass").count();
    let total = results.len();

    Ok(serde_json::json!({
        "mock": false,
        "summary": format!("{pass_count}/{total} 通过"),
        "results": results,
    }))
}

fn check(name: &str, status: &str, detail: &str) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "status": status,
        "detail": detail,
    })
}

/// Get available disk space in GB using libc::statvfs (Unix only).
#[cfg(unix)]
fn disk_available_gb(path: &std::path::Path) -> Option<f64> {
    use std::ffi::CString;
    let c_path = CString::new(path.to_string_lossy().as_ref()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    if ret != 0 {
        return None;
    }
    let avail = stat.f_bavail as u64 * stat.f_bsize as u64;
    Some(avail as f64 / 1_073_741_824.0)
}

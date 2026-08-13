use super::*;

/// 串行化修改进程 cwd 的测试：`std::env::set_current_dir` 是进程级全局
/// 状态，并行测试互相覆盖会导致 restore 时目标目录已被另一测试删除。
static CWD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn config_display_redacts_inline_keys_and_auth_headers() {
    let mut config = deepseeknova_config::Config::default();
    config.providers.push(deepseeknova_config::ProviderConfig {
        name: "deepseek".into(),
        kind: "openai".into(),
        base_url: None,
        model: None,
        context_window: None,
        api_key_env: None,
        api_key: Some("sk-super-secret".into()),
        timeout_secs: 120,
        max_retries: 3,
        headers: vec![
            deepseeknova_config::HeaderEntry {
                name: "Authorization".into(),
                value: "Bearer sk-secret".into(),
            },
            deepseeknova_config::HeaderEntry {
                name: "X-Trace-Id".into(),
                value: "abc-123".into(),
            },
        ],
        thinking_enabled: false,
        reasoning_effort: None,
        extra_body: None,
        cache_control: None,
        cache_ttl: None,
        cache_prompt_key: None,
        cache_exact: None,
    });

    let shown = redact_config_for_display(&config);
    let text = format!("{shown:#?}");
    assert!(!text.contains("sk-super-secret"));
    assert!(!text.contains("sk-secret"));
    assert!(text.contains("[REDACTED]"));
    assert!(text.contains("abc-123"));
    // 原始配置对象不得被修改（只读展示语义）。
    assert_eq!(
        config.providers[0].api_key.as_deref(),
        Some("sk-super-secret")
    );
}

#[test]
fn collect_at_files_skips_noise_dirs_and_caps() {
    let _guard = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let ws = tempfile::tempdir().unwrap();
    let root = ws.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("target")).unwrap();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(root.join(".deepseeknova")).unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main(){}").unwrap();
    std::fs::write(root.join("src/lib.rs"), "fn f(){}").unwrap();
    std::fs::write(root.join("target/x.txt"), "noise").unwrap();
    std::fs::write(root.join(".git/config"), "noise").unwrap();
    std::fs::write(root.join("Cargo.toml"), "[package]").unwrap();

    let old = std::env::current_dir().unwrap();
    std::env::set_current_dir(root).unwrap();
    let files = collect_at_files();
    std::env::set_current_dir(&old).unwrap();

    assert!(files.contains(&"Cargo.toml".to_string()));
    assert!(files.contains(&"src/main.rs".to_string()));
    assert!(files.contains(&"src/lib.rs".to_string()));
    assert!(
        !files
            .iter()
            .any(|f| f.contains("target/") || f.contains(".git/") || f.contains(".deepseeknova/")),
        "噪声目录必须被跳过: {files:?}"
    );
}

#[test]
#[cfg(unix)]
fn collect_at_files_skips_symlink_cycles() {
    let _guard = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // 回归：目录 symlink 指向自身/祖先会形成环，跟随则无限递归挂起
    // 启动。`file_type().is_symlink()` 不跟随链接，必须直接跳过。
    let ws = tempfile::tempdir().unwrap();
    let root = ws.path();
    std::fs::write(root.join("real.txt"), "x").unwrap();
    std::os::unix::fs::symlink(root, root.join("loop")).unwrap();
    std::os::unix::fs::symlink("real.txt", root.join("ln.txt")).unwrap();

    let old = std::env::current_dir().unwrap();
    std::env::set_current_dir(root).unwrap();
    let files = collect_at_files();
    std::env::set_current_dir(&old).unwrap();

    assert!(files.contains(&"real.txt".to_string()));
    assert!(
        !files.iter().any(|f| f.starts_with("loop") || f == "ln.txt"),
        "symlink 一律跳过，不成环也不收录: {files:?}"
    );
}

#[test]
fn sessions_root_honors_session_config() {
    let mut c = deepseeknova_config::Config::default();
    assert!(sessions_root(&c).is_some(), "default = enabled, home path");
    c.session.root = "/tmp/custom-sessions".into();
    assert_eq!(
        sessions_root(&c).unwrap(),
        std::path::PathBuf::from("/tmp/custom-sessions")
    );
    c.session.enabled = false;
    assert!(sessions_root(&c).is_none(), "disabled kills persistence");
}

#[test]
fn review_provider_none_when_disabled() {
    // review.enabled = false → 不构建 review provider（避免无关路径因
    // quick 指针的 API key 缺失而阻断 agent 构建）。
    let config = deepseeknova_config::Config::default();
    let router = deepseeknova_provider::router::ModelRouter::from_config(
        &config,
        std::sync::Arc::new(deepseeknova_provider::cost::CostLedger::new()),
    )
    .unwrap();
    assert!(review_provider_for(&router, &config).unwrap().is_none());
}

#[test]
fn compact_override_prefers_pointer_over_compact_model() {
    // 指针未设 + compact_model 非空 → override 为 compact_model
    let mut c = deepseeknova_config::Config::default();
    c.agent.compact_model = "cheap".into();
    assert_eq!(compact_override_model(&c), Some("cheap"));
    // 指针已设 → 指针胜，无 override
    c.model_pointers.compact = Some("ptr-model".into());
    assert_eq!(compact_override_model(&c), None);
    // 双无 → 无 override
    c.model_pointers.compact = None;
    c.agent.compact_model.clear();
    assert_eq!(compact_override_model(&c), None);
}

#[test]
fn cli_session_label_is_serve_safe() {
    // F11：会话标注必须是 serve 端点 id 白名单可接受的形态
    // （`[A-Za-z0-9_-]`，否则 Paused 透出的 id 无法用于端点）。
    let label = cli_session_label();
    assert!(label.starts_with("session-"), "unexpected label: {label}");
    assert!(
        label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
        "label must only contain [A-Za-z0-9_-] (serve path whitelist): {label}"
    );
}

// ── resolve_scan_root（fail-closed 逃逸检查） ───────────────────────

fn temp_scan_root(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("dpr-cli-scan-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn scan_root_aborts_on_parent_traversal() {
    let root = temp_scan_root("traversal");
    for bad in ["..", "../..", "../../etc/passwd"] {
        let err = resolve_scan_root(&root, std::path::Path::new(bad)).unwrap_err();
        assert!(
            err.to_string().contains("escapes the workspace root"),
            "`{bad}` must fail-closed, got: {err}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn scan_root_aborts_on_absolute_escape() {
    let root = temp_scan_root("abs-escape");
    let err = resolve_scan_root(&root, std::path::Path::new("/etc")).unwrap_err();
    assert!(err.to_string().contains("escapes the workspace root"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn scan_root_allows_inner_path() {
    let root = temp_scan_root("inner");
    std::fs::create_dir_all(root.join("a/b/c")).unwrap();
    let res = resolve_scan_root(&root, std::path::Path::new("a/b/c")).unwrap();
    assert_eq!(res, root.join("a/b/c"));
    let _ = std::fs::remove_dir_all(&root);
}

// symlink 场景依赖 unix 的 symlink()；非 unix 下链接不存在时
// canonicalize 失败走回落分支（返回 Ok），因此整个测试 cfg 门控。
#[cfg(unix)]
#[test]
fn scan_root_aborts_on_symlink_escape() {
    let ws = std::env::temp_dir().join(format!("dnv-symlink-{}", std::process::id()));
    let outside = ws.with_extension("outside"); // 同级外部目录
    std::fs::create_dir_all(outside.join("sub")).unwrap();
    std::fs::write(outside.join("sub/secret.rs"), "let api_key = \"sk-x\";\n").unwrap();
    std::fs::create_dir_all(&ws).unwrap();
    let _ = std::fs::remove_file(ws.join("link"));
    std::os::unix::fs::symlink(&outside, ws.join("link")).unwrap();
    let ws_root = std::path::PathBuf::from(&ws);
    let err = resolve_scan_root(&ws_root, std::path::Path::new("link/sub")).unwrap_err();
    assert!(err.to_string().contains("symlink"));
    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&outside);
}

#[test]
fn severity_min_filter_direction() {
    use deepseeknova_scanner::rule::Severity;
    // "medium" 下限应保留 High 与 Medium，排除 Low。
    let min = Severity::Medium;
    assert!(Severity::High <= min, "High kept under medium floor");
    assert!(Severity::Medium <= min);
    assert!(!(Severity::Low <= min), "Low excluded under medium floor");
}

// ── checkpoint diff 展示（format_diff_entries）──────────────────────────

/// 构造一个 `FileDiff` 测试助手（避免依赖 tokio 文件 I/O）。
fn fake_file_diff(
    path: &str,
    added: usize,
    removed: usize,
    diff_text: &str,
    truncated: bool,
) -> deepseeknova_checkpoint::FileDiff {
    deepseeknova_checkpoint::FileDiff {
        path: std::path::PathBuf::from(path),
        hash: "abcd1234".to_string(),
        added,
        removed,
        diff_text: diff_text.to_string(),
        truncated,
    }
}

#[test]
fn format_diff_entries_deleted_file_shows_all_removed() {
    // 快照存在但文件被删除 → 展示层应全为 `-` 行。
    let entries = vec![Some(fake_file_diff(
        "gone.txt",
        0,
        3,
        "-line1\n-line2\n-line3\n",
        false,
    ))];
    let lines = format_diff_entries(&entries, None);
    assert_eq!(lines[0], "--- gone.txt ---");
    assert_eq!(lines[1], "-line1");
    assert_eq!(lines[2], "-line2");
    assert_eq!(lines[3], "-line3");
    assert_eq!(lines.len(), 4);
}

#[test]
fn format_diff_entries_filter_keeps_only_matching_path() {
    let entries = vec![
        Some(fake_file_diff("a.txt", 1, 0, "+x\n", false)),
        Some(fake_file_diff("b.txt", 0, 2, "-y\n-z\n", false)),
    ];
    let lines = format_diff_entries(&entries, Some("b.txt"));
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0], "--- b.txt ---");
    assert!(lines.iter().all(|l| !l.contains("a.txt")));
}

#[test]
fn format_diff_entries_no_match_reports_empty() {
    let entries = vec![Some(fake_file_diff("a.txt", 1, 0, "+x\n", false))];
    let lines = format_diff_entries(&entries, Some("nope.txt"));
    assert_eq!(lines, vec!["no modified files to diff"]);
}

#[test]
fn format_diff_entries_truncated_shows_counts_only() {
    let entries = vec![Some(fake_file_diff("big.txt", 12, 3, "", true))];
    let lines = format_diff_entries(&entries, None);
    assert_eq!(lines, vec!["big.txt (truncated, +12/-3)"]);
}

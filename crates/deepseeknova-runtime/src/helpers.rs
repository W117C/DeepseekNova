//! 纯工具函数：blocking 工作释放、repo map seeds、压缩阈值推导。
//! M7b 拆分：从 lib.rs 纯搬移，不修改行为/签名。

use deepseeknova_config::Config;

/// H4：在同步闭包（`RecallProvider` / `DistillHook`）内执行潜在阻塞工作
/// （remote embedder 的同步 HTTP 调用，最长 `embed_timeout_secs`）。
///
/// 当前位于 tokio 多线程 runtime 的 worker 线程时用 [`tokio::task::block_in_place`]
/// 把 worker 让回调度器：阻塞发生在当前线程，但该线程不再被 runtime 视为
/// worker，其它任务照常调度（agent 运行不再被 embed 的 block_on 停顿）。
/// 无 runtime 上下文 / current_thread runtime / blocking 池线程时直接调用
/// （不 panic，测试与单线程场景行为与旧版一致）。
/// `block_in_place` 不要求闭包 `Send`，借用 `&str` 等非 `'static` 捕获安全。
pub(crate) fn run_blocking_work<T>(f: impl FnOnce() -> T) -> T {
    let multi_thread = tokio::runtime::Handle::try_current()
        .map(|h| h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
        .unwrap_or(false);
    if multi_thread {
        tokio::task::block_in_place(f)
    } else {
        f()
    }
}

/// A3 从用户输入提取 repo map 个性化 seeds：标识符 token（≥3 字符、
/// 去停用词、去重、上限 8），用于对图节点做 personalized PageRank。
pub(crate) fn repo_map_seeds(query: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "the", "and", "for", "with", "this", "that", "from", "into", "file", "code", "please",
        "help", "need", "want", "make", "fix", "add", "new", "use", "using", "should", "could",
        "would", "about", "your", "you", "our", "are", "not", "but", "can", "has", "have", "how",
        "what", "why", "where", "when", "which", "there", "their", "these", "those", "also",
        "then", "than", "will", "was", "were", "been", "being", "tell", "explain", "write",
        "build", "check", "review", "test", "run", "show", "list",
    ];
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for token in query
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| t.len() >= 3)
    {
        if STOP.contains(&token.to_lowercase().as_str()) {
            continue;
        }
        if seen.insert(token.to_lowercase()) {
            out.push(token.to_string());
        }
        if out.len() >= 8 {
            break;
        }
    }
    out
}

/// 压缩阈值推导：显式配置优先；否则 budget 启用时取 max_total_tokens/2；都没有则 None。
pub(crate) fn derive_compaction_threshold(config: &Config) -> Option<u32> {
    if let Some(explicit) = config.agent.compaction_threshold_tokens {
        return Some(explicit);
    }
    if config.budget.enabled {
        return Some((config.budget.max_total_tokens / 2) as u32);
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn compaction_threshold_derives_from_budget() {
        let mut c = Config::default(); // budget 默认启用、max_total=128000
        assert_eq!(derive_compaction_threshold(&c), Some(64_000));
        c.agent.compaction_threshold_tokens = Some(32_000);
        assert_eq!(derive_compaction_threshold(&c), Some(32_000)); // 显式优先
        c.agent.compaction_threshold_tokens = None;
        c.budget.enabled = false;
        assert_eq!(derive_compaction_threshold(&c), None); // budget 关 → None
    }

    // -----------------------------------------------------------------------
    // H4：同步闭包（RecallProvider / DistillHook）内的阻塞工作不占用 tokio
    // worker（仅 `[memory] embedder="remote"` 时命中，条件性 high）
    // -----------------------------------------------------------------------

    /// H4 回归：`run_blocking_work` 在单一 worker 的多线程 runtime 上必须把
    /// worker 让回调度器。若退化为直接调用，阻塞 sleep 期间该 worker 无法
    /// 调度其它任务，心跳在阻塞窗口 [block_start, block_end] 内饿死。
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn run_blocking_work_releases_worker_for_concurrent_tasks() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Mutex;

        let block_window = Arc::new(Mutex::new(None::<(std::time::Instant, std::time::Instant)>));
        let ticks = Arc::new(Mutex::new(Vec::<std::time::Instant>::new()));
        let stop = Arc::new(AtomicBool::new(false));

        // 心跳：2ms 周期记录 tick 时间戳。
        let (tk, st) = (ticks.clone(), stop.clone());
        let heartbeat = tokio::spawn(async move {
            while !st.load(Ordering::SeqCst) {
                tk.lock().unwrap().push(std::time::Instant::now());
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            }
        });

        // 阻塞任务：记录 [start, end] 阻塞窗口。
        let bw = block_window.clone();
        let blocker = tokio::spawn(async move {
            run_blocking_work(move || {
                let start = std::time::Instant::now();
                // 400ms 阻塞：期间若 worker 未释放，心跳无法推进。
                std::thread::sleep(std::time::Duration::from_millis(400));
                *bw.lock().unwrap() = Some((start, std::time::Instant::now()));
            });
        });

        blocker.await.unwrap();
        let (start, end) = block_window.lock().unwrap().expect("blocker must run");
        stop.store(true, Ordering::SeqCst);
        heartbeat.await.unwrap();

        let all_ticks = ticks.lock().unwrap().clone();
        let in_window = all_ticks.iter().any(|t| t >= &start && t <= &end);
        assert!(
            in_window,
            "阻塞窗口 {start:?}..{end:?} 内无心跳（{} 个 tick 均在外）：worker 被占用",
            all_ticks.len()
        );
    }

    #[test]
    fn repo_map_seeds_extracts_identifiers_and_skips_stopwords() {
        let seeds =
            repo_map_seeds("please fix CheckpointManager and add tests for repo_map wiring");
        assert!(seeds.iter().any(|s| s == "CheckpointManager"));
        assert!(seeds.iter().any(|s| s == "repo_map"));
        assert!(
            !seeds
                .iter()
                .any(|s| matches!(s.as_str(), "please" | "and" | "fix" | "add" | "for")),
            "stopwords must be excluded, got {seeds:?}"
        );

        let many =
            repo_map_seeds("alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu");
        assert!(many.len() <= 8, "seed cap must hold, got {many:?}");
        let deduped = repo_map_seeds("token token token again again");
        assert!(deduped.len() <= 2, "seeds must dedupe, got {deduped:?}");
    }

    /// repo_map_seeds 大小写去重 + 停用词/短 token（<3 字符）过滤。
    #[test]
    fn repo_map_seeds_dedupes_case_insensitively_and_filters_underscores() {
        let seeds = repo_map_seeds("Token token and OK net _x my-code");
        // Token/token 大小写去重；and/code 停用词；OK/my/_x 短 token（<3）。
        assert_eq!(
            seeds,
            vec!["Token".to_string(), "net".to_string()],
            "实得: {seeds:?}"
        );
    }

    /// run_blocking_work 无 tokio runtime 上下文时直接调用闭包（不 panic，
    /// 测试与单线程场景行为与旧版一致）。
    #[test]
    fn run_blocking_work_without_runtime_calls_directly() {
        let value = run_blocking_work(|| 42);
        assert_eq!(value, 42);
    }
}

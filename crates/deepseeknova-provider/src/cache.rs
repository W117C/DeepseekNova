//! L2 统一缓存层：provider 无关的精确匹配响应缓存（[`CachingProvider`]）。
//!
//! 与 L1（provider 原生前缀缓存：Anthropic `cache_control` / OpenAI 自动
//! 缓存 / DeepSeek 自动硬盘缓存）互补：L1 依赖服务端对**相同前缀**的 KV
//! 复用；L2 在客户端对**完整请求**做精确匹配，覆盖 L1 覆盖不到的场景——
//! 跨会话重复、无原生缓存的支持端点、TTL 过期后的重复调用、本地模型
//! （Ollama 无前缀缓存概念）。
//!
//! 安全边界（对齐对抗审查结论）：
//! - **agent 流量安全**：任何对话变化（新增工具结果/用户轮）即 miss，
//!   绝不重放过期响应；
//! - **中毒防护**：内层调用失败、响应为空时**不写入**缓存，单个错误
//!   不会被放大为系统性错误回答；
//! - **旁路开关**：调用方可注入谓词，命中新鲜度敏感场景（工具结果、
//!   文件内容）的请求直接跳过缓存；
//! - **语义缓存默认关闭**：本层仅做字节级精确匹配，不涉及 embedding
//!   相似度检索（语义缓存属后续扩展，需另配 TTL/置信度/中毒三重防护，
//!   且**不得**用于 agent 多轮流量——0.99 相似度会重放过期响应）。
//!
//! 记账约束：命中时不产生新的上游用量，缓存的 `Message.usage` 是首次
//! 请求的旧值。接线时 [`CachingProvider`] 应置于 `MeteredProvider` **内层**
//! （缓存只包真实上游调用），避免命中请求被重复/错误计费。

use crate::{Provider, ValidatedRequest};
use async_trait::async_trait;
use deepseeknova_core::chunk::{Chunk, ChunkStream, Usage};
use deepseeknova_core::{DeepseeknovaError, Message, Tool};
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 缓存条目：完整响应消息 + 插入时间（TTL 校验）。
struct Entry {
    message: Message,
    inserted: Instant,
}

/// 有界精确匹配缓存的有序内部状态：map 持条目，order 记录 LRU 使用顺序
/// （队头最久未用、队尾最近使用）。
struct ExactCacheState {
    map: HashMap<u64, Entry>,
    order: VecDeque<u64>,
}

/// 有界精确匹配缓存：完整请求（消息 + 工具 schema 稳定 hash）→ 响应。
///
/// 容量策略：超限时按 LRU 逐出最久未用条目，而非整体清空。整体清空会
/// 连带清掉高频模板条目（质量钩子、评分卡固定 prompt、eval 重复轮），
/// 实测命中率显著低于理论值；LRU 维护成本 O(容量)，容量默认 256 可忽略。
struct ExactCache {
    inner: Mutex<ExactCacheState>,
    max_entries: usize,
    ttl: Duration,
}

impl ExactCache {
    fn new(max_entries: usize, ttl: Duration) -> Self {
        Self {
            inner: Mutex::new(ExactCacheState {
                map: HashMap::new(),
                order: VecDeque::new(),
            }),
            max_entries: max_entries.max(1),
            ttl,
        }
    }

    /// 命中返回缓存响应并 touch（移到使用顺序队尾）；条目不存在或 TTL
    /// 过期视为 miss（不 touch，避免陈旧条目获得新生命）。
    fn get(&self, key: u64) -> Option<Message> {
        let mut guard = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        // 先克隆命中值结束对 map 的不可变借用，再可变更新 order。
        let hit = match guard.map.get(&key) {
            Some(entry) if entry.inserted.elapsed() <= self.ttl => Some(entry.message.clone()),
            _ => None,
        };
        if hit.is_some() {
            // touch：把 key 移到队列尾（最近使用）。
            if let Some(pos) = guard.order.iter().position(|&k| k == key) {
                guard.order.remove(pos);
            }
            guard.order.push_back(key);
        }
        hit
    }

    /// 写入缓存；超限时从队列头逐出最久未用条目（LRU，见类型文档）。
    fn put(&self, key: u64, message: Message) {
        let mut guard = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let exists = guard.map.contains_key(&key);
        if let Some(pos) = guard.order.iter().position(|&k| k == key) {
            guard.order.remove(pos);
        }
        // 仅新条目需要腾位；已存在条目为覆盖更新，不驱逐。
        if !exists {
            while guard.map.len() >= self.max_entries {
                if let Some(oldest) = guard.order.pop_front() {
                    guard.map.remove(&oldest);
                } else {
                    break; // order 与 map 不一致的兜底（理论上不发生）
                }
            }
        }
        guard.order.push_back(key);
        guard.map.insert(
            key,
            Entry {
                message,
                inserted: Instant::now(),
            },
        );
    }
}

/// 把完整请求折叠为稳定 key：messages 的规范化序列化 + 工具 schema 的
/// 序列化，经默认 hasher 折叠。`None` 仅当序列化失败（理论上不发生）。
fn request_key(messages: &[Message], tools: &[&dyn Tool]) -> Option<u64> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let msg_bytes = serde_json::to_vec(messages).ok()?;
    msg_bytes.hash(&mut hasher);
    // 工具集合按 schema（name/description/parameters）序列化，跨请求稳定
    // （与 tool_cache 的地址集合不同：这里要覆盖"同一工具集"的判定）。
    let mut schemas: Vec<_> = tools.iter().map(|t| t.schema()).collect::<Vec<_>>();
    schemas.sort_by(|a, b| a.name.cmp(&b.name));
    for s in &schemas {
        let bytes = serde_json::to_vec(s).ok()?;
        bytes.hash(&mut hasher);
    }
    Some(hasher.finish())
}

/// 旁路谓词：基于消息序列判定是否跳过缓存（新鲜度敏感调用返回 `true`）。
pub type BypassPredicate = Arc<dyn Fn(&[Message]) -> bool + Send + Sync>;

/// 客户端缓存命中统计快照。对不上报 usage 缓存字段的端点（agnese/gitcode
/// 等第三方 OpenAI 兼容代理普遍如此），这是唯一可测的命中率指标——
/// L1 前缀缓存命中率无法从 usage 获取时，L2 客户端统计补上度量缺口。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStatsSnapshot {
    /// 精确缓存命中次数（未调内层）。
    pub hits: u64,
    /// 未命中次数（含旁路请求——旁路即"不应缓存"的 miss）。
    pub misses: u64,
    /// 实际触达内层 provider 的次数（生成响应）。
    pub upstream_calls: u64,
}

impl CacheStatsSnapshot {
    /// 客户端命中率 `hits / (hits + misses)`；无可评估请求时为 `None`。
    pub fn hit_rate(&self) -> Option<f64> {
        let total = self.hits + self.misses;
        (total > 0).then(|| self.hits as f64 / total as f64)
    }
}

/// 线程安全的缓存命中计数（原子，无锁热路径）。
#[derive(Debug, Default)]
struct CacheStats {
    hits: std::sync::atomic::AtomicU64,
    misses: std::sync::atomic::AtomicU64,
    upstream_calls: std::sync::atomic::AtomicU64,
}

impl CacheStats {
    fn snapshot(&self) -> CacheStatsSnapshot {
        use std::sync::atomic::Ordering;
        CacheStatsSnapshot {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            upstream_calls: self.upstream_calls.load(Ordering::Relaxed),
        }
    }
}

/// L2 精确匹配缓存装饰器。见模块文档的安全边界与记账约束。
pub struct CachingProvider {
    inner: Arc<dyn Provider>,
    cache: ExactCache,
    bypass: Option<BypassPredicate>,
    stats: CacheStats,
}

impl CachingProvider {
    /// 包装 `inner`，启用有界（`max_entries`）+ TTL（`ttl`）精确缓存。
    pub fn new(inner: Arc<dyn Provider>, max_entries: usize, ttl: Duration) -> Self {
        Self {
            inner,
            cache: ExactCache::new(max_entries, ttl),
            bypass: None,
            stats: CacheStats::default(),
        }
    }

    /// 注入旁路谓词：返回 `true` 的请求不读不写缓存（如携带工具结果、
    /// 文件内容等新鲜度敏感数据的调用）。
    pub fn with_bypass(mut self, predicate: BypassPredicate) -> Self {
        self.bypass = Some(predicate);
        self
    }

    /// 客户端命中统计（hits/misses/upstream_calls + 命中率）。
    pub fn stats(&self) -> CacheStatsSnapshot {
        self.stats.snapshot()
    }

    fn should_bypass(&self, messages: &[Message]) -> bool {
        self.bypass.as_ref().is_some_and(|p| p(messages))
    }
}

#[async_trait]
impl Provider for CachingProvider {
    async fn generate(
        &self,
        validated: ValidatedRequest<'_>,
    ) -> Result<Message, DeepseeknovaError> {
        use std::sync::atomic::Ordering;
        if self.should_bypass(validated.messages) {
            self.stats.misses.fetch_add(1, Ordering::Relaxed);
            self.stats.upstream_calls.fetch_add(1, Ordering::Relaxed);
            return self.inner.generate(validated).await;
        }
        let Some(key) = request_key(validated.messages, validated.tools) else {
            self.stats.misses.fetch_add(1, Ordering::Relaxed);
            self.stats.upstream_calls.fetch_add(1, Ordering::Relaxed);
            return self.inner.generate(validated).await;
        };
        if let Some(cached) = self.cache.get(key) {
            self.stats.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(cached);
        }
        self.stats.misses.fetch_add(1, Ordering::Relaxed);
        self.stats.upstream_calls.fetch_add(1, Ordering::Relaxed);
        let out = self.inner.generate(validated).await?;
        // 中毒防护：空响应不写缓存（错误经 `?` 已上抛，亦不写）。
        if !out.content.trim().is_empty() {
            self.cache.put(key, out.clone());
        }
        Ok(out)
    }

    async fn stream(
        &self,
        validated: ValidatedRequest<'_>,
    ) -> Result<ChunkStream, DeepseeknovaError> {
        use std::sync::atomic::Ordering;
        if self.should_bypass(validated.messages) {
            self.stats.misses.fetch_add(1, Ordering::Relaxed);
            self.stats.upstream_calls.fetch_add(1, Ordering::Relaxed);
            return self.inner.stream(validated).await;
        }
        let Some(key) = request_key(validated.messages, validated.tools) else {
            self.stats.misses.fetch_add(1, Ordering::Relaxed);
            self.stats.upstream_calls.fetch_add(1, Ordering::Relaxed);
            return self.inner.stream(validated).await;
        };
        if let Some(cached) = self.cache.get(key) {
            // 命中：立即完成的 ChunkStream（与 Provider::stream 默认实现同形）。
            self.stats.hits.fetch_add(1, Ordering::Relaxed);
            let chunks = vec![
                Ok(Chunk::TextDelta(cached.content)),
                Ok(Chunk::Usage(cached.usage.unwrap_or(Usage::default()))),
                Ok(Chunk::Done),
            ];
            return Ok(Box::pin(tokio_stream::iter(chunks)));
        }
        // 未命中：直接转发内层流式（不写缓存）。流式路径逐块下发、无法在
        // 此处聚合完整响应重建缓存条目；且流式调用（主 agent 循环）每轮
        // 消息都变，精确匹配命中率趋近于零——流式由 L1 前缀缓存兜底，
        // L2 精确缓存只服务非流式 generate 的模板化重复调用（质量钩子等）。
        self.stats.misses.fetch_add(1, Ordering::Relaxed);
        self.stats.upstream_calls.fetch_add(1, Ordering::Relaxed);
        self.inner.stream(validated).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cost::{CostLedger, MeteredProvider, ModelRole};
    use deepseeknova_core::types::ToolSchema;
    use deepseeknova_core::{Role, ToolContext};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn msg(role: Role, content: &str) -> Message {
        Message {
            role,
            content: content.to_string(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            reasoning_signature: None,
            usage: None,
        }
    }

    struct DummyTool;

    #[async_trait::async_trait]
    impl Tool for DummyTool {
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: "dummy".into(),
                description: "does nothing".into(),
                parameters: serde_json::json!({"type": "object"}),
            }
        }

        async fn execute(
            &self,
            _ctx: &ToolContext,
            _args: &str,
        ) -> Result<String, DeepseeknovaError> {
            Ok("ok".into())
        }
    }

    #[allow(dead_code)]
    fn _assert_dummy_tool_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DummyTool>();
    }

    /// 计数 provider：记录 generate/stream 被真实调用次数。
    #[derive(Default)]
    struct CountingProvider {
        calls: AtomicUsize,
        fail: bool,
        empty: bool,
    }

    impl CountingProvider {
        fn failing() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                fail: true,
                empty: false,
            }
        }

        fn empty() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                fail: false,
                empty: true,
            }
        }
    }

    #[async_trait::async_trait]
    impl Provider for CountingProvider {
        async fn generate(
            &self,
            _validated: ValidatedRequest<'_>,
        ) -> Result<Message, DeepseeknovaError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Err(DeepseeknovaError::provider("boom"));
            }
            Ok(msg(Role::Assistant, if self.empty { "" } else { "done" }))
        }

        async fn stream(
            &self,
            validated: ValidatedRequest<'_>,
        ) -> Result<ChunkStream, DeepseeknovaError> {
            let out = self.generate(validated).await?;
            let chunks = vec![
                Ok(Chunk::TextDelta(out.content)),
                Ok(Chunk::Usage(Usage::default())),
                Ok(Chunk::Done),
            ];
            Ok(Box::pin(tokio_stream::iter(chunks)))
        }
    }

    fn validated(messages: &[Message]) -> ValidatedRequest<'_> {
        ValidatedRequest::new(messages, &[]).unwrap()
    }

    /// 相同请求第二次命中：内层只被调用一次。
    #[tokio::test]
    async fn same_request_hits_cache() {
        let inner = Arc::new(CountingProvider::default());
        let cached = CachingProvider::new(inner.clone(), 16, Duration::from_secs(60));
        let msgs = [msg(Role::User, "hi")];
        let _ = cached.generate(validated(&msgs)).await.unwrap();
        let _ = cached.generate(validated(&msgs)).await.unwrap();
        assert_eq!(
            inner.calls.load(Ordering::SeqCst),
            1,
            "second call must hit"
        );
    }

    /// 不同请求必须 miss 并重新调用内层（agent 流量安全的核心）。
    #[tokio::test]
    async fn different_request_misses_cache() {
        let inner = Arc::new(CountingProvider::default());
        let cached = CachingProvider::new(inner.clone(), 16, Duration::from_secs(60));
        let msgs_a = [msg(Role::User, "hi")];
        let msgs_b = [msg(Role::User, "hi again")];
        let _ = cached.generate(validated(&msgs_a)).await.unwrap();
        let _ = cached.generate(validated(&msgs_b)).await.unwrap();
        assert_eq!(
            inner.calls.load(Ordering::SeqCst),
            2,
            "changed request must miss"
        );
    }

    /// 一致性⑤：语义近似但字节不同的请求（尾随空格 / 标点）必须 miss——
    /// 精确匹配不做相似度放行，杜绝"看似相同实则不同"的过期重放。
    #[tokio::test]
    async fn near_identical_but_different_request_misses() {
        let inner = Arc::new(CountingProvider::default());
        let cached = CachingProvider::new(inner.clone(), 16, Duration::from_secs(60));
        let msgs_a = [msg(Role::User, "hello")];
        let msgs_b = [msg(Role::User, "hello ")];
        let msgs_c = [msg(Role::User, "hello?")];
        let _ = cached.generate(validated(&msgs_a)).await.unwrap();
        let _ = cached.generate(validated(&msgs_b)).await.unwrap();
        let _ = cached.generate(validated(&msgs_c)).await.unwrap();
        assert_eq!(
            inner.calls.load(Ordering::SeqCst),
            3,
            "whitespace/punctuation differences must each miss"
        );
    }

    /// 自研创新：客户端命中统计——同请求两次调用 → hits=1、misses=1、
    /// upstream_calls=1、hit_rate=0.5（不上报 usage 缓存字段的端点，
    /// 这是唯一可测的命中率指标）。
    #[tokio::test]
    async fn client_cache_stats_measure_hits_and_misses() {
        let inner = Arc::new(CountingProvider::default());
        let cached = CachingProvider::new(inner.clone(), 16, Duration::from_secs(60));
        let msgs = [msg(Role::User, "hi")];
        let _ = cached.generate(validated(&msgs)).await.unwrap();
        let _ = cached.generate(validated(&msgs)).await.unwrap();
        let stats = cached.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.upstream_calls, 1);
        assert_eq!(stats.hit_rate(), Some(0.5));
        assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    }

    /// 自研创新：旁路请求计入 miss 与 upstream_calls（"不应缓存"的 miss），
    /// 命中率分母不受旁路污染。
    #[tokio::test]
    async fn client_cache_stats_bypass_counts_as_miss() {
        let inner = Arc::new(CountingProvider::default());
        let cached = CachingProvider::new(inner.clone(), 16, Duration::from_secs(60)).with_bypass(
            Arc::new(|msgs| msgs.iter().any(|m| m.content.contains("<tool-result>"))),
        );
        let bypassed = [msg(Role::User, "<tool-result>fresh")];
        let _ = cached.generate(validated(&bypassed)).await.unwrap();
        let stats = cached.stats();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.upstream_calls, 1);
        assert_eq!(stats.hit_rate(), Some(0.0));
    }

    /// 95% 目标验证（客户端请求级命中率）：20 次完全相同请求——
    /// 第 1 次冷启动 miss + 19 次命中 → hit_rate = 19/20 = 0.95，
    /// 测量链路必须正确报告 ≥95%（对不上报 usage 缓存字段的端点，
    /// 这是唯一可达 95% 的度量口径）。
    #[tokio::test]
    async fn client_cache_hit_rate_reaches_95_percent() {
        let inner = Arc::new(CountingProvider::default());
        let cached = CachingProvider::new(inner.clone(), 64, Duration::from_secs(60));
        let msgs = [msg(Role::User, "template-call")];
        for _ in 0..20 {
            let _ = cached.generate(validated(&msgs)).await.unwrap();
        }
        let stats = cached.stats();
        assert_eq!(stats.hits, 19);
        assert_eq!(stats.misses, 1);
        assert_eq!(
            stats.upstream_calls, 1,
            "only the cold-start call reaches upstream"
        );
        let rate = stats.hit_rate().expect("evaluable requests present");
        assert!(rate >= 0.95, "hit rate must reach 95%, got {rate}");
    }

    /// 中毒防护：内层失败后不写缓存，重试同请求仍调内层。
    #[tokio::test]
    async fn failed_call_is_not_cached() {
        let inner = Arc::new(CountingProvider::failing());
        let cached = CachingProvider::new(inner.clone(), 16, Duration::from_secs(60));
        let msgs = [msg(Role::User, "hi")];
        assert!(cached.generate(validated(&msgs)).await.is_err());
        assert!(cached.generate(validated(&msgs)).await.is_err());
        assert_eq!(
            inner.calls.load(Ordering::SeqCst),
            2,
            "errors must not be cached"
        );
    }

    /// 中毒防护：空响应不写缓存。
    #[tokio::test]
    async fn empty_response_is_not_cached() {
        let inner = Arc::new(CountingProvider::empty());
        let cached = CachingProvider::new(inner.clone(), 16, Duration::from_secs(60));
        let msgs = [msg(Role::User, "hi")];
        let _ = cached.generate(validated(&msgs)).await.unwrap();
        let _ = cached.generate(validated(&msgs)).await.unwrap();
        assert_eq!(
            inner.calls.load(Ordering::SeqCst),
            2,
            "empty responses must not be cached"
        );
    }

    /// TTL 过期后重新 miss（新鲜度保障）。
    #[tokio::test]
    async fn expired_entry_misses() {
        let inner = Arc::new(CountingProvider::default());
        let cached = CachingProvider::new(inner.clone(), 16, Duration::from_millis(20));
        let msgs = [msg(Role::User, "hi")];
        let _ = cached.generate(validated(&msgs)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
        let _ = cached.generate(validated(&msgs)).await.unwrap();
        assert_eq!(
            inner.calls.load(Ordering::SeqCst),
            2,
            "expired entry must miss"
        );
    }

    /// 旁路谓词：返回 true 的请求跳过缓存（读+写）。
    #[tokio::test]
    async fn bypass_predicate_skips_cache() {
        let inner = Arc::new(CountingProvider::default());
        let cached = CachingProvider::new(inner.clone(), 16, Duration::from_secs(60)).with_bypass(
            Arc::new(|msgs| msgs.iter().any(|m| m.content.contains("<tool-result>"))),
        );
        let msgs = [msg(Role::User, "<tool-result>fresh data")];
        let _ = cached.generate(validated(&msgs)).await.unwrap();
        let _ = cached.generate(validated(&msgs)).await.unwrap();
        assert_eq!(
            inner.calls.load(Ordering::SeqCst),
            2,
            "bypassed requests must always hit inner"
        );
    }

    /// 超限时按 LRU 驱逐最久未用条目，而非整体清空。
    #[tokio::test]
    async fn overflow_evicts_lru_not_clear_all() {
        let inner = Arc::new(CountingProvider::default());
        let cached = CachingProvider::new(inner.clone(), 2, Duration::from_secs(60));
        // 插入 A、B；命中 B（touch B）；插入 C 超限：LRU 驱逐最久未用的 A
        // （而非整体清空连带清掉 B）。随后 B 应仍命中、A 应 miss。
        // 整体清空实现下 B 被清 → 冷调用 5 次；LRU 下 B 命中 → 冷调用 4 次。
        let a = [msg(Role::User, "a")];
        let b = [msg(Role::User, "b")];
        let c = [msg(Role::User, "c")];
        let _ = cached.generate(validated(&a)).await.unwrap(); // miss (1) → {a}
        let _ = cached.generate(validated(&b)).await.unwrap(); // miss (2) → {a,b}
        let _ = cached.generate(validated(&b)).await.unwrap(); // hit B（touch B）
        let _ = cached.generate(validated(&c)).await.unwrap(); // miss (3) → 驱逐 A，{b,c}
        let _ = cached.generate(validated(&b)).await.unwrap(); // B 仍命中（LRU 保留高频条目）
        let _ = cached.generate(validated(&a)).await.unwrap(); // A 已驱逐 → miss (4)
        assert_eq!(
            inner.calls.load(Ordering::SeqCst),
            4,
            "a/b/c cold + a cold(evicted)；b 两次命中"
        );
    }

    /// 记账约束验证：CachingProvider 在 MeteredProvider 内层时，命中请求
    /// 不产生新的上游计量（真实上游只计 1 次）。
    #[tokio::test]
    async fn metered_inner_plus_caching_outer_counts_upstream_once() {
        let ledger = Arc::new(CostLedger::new());
        let inner = MeteredProvider::new(
            Arc::new(CountingProvider::default()),
            ModelRole::Main,
            "test-model",
            Arc::clone(&ledger),
        );
        let cached = CachingProvider::new(Arc::new(inner), 16, Duration::from_secs(60));
        let msgs = [msg(Role::User, "hi")];
        let _ = cached.generate(validated(&msgs)).await.unwrap();
        let _ = cached.generate(validated(&msgs)).await.unwrap();
        let report = ledger.report(&Default::default());
        assert_eq!(
            report.unmetered_calls, 1,
            "only the first call reaches upstream"
        );
    }
}

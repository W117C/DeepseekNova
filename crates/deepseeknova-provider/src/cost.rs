//! Per-model × per-role token accounting with optional USD estimation.
//!
//! [`MeteredProvider`] is a transparent [`Provider`] decorator: it intercepts
//! `Chunk::Usage` on the stream and records it into a shared [`CostLedger`].
//! Non-streaming `generate` calls carry no usage (Message has no usage field)
//! and are counted as *unmetered*, as are streams that end or drop without a
//! usage chunk.

use crate::{Provider, ValidatedRequest};
use async_trait::async_trait;
use deepseeknova_core::chunk::{Chunk, ChunkStream, Usage};
use deepseeknova_core::Message;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

/// The role a model plays in the agent pipeline (Kode-style pointers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ModelRole {
    /// Primary conversation.
    Main,
    /// Sub-agent / delegation.
    Task,
    /// History compaction (summarize).
    Compact,
    /// Fast utility operations.
    Quick,
}

impl ModelRole {
    /// Stable lowercase label, matching `[model_pointers]` keys.
    pub fn label(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Task => "task",
            Self::Compact => "compact",
            Self::Quick => "quick",
        }
    }

    /// Parse a `[model_pointers]` key / CLI role argument.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "main" => Some(Self::Main),
            "task" => Some(Self::Task),
            "compact" => Some(Self::Compact),
            "quick" => Some(Self::Quick),
            _ => None,
        }
    }
}

/// Accumulated token counts for one (role, model) pair.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageBucket {
    /// Total prompt tokens (including those served from the context cache).
    pub prompt_tokens: u64,
    /// Total completion (output) tokens billed.
    pub completion_tokens: u64,
    /// Tokens spent on the model's internal reasoning.
    pub reasoning_tokens: u64,
    /// Prompt tokens served from the DeepSeek context cache.
    pub cache_hit_tokens: u64,
    /// Prompt tokens that missed the context cache.
    pub cache_miss_tokens: u64,
    /// Calls that reported a usage chunk.
    pub metered_calls: u64,
    /// Calls with no usage information (non-streaming generate, interrupted
    /// or usage-less streams). No dollar estimate is attempted for these.
    pub unmetered_calls: u64,
}

/// Optional USD prices for one model, in $/1M tokens.
#[derive(Debug, Clone, Copy, Default)]
pub struct ModelPrices {
    /// Price per 1M prompt tokens, in USD.
    pub input_per_mtok: Option<f64>,
    /// Price per 1M completion tokens, in USD.
    pub output_per_mtok: Option<f64>,
    /// Falls back to `input_per_mtok` when unset.
    pub cache_hit_per_mtok: Option<f64>,
}

/// Model-name → prices lookup, built from config by the router.
pub type PriceTable = HashMap<String, ModelPrices>;

/// One line of a [`CostReport`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostRow {
    /// Model name this row accounts for.
    pub model: String,
    /// Pipeline role the tokens were attributed to.
    pub role: ModelRole,
    /// Accumulated token counts for this (model, role) pair.
    pub bucket: UsageBucket,
    /// USD estimate; `None` when input or output price is missing.
    pub cost_usd: Option<f64>,
}

/// Aggregated accounting snapshot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CostReport {
    /// One aggregated row per (model, role) pair.
    pub rows: Vec<CostRow>,
    /// Sum over rows that have an estimate; `None` when no row has one.
    pub total_usd: Option<f64>,
    /// Total unmetered calls across all rows.
    pub unmetered_calls: u64,
    /// 前缀缓存命中率：`cache_hit / (cache_hit + cache_miss)`，跨全部行汇总
    /// （0..=1）。没有任何缓存记账（hit+miss 均为 0，如全部 unmetered）时为
    /// `None`。命中率是 token 成本的核心杠杆指标（Claude Code 实践 ≥90%）。
    pub cache_hit_rate: Option<f64>,
}

/// Thread-safe per-(role, model) token ledger shared across the run.
#[derive(Debug, Default)]
pub struct CostLedger {
    buckets: Mutex<HashMap<(ModelRole, String), UsageBucket>>,
}

impl CostLedger {
    /// Create an empty ledger.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a metered usage report for one call.
    pub fn record(&self, role: ModelRole, model: &str, usage: &Usage) {
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        let b = buckets.entry((role, model.to_string())).or_default();
        b.prompt_tokens += u64::from(usage.prompt_tokens);
        b.completion_tokens += u64::from(usage.completion_tokens);
        b.reasoning_tokens += u64::from(usage.reasoning_tokens);
        b.cache_hit_tokens += u64::from(usage.cache_hit_tokens);
        b.cache_miss_tokens += u64::from(usage.cache_miss_tokens);
        b.metered_calls += 1;
    }

    /// Record a call for which no usage information was received.
    pub fn record_unmetered(&self, role: ModelRole, model: &str) {
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        buckets
            .entry((role, model.to_string()))
            .or_default()
            .unmetered_calls += 1;
    }

    /// Snapshot the ledger into a report, estimating USD where prices exist.
    pub fn report(&self, prices: &PriceTable) -> CostReport {
        let buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        let mut rows: Vec<CostRow> = buckets
            .iter()
            .map(|((role, model), bucket)| CostRow {
                model: model.clone(),
                role: *role,
                bucket: bucket.clone(),
                cost_usd: prices.get(model).and_then(|p| row_cost(bucket, p)),
            })
            .collect();
        rows.sort_by(|a, b| (&a.model, a.role.label()).cmp(&(&b.model, b.role.label())));
        let estimates: Vec<f64> = rows.iter().filter_map(|r| r.cost_usd).collect();
        let total_usd = if estimates.is_empty() {
            None
        } else {
            Some(estimates.iter().sum())
        };
        let unmetered_calls = rows.iter().map(|r| r.bucket.unmetered_calls).sum();
        // 前缀缓存命中率：跨全部行的 hit/miss 汇总（hit 只来自 metered 行）。
        let cache_hit: u64 = rows.iter().map(|r| r.bucket.cache_hit_tokens).sum();
        let cache_miss: u64 = rows.iter().map(|r| r.bucket.cache_miss_tokens).sum();
        let cache_hit_rate = (cache_hit + cache_miss > 0)
            .then(|| cache_hit as f64 / (cache_hit + cache_miss) as f64);
        CostReport {
            rows,
            total_usd,
            unmetered_calls,
            cache_hit_rate,
        }
    }

    /// Cumulative USD estimate across the whole ledger, `None` when no metered
    /// row has a full (input + output) price pair. Same total as
    /// [`Self::report`]'s `total_usd` but without allocating a [`CostReport`];
    /// intended for hot-path (step-boundary) spending-cap checks.
    pub fn total_usd(&self, prices: &PriceTable) -> Option<f64> {
        let buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        let mut sum = 0.0f64;
        let mut any = false;
        for ((_, model), bucket) in buckets.iter() {
            if let Some(cost) = prices.get(model).and_then(|p| row_cost(bucket, p)) {
                sum += cost;
                any = true;
            }
        }
        if any {
            Some(sum)
        } else {
            None
        }
    }
}

/// USD estimate for one bucket. Requires input + output prices; cache-hit
/// tokens use the cache price when set, else the input price. Reasoning
/// tokens are already included in `completion_tokens` by providers.
fn row_cost(b: &UsageBucket, p: &ModelPrices) -> Option<f64> {
    let input = p.input_per_mtok?;
    let output = p.output_per_mtok?;
    let cache = p.cache_hit_per_mtok.unwrap_or(input);
    let hit = b.cache_hit_tokens as f64;
    let uncached_prompt = (b.prompt_tokens as f64 - hit).max(0.0);
    Some(
        (uncached_prompt * input + hit * cache + b.completion_tokens as f64 * output) / 1_000_000.0,
    )
}

// ---------------------------------------------------------------------------
// MeteredProvider — transparent accounting decorator
// ---------------------------------------------------------------------------

/// Wraps a [`Provider`] and records usage under a fixed (role, model) into a
/// shared [`CostLedger`]. Streaming calls are metered from `Chunk::Usage`;
/// `generate` calls are counted as unmetered (no usage on `Message`).
pub struct MeteredProvider {
    inner: Arc<dyn Provider>,
    role: ModelRole,
    model: String,
    ledger: Arc<CostLedger>,
}

impl MeteredProvider {
    /// Wrap `inner` so every call it makes is accounted under the given
    /// `(role, model)` into the shared `ledger`.
    pub fn new(
        inner: Arc<dyn Provider>,
        role: ModelRole,
        model: impl Into<String>,
        ledger: Arc<CostLedger>,
    ) -> Self {
        Self {
            inner,
            role,
            model: model.into(),
            ledger,
        }
    }
}

#[async_trait]
impl Provider for MeteredProvider {
    async fn generate(
        &self,
        validated: ValidatedRequest<'_>,
    ) -> Result<Message, deepseeknova_core::DeepseeknovaError> {
        let out = self.inner.generate(validated).await?;
        self.ledger.record_unmetered(self.role, &self.model);
        Ok(out)
    }

    async fn stream(
        &self,
        validated: ValidatedRequest<'_>,
    ) -> Result<ChunkStream, deepseeknova_core::DeepseeknovaError> {
        let inner = self.inner.stream(validated).await?;
        Ok(Box::pin(MeteredStream {
            inner,
            role: self.role,
            model: self.model.clone(),
            ledger: Arc::clone(&self.ledger),
            saw_usage: false,
            closed: false,
        }))
    }
}

/// Stream adapter: forwards chunks, records `Chunk::Usage`, and counts the
/// call as unmetered if the stream ends or is dropped without one.
struct MeteredStream {
    inner: ChunkStream,
    role: ModelRole,
    model: String,
    ledger: Arc<CostLedger>,
    saw_usage: bool,
    closed: bool,
}

impl tokio_stream::Stream for MeteredStream {
    type Item = Result<Chunk, deepseeknova_core::DeepseeknovaError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match this.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(item)) => {
                if let Ok(Chunk::Usage(u)) = &item {
                    this.saw_usage = true;
                    this.ledger.record(this.role, &this.model, u);
                }
                Poll::Ready(Some(item))
            }
            Poll::Ready(None) => {
                if !this.closed {
                    this.closed = true;
                    if !this.saw_usage {
                        this.ledger.record_unmetered(this.role, &this.model);
                    }
                }
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for MeteredStream {
    fn drop(&mut self) {
        // Interrupted mid-stream (e.g. cancellation) without usage → unmetered.
        if !self.closed && !self.saw_usage {
            self.ledger.record_unmetered(self.role, &self.model);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_core::chunk::Usage;

    fn usage(prompt: u32, completion: u32, cache_hit: u32) -> Usage {
        Usage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt + completion,
            cache_hit_tokens: cache_hit,
            cache_miss_tokens: prompt.saturating_sub(cache_hit),
            reasoning_tokens: 0,
        }
    }

    #[test]
    fn ledger_aggregates_per_model_and_role() {
        let ledger = CostLedger::new();
        ledger.record(ModelRole::Main, "big", &usage(100, 50, 0));
        ledger.record(ModelRole::Main, "big", &usage(100, 50, 0));
        ledger.record(ModelRole::Task, "small", &usage(10, 5, 0));
        let report = ledger.report(&PriceTable::new());
        assert_eq!(report.rows.len(), 2);
        let main_row = report
            .rows
            .iter()
            .find(|r| r.role == ModelRole::Main)
            .unwrap();
        assert_eq!(main_row.model, "big");
        assert_eq!(main_row.bucket.prompt_tokens, 200);
        assert_eq!(main_row.bucket.completion_tokens, 100);
        assert_eq!(main_row.bucket.metered_calls, 2);
        // 无单价 → 无美元估算，但不报错
        assert!(main_row.cost_usd.is_none());
        assert!(report.total_usd.is_none());
    }

    #[test]
    fn cost_estimate_with_prices_and_cache_fallback() {
        let ledger = CostLedger::new();
        // 1M prompt（其中 0.5M cache hit）、1M completion
        ledger.record(
            ModelRole::Main,
            "big",
            &usage(1_000_000, 1_000_000, 500_000),
        );
        let mut prices = PriceTable::new();
        prices.insert(
            "big".to_string(),
            ModelPrices {
                input_per_mtok: Some(2.0),
                output_per_mtok: Some(8.0),
                cache_hit_per_mtok: Some(0.2),
            },
        );
        let report = ledger.report(&prices);
        // 0.5M*2.0 + 0.5M*0.2 + 1M*8.0 = 1.0 + 0.1 + 8.0
        let cost = report.rows[0].cost_usd.unwrap();
        assert!((cost - 9.1).abs() < 1e-9, "got {cost}");
        assert!((report.total_usd.unwrap() - 9.1).abs() < 1e-9);

        // cache 单价缺失 → cache hit 按 input 单价计：1M*2.0 + 1M*8.0 = 10.0
        prices.get_mut("big").unwrap().cache_hit_per_mtok = None;
        let cost = ledger.report(&prices).rows[0].cost_usd.unwrap();
        assert!((cost - 10.0).abs() < 1e-9, "got {cost}");
    }

    #[test]
    fn partial_prices_disable_dollar_estimate() {
        let ledger = CostLedger::new();
        ledger.record(ModelRole::Quick, "small", &usage(10, 5, 0));
        let mut prices = PriceTable::new();
        prices.insert(
            "small".to_string(),
            ModelPrices {
                input_per_mtok: Some(1.0),
                output_per_mtok: None, // 缺 output 单价
                cache_hit_per_mtok: None,
            },
        );
        let report = ledger.report(&prices);
        assert!(report.rows[0].cost_usd.is_none());
    }

    #[test]
    fn unmetered_calls_are_counted() {
        let ledger = CostLedger::new();
        ledger.record_unmetered(ModelRole::Compact, "big");
        let report = ledger.report(&PriceTable::new());
        assert_eq!(report.unmetered_calls, 1);
        assert_eq!(report.rows[0].bucket.unmetered_calls, 1);
        assert_eq!(report.rows[0].bucket.metered_calls, 0);
    }

    #[test]
    fn total_usd_matches_report_total_and_none_without_prices() {
        let ledger = CostLedger::new();
        // 无单价 → None（与 report().total_usd 同口径）。
        assert_eq!(ledger.total_usd(&PriceTable::new()), None);

        ledger.record(
            ModelRole::Main,
            "big",
            &usage(1_000_000, 1_000_000, 500_000),
        );
        let mut prices = PriceTable::new();
        prices.insert(
            "big".to_string(),
            ModelPrices {
                input_per_mtok: Some(2.0),
                output_per_mtok: Some(8.0),
                cache_hit_per_mtok: Some(0.2),
            },
        );
        // 0.5M*2.0 + 0.5M*0.2 + 1M*8.0 = 9.1
        assert_eq!(ledger.total_usd(&prices), Some(9.1));
        assert_eq!(ledger.total_usd(&prices), ledger.report(&prices).total_usd);

        // 部分单价 → 该行无估算；全部行无估算时整体 None。
        let mut partial = PriceTable::new();
        partial.insert(
            "big".to_string(),
            ModelPrices {
                input_per_mtok: Some(2.0),
                output_per_mtok: None,
                cache_hit_per_mtok: None,
            },
        );
        assert_eq!(ledger.total_usd(&partial), None);
    }

    #[tokio::test]
    async fn metered_provider_records_stream_usage() {
        use deepseeknova_core::chunk::Chunk;
        use deepseeknova_core::{Message, Role};
        use std::sync::Arc;

        struct FakeProvider {
            with_usage: bool,
        }
        #[async_trait::async_trait]
        impl crate::Provider for FakeProvider {
            async fn generate(
                &self,
                _v: crate::ValidatedRequest<'_>,
            ) -> Result<Message, deepseeknova_core::DeepseeknovaError> {
                Ok(Message {
                    role: Role::Assistant,
                    content: "ok".into(),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                    reasoning_signature: None,
                })
            }
            async fn stream(
                &self,
                _v: crate::ValidatedRequest<'_>,
            ) -> Result<deepseeknova_core::chunk::ChunkStream, deepseeknova_core::DeepseeknovaError>
            {
                let mut chunks = vec![Ok(Chunk::TextDelta("hi".into()))];
                if self.with_usage {
                    chunks.push(Ok(Chunk::Usage(usage(7, 3, 0))));
                }
                chunks.push(Ok(Chunk::Done));
                Ok(Box::pin(tokio_stream::iter(chunks)))
            }
        }

        let msgs = vec![Message {
            role: Role::User,
            content: "hi".into(),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            reasoning_signature: None,
        }];

        // 带 Usage 的流 → 记账
        let ledger = Arc::new(CostLedger::new());
        let p = MeteredProvider::new(
            Arc::new(FakeProvider { with_usage: true }),
            ModelRole::Task,
            "small",
            Arc::clone(&ledger),
        );
        let v = crate::ValidatedRequest::new(&msgs, &[]).unwrap();
        let mut s = p.stream(v).await.unwrap();
        use tokio_stream::StreamExt;
        while s.next().await.is_some() {}
        drop(s);
        let report = ledger.report(&PriceTable::new());
        assert_eq!(report.rows[0].bucket.prompt_tokens, 7);
        assert_eq!(report.rows[0].bucket.metered_calls, 1);
        assert_eq!(report.unmetered_calls, 0);

        // 无 Usage 的流 → 未计量
        let ledger = Arc::new(CostLedger::new());
        let p = MeteredProvider::new(
            Arc::new(FakeProvider { with_usage: false }),
            ModelRole::Task,
            "small",
            Arc::clone(&ledger),
        );
        let v = crate::ValidatedRequest::new(&msgs, &[]).unwrap();
        let mut s = p.stream(v).await.unwrap();
        while s.next().await.is_some() {}
        drop(s);
        assert_eq!(ledger.report(&PriceTable::new()).unmetered_calls, 1);
    }
}

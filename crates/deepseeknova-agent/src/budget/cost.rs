//! Session-level USD spending cap (团队级花费上限, P2-4).
//!
//! [`CostBudget`] wraps the router's shared [`CostLedger`] plus its price table
//! and a USD ceiling. The agent main loop evaluates it at the same step
//! boundary as the token budget: either cap triggering pauses the run.

use deepseeknova_provider::cost::{CostLedger, PriceTable};
use deepseeknova_provider::router::ModelRouter;
use std::sync::Arc;

/// Session-level USD spending cap over the shared [`CostLedger`].
///
/// `max_total_cost_usd` is the ceiling; the run pauses once cumulative spend
/// reaches it (step-boundary semantics, same as the token budget). When no
/// metered model has a full (input + output) price pair, the cumulative spend
/// cannot be estimated and the cap is a **no-op** — fail-open on unknown cost,
/// mirroring how an empty price table disables dollar estimation in the
/// ledger itself.
#[derive(Debug, Clone)]
pub struct CostBudget {
    ledger: Arc<CostLedger>,
    prices: PriceTable,
    max_total_cost_usd: f64,
}

impl CostBudget {
    /// Build from an explicit ledger + price table + ceiling. Prices must come
    /// from the same `[[models]]` the providers were built from (the router's
    /// own ledger/price table are consistent by construction — prefer
    /// [`Self::from_router`]).
    pub fn new(ledger: Arc<CostLedger>, prices: PriceTable, max_total_cost_usd: f64) -> Self {
        assert!(
            max_total_cost_usd.is_finite() && max_total_cost_usd >= 0.0,
            "max_total_cost_usd must be a finite value >= 0, got {max_total_cost_usd}"
        );
        Self {
            ledger,
            prices,
            max_total_cost_usd,
        }
    }

    /// Convenience constructor from a [`ModelRouter`]: shares the router's
    /// ledger and price table, keeping the cap consistent with what the
    /// metered providers actually record.
    pub fn from_router(router: &ModelRouter, max_total_cost_usd: f64) -> Self {
        Self::new(router.ledger(), router.price_table(), max_total_cost_usd)
    }

    /// The configured ceiling in USD.
    pub fn max_total_cost_usd(&self) -> f64 {
        self.max_total_cost_usd
    }

    /// Cumulative USD spend so far; `None` when no metered model has a full
    /// price pair (cap not enforceable).
    pub fn spent_usd(&self) -> Option<f64> {
        self.ledger.total_usd(&self.prices)
    }

    /// `Some((limit, spent))` once cumulative spend reached the cap;
    /// `None` when still under it or cost cannot be estimated.
    pub fn exceeded(&self) -> Option<(f64, f64)> {
        self.spent_usd().and_then(|spent| {
            (spent >= self.max_total_cost_usd).then_some((self.max_total_cost_usd, spent))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_core::chunk::Usage;
    use deepseeknova_provider::cost::{ModelPrices, ModelRole};

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

    fn priced_table() -> PriceTable {
        let mut prices = PriceTable::new();
        prices.insert(
            "big".to_string(),
            ModelPrices {
                input_per_mtok: Some(2.0),
                output_per_mtok: Some(8.0),
                cache_hit_per_mtok: Some(0.2),
            },
        );
        prices
    }

    #[test]
    fn spent_tracks_ledger_and_exceeded_flips_at_cap() {
        let ledger = Arc::new(CostLedger::new());
        let cb = CostBudget::new(Arc::clone(&ledger), priced_table(), 5.0);
        assert_eq!(cb.max_total_cost_usd(), 5.0);
        // 空账本 → 无可估行 → spent None，未超限（与 report().total_usd 同口径）。
        assert_eq!(cb.spent_usd(), None);
        assert_eq!(cb.exceeded(), None);

        // 0.5M prompt(hit 0.25M) + 0.25M completion → 0.25*2 + 0.25*0.2 + 0.25*8 = 2.55
        ledger.record(ModelRole::Main, "big", &usage(500_000, 250_000, 250_000));
        assert!((cb.spent_usd().unwrap() - 2.55).abs() < 1e-9);
        assert_eq!(cb.exceeded(), None);

        // 再记 → 累计 5.1 ≥ 5.0 → 超限。
        ledger.record(ModelRole::Main, "big", &usage(500_000, 250_000, 250_000));
        let (limit, spent) = cb.exceeded().unwrap();
        assert_eq!(limit, 5.0);
        assert!((spent - 5.1).abs() < 1e-9);
    }

    #[test]
    fn exceeded_boundary_is_inclusive() {
        let ledger = Arc::new(CostLedger::new());
        // 1M prompt → 1M*2.0/1M = 2.0，正好打到上限 2.0 → 触发（>= 语义）。
        let cb = CostBudget::new(Arc::clone(&ledger), priced_table(), 2.0);
        ledger.record(ModelRole::Main, "big", &usage(1_000_000, 0, 0));
        let (limit, spent) = cb.exceeded().unwrap();
        assert_eq!(limit, 2.0);
        assert!((spent - 2.0).abs() < 1e-9);
    }

    #[test]
    fn no_price_data_makes_cap_a_noop() {
        let ledger = Arc::new(CostLedger::new());
        let cb = CostBudget::new(Arc::clone(&ledger), PriceTable::new(), 0.01);
        ledger.record(ModelRole::Main, "big", &usage(1_000_000, 0, 0));
        // 无单价 → spent None → exceeded None（fail-open）。
        assert_eq!(cb.spent_usd(), None);
        assert_eq!(cb.exceeded(), None);
    }

    #[test]
    #[should_panic(expected = "max_total_cost_usd")]
    fn new_rejects_negative_or_non_finite_cap() {
        let ledger = Arc::new(CostLedger::new());
        let _ = CostBudget::new(Arc::clone(&ledger), PriceTable::new(), -1.0);
    }
}

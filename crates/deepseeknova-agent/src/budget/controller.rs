#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetDecision {
    Allow,
    CompressHistory,
    Reject(String),
}

pub struct PromptBudgetController {
    pub max_total_tokens: usize,
    pub max_memory_tokens: usize,
}

impl Default for PromptBudgetController {
    fn default() -> Self {
        Self {
            max_total_tokens: 128_000,
            max_memory_tokens: 32_000,
        }
    }
}

impl PromptBudgetController {
    pub fn evaluate_budget(
        &self,
        current_tokens: usize,
        proposed_addition: usize,
        memory_tokens: usize,
    ) -> BudgetDecision {
        // 记忆注入侧独立预算：超过 max_memory_tokens 时触发压缩（压缩链会
        // 驱逐 recall 消息），避免记忆块无限膨胀挤占对话空间。
        if memory_tokens > self.max_memory_tokens {
            return BudgetDecision::CompressHistory;
        }
        if current_tokens + proposed_addition > self.max_total_tokens {
            if current_tokens > (self.max_total_tokens as f64 * 0.8) as usize {
                // We are getting close to the hard limit, initiate compression
                return BudgetDecision::CompressHistory;
            } else {
                return BudgetDecision::Reject(
                    "Proposed addition drastically exceeds context window.".into(),
                );
            }
        }

        BudgetDecision::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_when_under_both_budgets() {
        let c = PromptBudgetController::default();
        assert_eq!(
            c.evaluate_budget(10_000, 2048, 5_000),
            BudgetDecision::Allow
        );
    }

    #[test]
    fn compress_when_memory_tokens_exceed_max() {
        let c = PromptBudgetController::default();
        // memory_tokens 超过 max_memory_tokens（32_000）→ CompressHistory
        assert_eq!(
            c.evaluate_budget(10_000, 2048, 40_000),
            BudgetDecision::CompressHistory
        );
    }

    #[test]
    fn compress_when_total_approaches_limit() {
        let c = PromptBudgetController::default();
        // current + addition > max_total_tokens 且 current > 80% → CompressHistory
        assert_eq!(
            c.evaluate_budget(127_000, 2048, 10_000),
            BudgetDecision::CompressHistory
        );
    }

    #[test]
    fn reject_when_addition_drastically_exceeds() {
        let c = PromptBudgetController::default();
        // current < 80% 但 current + addition 超限 → Reject
        assert!(matches!(
            c.evaluate_budget(50_000, 100_000, 10_000),
            BudgetDecision::Reject(_)
        ));
    }
}

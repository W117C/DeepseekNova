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
    ) -> BudgetDecision {
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

        // Memory specific checks would go here based on how much of `current_tokens` is memory.
        BudgetDecision::Allow
    }
}

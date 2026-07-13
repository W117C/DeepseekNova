use super::state::PrefixState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationType {
    NonSemantic, // Tool order, JSON keys
    Semantic,    // Schema change, system prompt change
    Critical,    // Security policy modified
}

pub enum MutationDecision {
    Allow,
    Recover(PrefixState),
    RequireEpoch,
    Reject(String),
}

pub trait PrefixMutationGuard {
    fn inspect(&self, old: &PrefixState, new: &PrefixState) -> MutationDecision;
}

pub struct StandardMutationGuard;

impl PrefixMutationGuard for StandardMutationGuard {
    fn inspect(&self, old: &PrefixState, new: &PrefixState) -> MutationDecision {
        // Simple heuristic rules for mutation detection
        if old.system_hash != new.system_hash {
            // Assume system hash changes are always Semantic
            return MutationDecision::RequireEpoch;
        }

        if old.tool_registry_hash != new.tool_registry_hash {
            // Further inspection would be needed to classify NonSemantic vs Semantic,
            // e.g., checking if it's just ordering. We'll delegate that to recovery
            // if we return Recover or let the controller handle it.
            // For now, Semantic changes require a new epoch.
            return MutationDecision::RequireEpoch;
        }

        MutationDecision::Allow
    }
}

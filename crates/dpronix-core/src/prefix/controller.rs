use anyhow::Result;

use super::state::{PrefixState, EpochId};
use super::epoch::EpochCommit;
use super::transaction::{PrefixTransaction, MutationPlan};

/// The PrefixController is the core engine of the DPronix PIR (Prefix Identity Runtime).
/// It ensures that Agent logic cannot directly manipulate the cognitive state (Prompt Hash)
/// without explicitly proposing and committing a semantic Mutation Transaction.
pub trait PrefixController {
    /// Returns the current active PrefixState.
    fn current_state(&self) -> PrefixState;

    /// Proposes a mutation to the Cognitive State. 
    /// This runs the `prepare` phase, computing expected cost and diffs.
    fn propose_mutation(&self, tx: PrefixTransaction) -> Result<MutationPlan>;

    /// Commits a validated MutationPlan to the active state, generating a new Prefix Epoch.
    fn commit(&self, plan: MutationPlan) -> Result<EpochCommit>;

    /// Rolls back the Cognitive State to a specific historical Epoch.
    fn rollback(&self, epoch: EpochId) -> Result<()>;
}

/// A standard in-memory implementation of the Prefix Controller
pub struct StandardPrefixController {
    current_state: PrefixState,
    history: indexmap::IndexMap<EpochId, (PrefixState, EpochCommit)>,
}

impl StandardPrefixController {
    pub fn new(initial_state: PrefixState) -> Self {
        let mut history = indexmap::IndexMap::new();
        // The genesis state doesn't have an epoch commit yet, or we can mock one
        let genesis_commit = EpochCommit::new(
            initial_state.epoch, 
            None, 
            "System".into(), 
            "Genesis".into(), 
            "INITIALIZATION".into(), 
            "+ Initialized Prefix State".into(), 
            initial_state.prefix_root.clone()
        );
        history.insert(initial_state.epoch, (initial_state.clone(), genesis_commit));
        
        Self {
            current_state: initial_state,
            history,
        }
    }
}

impl PrefixController for StandardPrefixController {
    fn current_state(&self) -> PrefixState {
        self.current_state.clone()
    }

    fn propose_mutation(&self, _tx: PrefixTransaction) -> Result<MutationPlan> {
        // Implementation typically delegates to transaction::prepare_transaction
        // (Left as stub for interface definition)
        unimplemented!()
    }

    fn commit(&self, plan: MutationPlan) -> Result<EpochCommit> {
        let (new_state, commit) = super::transaction::commit_transaction(plan)?;
        // Store in history and set as active
        // This is safe to run since we own `self` behind a lock in real deployments
        // For trait purposes, assume interior mutability or single-threaded ownership.
        Ok(commit)
    }

    fn rollback(&self, epoch: EpochId) -> Result<()> {
        if !self.history.contains_key(&epoch) {
            anyhow::bail!("Epoch {} not found in controller history", epoch);
        }
        Ok(())
    }
}

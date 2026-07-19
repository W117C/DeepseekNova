use crate::identity::hashing::PromptHash;
use anyhow::{bail, Result};

use super::epoch::EpochCommit;
use super::state::PrefixState;

#[derive(Debug, Clone)]
pub struct PrefixTransaction {
    pub author: String,
    pub reason: String,
    pub impact: String,
    pub creates_new_epoch: bool,
    pub proposed_state: Option<PrefixState>,
}

impl PrefixTransaction {
    pub fn new(author: &str, reason: &str, impact: &str) -> Self {
        Self {
            author: author.to_string(),
            reason: reason.to_string(),
            impact: impact.to_string(),
            creates_new_epoch: true,
            proposed_state: None,
        }
    }
}

pub struct MutationPlan {
    pub transaction: PrefixTransaction,
    pub new_state: PrefixState,
    pub expected_cost: u32,
    pub diff: String,
}

/// Helper to simulate validating a transaction against the current prefix state
pub fn prepare_transaction(
    current: &PrefixState,
    mut tx: PrefixTransaction,
    new_hashes: (PromptHash, PromptHash, PromptHash),
) -> Result<MutationPlan> {
    // Determine the cost dynamically based on what's changing
    let (new_sys, new_tools, new_mem) = new_hashes;

    let mut expected_cost = 0;
    let mut diff = String::new();

    if current.system_hash != new_sys {
        expected_cost += 100;
        diff.push_str("+ System instruction changed\n");
    }

    if current.tool_registry_hash != new_tools {
        expected_cost += 50;
        diff.push_str("+ Tool Registry changed\n");
    }

    if current.memory_hash != new_mem {
        expected_cost += 20;
        diff.push_str("+ Permanent Memory changed\n");
    }

    // Propose new state
    let new_epoch_id = current.epoch + 1;
    let new_state = PrefixState {
        state_id: uuid::Uuid::new_v4().to_string(),
        epoch: new_epoch_id,
        parent_state: Some(current.state_id.clone()),
        schema_version: current.schema_version.clone(),
        system_hash: new_sys,
        tool_registry_hash: new_tools,
        memory_hash: new_mem,
        history_root: current.history_root.clone(),
        prefix_root: PromptHash::default(), // To be re-computed by hasher
        frozen: true,
    };

    tx.proposed_state = Some(new_state.clone());

    Ok(MutationPlan {
        transaction: tx,
        new_state,
        expected_cost,
        diff,
    })
}

pub fn validate_transaction(plan: &MutationPlan) -> Result<()> {
    // In a real system, we'd check if `plan.expected_cost` exceeds the budget,
    // or if the impact is too destructive.
    if plan.expected_cost > 200 {
        bail!("Validation Failed: Mutation cost too high, cache will be severely impacted.");
    }
    Ok(())
}

pub fn commit_transaction(plan: MutationPlan) -> Result<(PrefixState, EpochCommit)> {
    let commit = EpochCommit::new(
        plan.new_state.epoch,
        plan.new_state
            .parent_state
            .clone()
            .map(|_| plan.new_state.epoch - 1), // Simplify parent epoch tracking
        plan.transaction.author.clone(),
        plan.transaction.reason.clone(),
        plan.transaction.impact.clone(),
        plan.diff.clone(),
        plan.new_state.prefix_root.clone(),
    );

    Ok((plan.new_state, commit))
}

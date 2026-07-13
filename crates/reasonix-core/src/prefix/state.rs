use crate::identity::hashing::PromptHash;

pub type PrefixStateId = String;
pub type EpochId = u64;

#[derive(Debug, Clone)]
pub struct PrefixState {
    pub state_id: PrefixStateId,
    pub epoch: EpochId,
    pub parent_state: Option<PrefixStateId>,
    pub schema_version: String,
    pub system_hash: PromptHash,
    pub tool_registry_hash: PromptHash,
    pub memory_hash: PromptHash,
    pub history_root: PromptHash,
    pub prefix_root: PromptHash,
    pub frozen: bool,
}

impl PrefixState {
    pub fn new(
        epoch: EpochId,
        schema_version: String,
        system_hash: PromptHash,
        tool_registry_hash: PromptHash,
        memory_hash: PromptHash,
        history_root: PromptHash,
    ) -> Self {
        let state_id = uuid::Uuid::new_v4().to_string();
        Self {
            state_id,
            epoch,
            parent_state: None,
            schema_version,
            system_hash,
            tool_registry_hash,
            memory_hash,
            history_root,
            prefix_root: PromptHash::default(), // To be computed
            frozen: false,
        }
    }
}

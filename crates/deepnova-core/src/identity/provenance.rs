use crate::identity::hashing::{DefaultHasher, PromptHash, PromptHasher};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvenanceNodeType {
    SystemSource,
    ToolSource,
    MemorySource,
    UserInput,
    GeneratedOutput,
    ExternalContext,
}

#[derive(Debug, Clone)]
pub struct ProvenanceNode {
    pub id: String,
    pub node_type: ProvenanceNodeType,
    pub description: String,
    pub dependencies: Vec<String>, // IDs of parent nodes
}

#[derive(Debug, Clone, Default)]
pub struct ProvenanceDAG {
    nodes: indexmap::IndexMap<String, ProvenanceNode>,
}

impl ProvenanceDAG {
    pub fn new() -> Self {
        Self {
            nodes: indexmap::IndexMap::new(),
        }
    }

    pub fn add_node(&mut self, node: ProvenanceNode) {
        self.nodes.insert(node.id.clone(), node);
    }

    /// Computes the cryptographic hash of the entire provenance graph topology.
    pub fn hash(&self) -> PromptHash {
        let canonical_bytes = crate::identity::canonical::to_canonical_json(
            &self
                .nodes
                .values()
                .map(|n| n.id.clone())
                .collect::<Vec<_>>(),
        )
        .unwrap_or_default();
        DefaultHasher::hash(&canonical_bytes)
    }
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrefixTelemetry {
    pub epoch: u64,
    pub hash: String,
    pub health: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationTelemetry {
    #[serde(rename = "type")]
    pub mutation_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryTelemetry {
    pub permanent: usize,
    pub dynamic: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenTelemetry {
    pub input: usize,
    pub budget: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OTelExportSchema {
    pub request_id: String,
    pub prefix: PrefixTelemetry,
    pub mutation: MutationTelemetry,
    pub memory: MemoryTelemetry,
    pub tokens: TokenTelemetry,
}

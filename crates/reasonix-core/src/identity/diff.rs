#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffSeverity {
    INFO,
    WARNING,
    CRITICAL,
}

#[derive(Debug, Clone)]
pub struct PromptASTDiff {
    pub severity: DiffSeverity,
    pub location: String, // e.g. "Conversation -> Message #8 -> Role"
    pub old_value: String,
    pub new_value: String,
}

impl PromptASTDiff {
    pub fn new(severity: DiffSeverity, location: &str, old: &str, new: &str) -> Self {
        Self {
            severity,
            location: location.to_string(),
            old_value: old.to_string(),
            new_value: new.to_string(),
        }
    }
}

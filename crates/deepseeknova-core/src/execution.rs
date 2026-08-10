//! # Execution ledger contracts
//!
//! **库级 API — 当前未接入生产路径。** `agent` / `runtime` / `serve` 尚未
//! 消费此模块；为后续持久化恢复驱动预留（见 [`ExecutionMode::Authoritative`]）。
//!
//! This module defines durable execution facts independently from live
//! [`crate::RunEvent`] presentation events. Implementations append immutable
//! facts and expose a replayable [`RunProjection`] for recovery and auditing.

use crate::tool_hook::QualityFinding;
use crate::types::Message;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Current schema version for [`ExecutionEventEnvelope`].
pub const EXECUTION_SCHEMA_VERSION: u32 = 1;

/// Controls whether execution facts are disabled, shadow-recorded, or
/// eventually made authoritative for recovery.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    /// Do not create or append execution ledger records.
    #[default]
    Off,
    /// Record durable facts without changing legacy execution or recovery.
    RecordOnly,
    /// Reserve the mode used by a future recovery driver.
    Authoritative,
}

/// Identifies the execution implementation that produced a ledger fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPath {
    /// The primary interactive agent loop.
    Agent,
    /// The graph-based coordinator runner.
    Coordinator,
    /// A named sub-agent runner.
    SubAgent,
    /// A delegated child execution.
    Delegate,
}

/// Outcome of a policy or approval decision recorded before a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionDecision {
    /// The requested action may proceed.
    Allow,
    /// The requested action needs an explicit approval.
    Ask,
    /// The requested action is denied.
    Deny,
}

/// Terminal or recoverable state of one execution run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    /// The run was accepted and may make progress.
    Running,
    /// The run stopped at a resumable boundary.
    Paused,
    /// The run reached its successful terminal state.
    Completed,
    /// The run reached its unsuccessful terminal state.
    Failed,
    /// The run was cancelled before successful completion.
    Cancelled,
}

/// Current state of a requested tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolExecutionStatus {
    /// The model requested the call but it has not passed policy handling.
    Requested,
    /// The call is waiting for a user approval.
    AwaitingApproval,
    /// The call was denied before execution.
    Denied,
    /// The external tool has begun and its outcome is not yet known.
    Started,
    /// The tool returned successfully.
    Completed,
    /// The tool returned an execution error.
    Failed,
    /// The process stopped after tool start and before a terminal fact.
    Indeterminate,
}

/// A pending approval reconstructed from durable execution facts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingApproval {
    /// Stable identifier of the approval request.
    pub approval_id: String,
    /// Tool call that cannot progress before resolution.
    pub call_id: String,
    /// Human-readable approval title.
    pub title: String,
    /// Optional explanation displayed by a frontend.
    pub description: Option<String>,
}

/// Reconstructed lifecycle state for one tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolExecutionProjection {
    /// Tool-call identity within the run.
    pub call_id: String,
    /// Requested tool name.
    pub tool_name: String,
    /// Original private arguments used for replay and local auditing.
    pub arguments: String,
    /// Current lifecycle state.
    pub status: ToolExecutionStatus,
    /// Optional graph node that caused the call.
    pub node_id: Option<String>,
    /// Last terminal result when one exists.
    pub result: Option<String>,
    /// Last failure or denial reason when one exists.
    pub error: Option<String>,
}

/// The next safe action a recovery driver may take for a reconstructed run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryDisposition {
    /// It is safe to issue a new provider request from committed history.
    ResumeProvider,
    /// A recorded approval request must be resolved before progress continues.
    AwaitApproval {
        /// Approval request to present or resolve.
        approval_id: String,
    },
    /// A started tool has no terminal fact and requires explicit resolution.
    RequiresToolResolution {
        /// Tool call whose outcome must not be guessed or re-run automatically.
        call_id: String,
    },
    /// The run is terminal and cannot accept another execution command.
    Terminal {
        /// Final run status.
        status: ExecutionStatus,
    },
}

/// Domain facts that form one immutable execution stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "data")]
pub enum ExecutionEvent {
    /// Begins a new run and establishes its owning session and execution path.
    RunAccepted {
        /// Workspace root recorded for local auditing and projection routing.
        workspace: Option<String>,
    },
    /// Records that a model request may be in flight.
    ModelRequestStarted {
        /// Requested model override, if any.
        model: Option<String>,
    },
    /// Records an abandoned model request that produced no committed message.
    ModelRequestAbandoned {
        /// Explanation for abandoning the request.
        reason: String,
    },
    /// Commits one complete provider-ready message to the replay history.
    AssistantMessageCommitted {
        /// Complete message after streaming has settled.
        message: Message,
    },
    /// Records a requested tool before policy or approval handling.
    ToolRequested {
        /// Model or generated tool-call identifier.
        call_id: String,
        /// Requested tool name.
        tool_name: String,
        /// Private tool arguments used for local replay.
        arguments: String,
        /// Optional Coordinator graph node that caused the call.
        node_id: Option<String>,
    },
    /// Records the policy result for a requested tool.
    PolicyEvaluated {
        /// Tool-call identifier.
        call_id: String,
        /// Permission or hook decision.
        decision: ExecutionDecision,
        /// Human-readable policy explanation.
        reason: String,
    },
    /// Records a durable approval request.
    ApprovalRequested {
        /// Stable approval-request identifier.
        approval_id: String,
        /// Tool-call identifier awaiting approval.
        call_id: String,
        /// Human-readable request title.
        title: String,
        /// Optional longer description.
        description: Option<String>,
    },
    /// Records an explicit approval decision.
    ApprovalResolved {
        /// Approval request being resolved.
        approval_id: String,
        /// Whether the user approved execution.
        approved: bool,
    },
    /// Records the durable boundary immediately before a tool invocation.
    ToolStarted {
        /// Tool-call identifier.
        call_id: String,
    },
    /// Records a successful tool outcome.
    ToolCompleted {
        /// Tool-call identifier.
        call_id: String,
        /// Tool result retained in the private ledger.
        result: String,
    },
    /// Records an unsuccessful tool outcome.
    ToolFailed {
        /// Tool-call identifier.
        call_id: String,
        /// Execution error summary.
        error: String,
    },
    /// Records an explicitly resolved unknown tool outcome.
    ToolIndeterminate {
        /// Tool-call identifier.
        call_id: String,
        /// Reason the tool outcome cannot be safely inferred.
        reason: String,
    },
    /// Records deterministic or model-assisted verification.
    VerificationFinished {
        /// Command or verifier label.
        command: String,
        /// Whether verification passed.
        passed: bool,
        /// Bounded verification summary.
        summary: String,
    },
    /// Records a quality finding without coupling the ledger to a hook source.
    QualityFindingRecorded {
        /// Finding emitted by an execution policy.
        finding: QualityFinding,
    },
    /// Associates a checkpoint with the execution evidence stream.
    CheckpointReferenced {
        /// Stable checkpoint reference.
        checkpoint_id: String,
        /// Checkpoint content hash or other integrity reference.
        digest: Option<String>,
    },
    /// Records a resumable pause.
    RunPaused {
        /// Pause reason exposed through existing presentation adapters.
        reason: String,
    },
    /// Records successful terminal completion.
    RunCompleted,
    /// Records cancellation as a distinct non-success terminal outcome.
    RunCancelled,
    /// Records unsuccessful terminal completion.
    RunFailed {
        /// Failure reason.
        error: String,
    },
    /// Records a legacy completed turn without claiming unavailable lifecycle facts.
    ImportedCompletedTurn {
        /// Legacy source identifier.
        source: String,
    },
}

/// Caller-supplied data for one optimistic ledger append.
#[derive(Debug, Clone)]
pub struct ExecutionAppend {
    /// Session owning the run.
    pub session_id: String,
    /// Run receiving the event.
    pub run_id: String,
    /// Optional user-visible turn identity.
    pub turn_id: Option<String>,
    /// Execution implementation producing this fact.
    pub path: ExecutionPath,
    /// Last committed stream sequence observed by the caller.
    pub expected_sequence: u64,
    /// Optional prior event responsible for this fact.
    pub causation_id: Option<String>,
    /// Typed durable execution fact.
    pub event: ExecutionEvent,
}

impl ExecutionAppend {
    /// Creates an append request with no turn or causation reference.
    pub fn new(
        session_id: impl Into<String>,
        run_id: impl Into<String>,
        path: ExecutionPath,
        expected_sequence: u64,
        event: ExecutionEvent,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            run_id: run_id.into(),
            turn_id: None,
            path,
            expected_sequence,
            causation_id: None,
            event,
        }
    }
}

/// Immutable, versioned record produced by an [`ExecutionLedger`] append.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionEventEnvelope {
    /// Version used to deserialize the envelope and payload.
    pub schema_version: u32,
    /// Globally unique immutable event identifier.
    pub event_id: String,
    /// Owning session identifier.
    pub session_id: String,
    /// Owning execution run identifier.
    pub run_id: String,
    /// Optional user-visible turn identifier.
    pub turn_id: Option<String>,
    /// Strictly increasing sequence inside `run_id`.
    pub sequence: u64,
    /// Wall-clock creation timestamp in RFC 3339 UTC format.
    pub occurred_at: String,
    /// Execution implementation producing this fact.
    pub path: ExecutionPath,
    /// Optional event that directly caused this record.
    pub causation_id: Option<String>,
    /// Typed durable execution fact.
    pub event: ExecutionEvent,
}

impl ExecutionEventEnvelope {
    /// Builds an envelope after the ledger has assigned `sequence`.
    pub fn from_append(append: ExecutionAppend, sequence: u64) -> Self {
        Self {
            schema_version: EXECUTION_SCHEMA_VERSION,
            event_id: uuid::Uuid::new_v4().to_string(),
            session_id: append.session_id,
            run_id: append.run_id,
            turn_id: append.turn_id,
            sequence,
            occurred_at: Utc::now().to_rfc3339(),
            path: append.path,
            causation_id: append.causation_id,
            event: append.event,
        }
    }
}

/// Current replay and recovery state derived only from one event stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunProjection {
    /// Owning session identifier.
    pub session_id: String,
    /// Owning execution run identifier.
    pub run_id: String,
    /// Execution implementation recorded at run creation.
    pub path: ExecutionPath,
    /// Workspace root from the run-accepted event.
    pub workspace: Option<String>,
    /// Last accepted stream sequence.
    pub sequence: u64,
    /// Current run status.
    pub status: ExecutionStatus,
    /// Provider-ready history reconstructed from committed messages.
    pub messages: Vec<Message>,
    /// Currently pending approval, if any.
    pub pending_approval: Option<PendingApproval>,
    /// Tool lifecycle state keyed by tool-call identifier.
    pub tools: BTreeMap<String, ToolExecutionProjection>,
    /// Last committed event identifier.
    pub last_event_id: Option<String>,
}

impl RunProjection {
    /// Creates the initial running projection from its first accepted event.
    pub fn new(envelope: &ExecutionEventEnvelope) -> Result<Self, LedgerAppendError> {
        let ExecutionEvent::RunAccepted { workspace } = &envelope.event else {
            return Err(LedgerAppendError::InvalidTransition(
                "the first event must be run_accepted".to_string(),
            ));
        };
        if envelope.sequence != 1 {
            return Err(LedgerAppendError::InvalidSequence {
                expected: 1,
                actual: envelope.sequence,
            });
        }
        Ok(Self {
            session_id: envelope.session_id.clone(),
            run_id: envelope.run_id.clone(),
            path: envelope.path,
            workspace: workspace.clone(),
            sequence: envelope.sequence,
            status: ExecutionStatus::Running,
            messages: Vec::new(),
            pending_approval: None,
            tools: BTreeMap::new(),
            last_event_id: Some(envelope.event_id.clone()),
        })
    }

    /// Applies one ordered event while enforcing lifecycle and terminal-state invariants.
    pub fn apply(&mut self, envelope: &ExecutionEventEnvelope) -> Result<(), LedgerAppendError> {
        if envelope.schema_version != EXECUTION_SCHEMA_VERSION {
            return Err(LedgerAppendError::UnsupportedSchema {
                actual: envelope.schema_version,
            });
        }
        if envelope.session_id != self.session_id || envelope.run_id != self.run_id {
            return Err(LedgerAppendError::InvalidTransition(
                "event belongs to a different execution stream".to_string(),
            ));
        }
        let expected = self.sequence.saturating_add(1);
        if envelope.sequence != expected {
            return Err(LedgerAppendError::InvalidSequence {
                expected,
                actual: envelope.sequence,
            });
        }
        if matches!(
            self.status,
            ExecutionStatus::Completed | ExecutionStatus::Failed | ExecutionStatus::Cancelled
        ) {
            return Err(LedgerAppendError::TerminalRun {
                run_id: self.run_id.clone(),
            });
        }

        match &envelope.event {
            ExecutionEvent::RunAccepted { .. } => {
                return Err(LedgerAppendError::InvalidTransition(
                    "run_accepted may only appear once".to_string(),
                ));
            }
            ExecutionEvent::AssistantMessageCommitted { message } => {
                self.messages.push(message.clone());
            }
            ExecutionEvent::ToolRequested {
                call_id,
                tool_name,
                arguments,
                node_id,
            } => {
                if self.tools.contains_key(call_id) {
                    return Err(LedgerAppendError::InvalidTransition(format!(
                        "tool call '{call_id}' was already requested"
                    )));
                }
                self.tools.insert(
                    call_id.clone(),
                    ToolExecutionProjection {
                        call_id: call_id.clone(),
                        tool_name: tool_name.clone(),
                        arguments: arguments.clone(),
                        status: ToolExecutionStatus::Requested,
                        node_id: node_id.clone(),
                        result: None,
                        error: None,
                    },
                );
            }
            ExecutionEvent::PolicyEvaluated {
                call_id,
                decision,
                reason,
            } => {
                let tool = self.tool_mut(call_id)?;
                if tool.status != ToolExecutionStatus::Requested {
                    return Err(LedgerAppendError::InvalidTransition(format!(
                        "policy for tool call '{call_id}' is not pending"
                    )));
                }
                match decision {
                    ExecutionDecision::Allow => {}
                    ExecutionDecision::Ask => tool.status = ToolExecutionStatus::AwaitingApproval,
                    ExecutionDecision::Deny => {
                        tool.status = ToolExecutionStatus::Denied;
                        tool.error = Some(reason.clone());
                    }
                }
            }
            ExecutionEvent::ApprovalRequested {
                approval_id,
                call_id,
                title,
                description,
            } => {
                if self.pending_approval.is_some() {
                    return Err(LedgerAppendError::InvalidTransition(
                        "only one approval may be pending per run".to_string(),
                    ));
                }
                let tool = self.tool_mut(call_id)?;
                if tool.status != ToolExecutionStatus::AwaitingApproval {
                    return Err(LedgerAppendError::InvalidTransition(format!(
                        "approval for tool call '{call_id}' has no ask decision"
                    )));
                }
                self.pending_approval = Some(PendingApproval {
                    approval_id: approval_id.clone(),
                    call_id: call_id.clone(),
                    title: title.clone(),
                    description: description.clone(),
                });
            }
            ExecutionEvent::ApprovalResolved {
                approval_id,
                approved,
            } => {
                let Some(pending) = self.pending_approval.take() else {
                    return Err(LedgerAppendError::InvalidTransition(
                        "approval resolution has no pending approval".to_string(),
                    ));
                };
                if &pending.approval_id != approval_id {
                    return Err(LedgerAppendError::InvalidTransition(
                        "approval resolution does not match the pending request".to_string(),
                    ));
                }
                let tool = self.tool_mut(&pending.call_id)?;
                if *approved {
                    tool.status = ToolExecutionStatus::Requested;
                } else {
                    tool.status = ToolExecutionStatus::Denied;
                    tool.error = Some("approval denied".to_string());
                }
            }
            ExecutionEvent::ToolStarted { call_id } => {
                let tool = self.tool_mut(call_id)?;
                if tool.status != ToolExecutionStatus::Requested {
                    return Err(LedgerAppendError::InvalidTransition(format!(
                        "tool call '{call_id}' was not allowed before start"
                    )));
                }
                tool.status = ToolExecutionStatus::Started;
            }
            ExecutionEvent::ToolCompleted { call_id, result } => {
                let tool = self.started_tool_mut(call_id)?;
                tool.status = ToolExecutionStatus::Completed;
                tool.result = Some(result.clone());
            }
            ExecutionEvent::ToolFailed { call_id, error } => {
                let tool = self.started_tool_mut(call_id)?;
                tool.status = ToolExecutionStatus::Failed;
                tool.error = Some(error.clone());
            }
            ExecutionEvent::ToolIndeterminate { call_id, reason } => {
                let tool = self.started_tool_mut(call_id)?;
                tool.status = ToolExecutionStatus::Indeterminate;
                tool.error = Some(reason.clone());
            }
            ExecutionEvent::RunPaused { .. } => self.status = ExecutionStatus::Paused,
            ExecutionEvent::RunCompleted => {
                self.ensure_no_started_tools()?;
                self.status = ExecutionStatus::Completed;
            }
            ExecutionEvent::RunCancelled => self.status = ExecutionStatus::Cancelled,
            ExecutionEvent::RunFailed { .. } => self.status = ExecutionStatus::Failed,
            ExecutionEvent::ModelRequestStarted { .. }
            | ExecutionEvent::ModelRequestAbandoned { .. }
            | ExecutionEvent::VerificationFinished { .. }
            | ExecutionEvent::QualityFindingRecorded { .. }
            | ExecutionEvent::CheckpointReferenced { .. }
            | ExecutionEvent::ImportedCompletedTurn { .. } => {}
        }

        self.sequence = envelope.sequence;
        self.last_event_id = Some(envelope.event_id.clone());
        Ok(())
    }

    /// Computes the only recovery action that can safely follow this projection.
    pub fn recovery_disposition(&self) -> RecoveryDisposition {
        if matches!(
            self.status,
            ExecutionStatus::Completed | ExecutionStatus::Failed | ExecutionStatus::Cancelled
        ) {
            return RecoveryDisposition::Terminal {
                status: self.status,
            };
        }
        if let Some(approval) = &self.pending_approval {
            return RecoveryDisposition::AwaitApproval {
                approval_id: approval.approval_id.clone(),
            };
        }
        if let Some(tool) = self
            .tools
            .values()
            .find(|tool| tool.status == ToolExecutionStatus::Started)
        {
            return RecoveryDisposition::RequiresToolResolution {
                call_id: tool.call_id.clone(),
            };
        }
        RecoveryDisposition::ResumeProvider
    }

    fn tool_mut(
        &mut self,
        call_id: &str,
    ) -> Result<&mut ToolExecutionProjection, LedgerAppendError> {
        self.tools.get_mut(call_id).ok_or_else(|| {
            LedgerAppendError::InvalidTransition(format!("unknown tool call '{call_id}'"))
        })
    }

    fn started_tool_mut(
        &mut self,
        call_id: &str,
    ) -> Result<&mut ToolExecutionProjection, LedgerAppendError> {
        let tool = self.tool_mut(call_id)?;
        if tool.status != ToolExecutionStatus::Started {
            return Err(LedgerAppendError::InvalidTransition(format!(
                "tool call '{call_id}' has not started"
            )));
        }
        Ok(tool)
    }

    fn ensure_no_started_tools(&self) -> Result<(), LedgerAppendError> {
        if let Some(tool) = self
            .tools
            .values()
            .find(|tool| tool.status == ToolExecutionStatus::Started)
        {
            return Err(LedgerAppendError::InvalidTransition(format!(
                "tool call '{}' is still started",
                tool.call_id
            )));
        }
        Ok(())
    }
}

/// Errors returned when an execution event cannot be durably appended.
#[derive(Debug, Clone, thiserror::Error)]
pub enum LedgerAppendError {
    /// A caller attempted to append at an unexpected sequence.
    #[error("execution sequence conflict: expected {expected}, got {actual}")]
    InvalidSequence {
        /// Sequence expected by the durable stream.
        expected: u64,
        /// Sequence supplied or encountered by the caller.
        actual: u64,
    },
    /// The event would violate an execution lifecycle invariant.
    #[error("invalid execution transition: {0}")]
    InvalidTransition(String),
    /// The run already reached a terminal state.
    #[error("execution run '{run_id}' is terminal")]
    TerminalRun {
        /// Terminal run identifier.
        run_id: String,
    },
    /// An event envelope has an unsupported schema version.
    #[error("unsupported execution schema version {actual}")]
    UnsupportedSchema {
        /// Actual schema version found in the record.
        actual: u32,
    },
    /// The requested run was not found in the ledger.
    #[error("execution run '{run_id}' was not found")]
    NotFound {
        /// Missing run identifier.
        run_id: String,
    },
    /// Durable storage could not read or write the execution stream.
    #[error("execution ledger storage error: {0}")]
    Storage(String),
}

/// Durable append/read interface for execution streams.
pub trait ExecutionLedger: Send + Sync {
    /// Appends one event using optimistic sequence ownership.
    fn append(&self, append: ExecutionAppend) -> Result<ExecutionEventEnvelope, LedgerAppendError>;

    /// Loads immutable events in ascending sequence order.
    fn events(&self, run_id: &str) -> Result<Vec<ExecutionEventEnvelope>, LedgerAppendError>;

    /// Loads the current projection for one execution run.
    fn projection(&self, run_id: &str) -> Result<Option<RunProjection>, LedgerAppendError>;
}

/// A compatibility ledger that deliberately records nothing.
#[derive(Debug, Default)]
pub struct NoopExecutionLedger;

impl ExecutionLedger for NoopExecutionLedger {
    fn append(&self, append: ExecutionAppend) -> Result<ExecutionEventEnvelope, LedgerAppendError> {
        let sequence = append.expected_sequence.saturating_add(1);
        Ok(ExecutionEventEnvelope::from_append(append, sequence))
    }

    fn events(&self, _run_id: &str) -> Result<Vec<ExecutionEventEnvelope>, LedgerAppendError> {
        Ok(Vec::new())
    }

    fn projection(&self, _run_id: &str) -> Result<Option<RunProjection>, LedgerAppendError> {
        Ok(None)
    }
}

/// Shared dynamic ledger handle used by runtime builders and runners.
pub type ExecutionLedgerHandle = Arc<dyn ExecutionLedger>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Role;

    fn append(sequence: u64, event: ExecutionEvent) -> ExecutionEventEnvelope {
        ExecutionEventEnvelope::from_append(
            ExecutionAppend::new(
                "session-1",
                "run-1",
                ExecutionPath::Agent,
                sequence - 1,
                event,
            ),
            sequence,
        )
    }

    fn projection() -> RunProjection {
        RunProjection::new(&append(1, ExecutionEvent::RunAccepted { workspace: None })).unwrap()
    }

    #[test]
    fn projection_tracks_tool_lifecycle_and_completion() {
        let mut projection = projection();
        projection
            .apply(&append(
                2,
                ExecutionEvent::ToolRequested {
                    call_id: "call-1".into(),
                    tool_name: "read_file".into(),
                    arguments: r#"{"path":"src/lib.rs"}"#.into(),
                    node_id: None,
                },
            ))
            .unwrap();
        projection
            .apply(&append(
                3,
                ExecutionEvent::PolicyEvaluated {
                    call_id: "call-1".into(),
                    decision: ExecutionDecision::Allow,
                    reason: "allowed".into(),
                },
            ))
            .unwrap();
        projection
            .apply(&append(
                4,
                ExecutionEvent::ToolStarted {
                    call_id: "call-1".into(),
                },
            ))
            .unwrap();
        assert_eq!(
            projection.recovery_disposition(),
            RecoveryDisposition::RequiresToolResolution {
                call_id: "call-1".into()
            }
        );
        projection
            .apply(&append(
                5,
                ExecutionEvent::ToolCompleted {
                    call_id: "call-1".into(),
                    result: "contents".into(),
                },
            ))
            .unwrap();
        projection
            .apply(&append(6, ExecutionEvent::RunCompleted))
            .unwrap();
        assert_eq!(
            projection.recovery_disposition(),
            RecoveryDisposition::Terminal {
                status: ExecutionStatus::Completed
            }
        );
    }

    #[test]
    fn projection_requires_matching_approval_before_tool_start() {
        let mut projection = projection();
        projection
            .apply(&append(
                2,
                ExecutionEvent::ToolRequested {
                    call_id: "call-1".into(),
                    tool_name: "write_file".into(),
                    arguments: "{}".into(),
                    node_id: None,
                },
            ))
            .unwrap();
        projection
            .apply(&append(
                3,
                ExecutionEvent::PolicyEvaluated {
                    call_id: "call-1".into(),
                    decision: ExecutionDecision::Ask,
                    reason: "requires approval".into(),
                },
            ))
            .unwrap();
        projection
            .apply(&append(
                4,
                ExecutionEvent::ApprovalRequested {
                    approval_id: "approval-1".into(),
                    call_id: "call-1".into(),
                    title: "Write a file".into(),
                    description: None,
                },
            ))
            .unwrap();
        assert_eq!(
            projection.recovery_disposition(),
            RecoveryDisposition::AwaitApproval {
                approval_id: "approval-1".into()
            }
        );
        let err = projection
            .apply(&append(
                5,
                ExecutionEvent::ToolStarted {
                    call_id: "call-1".into(),
                },
            ))
            .unwrap_err();
        assert!(matches!(err, LedgerAppendError::InvalidTransition(_)));
        projection
            .apply(&append(
                5,
                ExecutionEvent::ApprovalResolved {
                    approval_id: "approval-1".into(),
                    approved: true,
                },
            ))
            .unwrap();
        projection
            .apply(&append(
                6,
                ExecutionEvent::ToolStarted {
                    call_id: "call-1".into(),
                },
            ))
            .unwrap();
    }

    #[test]
    fn projection_rejects_non_contiguous_and_terminal_events() {
        let mut projection = projection();
        let err = projection
            .apply(&append(
                3,
                ExecutionEvent::ModelRequestStarted { model: None },
            ))
            .unwrap_err();
        assert!(matches!(err, LedgerAppendError::InvalidSequence { .. }));

        projection
            .apply(&append(2, ExecutionEvent::RunCompleted))
            .unwrap();
        let err = projection
            .apply(&append(
                3,
                ExecutionEvent::AssistantMessageCommitted {
                    message: Message {
                        role: Role::Assistant,
                        content: "late".into(),
                        name: None,
                        tool_calls: None,
                        tool_call_id: None,
                        reasoning_content: None,
                    },
                },
            ))
            .unwrap_err();
        assert!(matches!(err, LedgerAppendError::TerminalRun { .. }));
    }

    #[test]
    fn envelope_round_trips_through_json() {
        let event = append(
            1,
            ExecutionEvent::RunAccepted {
                workspace: Some("/workspace".into()),
            },
        );
        let parsed: ExecutionEventEnvelope =
            serde_json::from_str(&serde_json::to_string(&event).unwrap()).unwrap();
        assert_eq!(parsed.sequence, 1);
        assert!(matches!(parsed.event, ExecutionEvent::RunAccepted { .. }));
    }
}

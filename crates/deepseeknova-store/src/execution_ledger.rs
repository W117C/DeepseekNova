//! SQLite-backed implementation of [`deepseeknova_core::execution::ExecutionLedger`].
//!
//! **库级 API — 当前未接入生产路径。** 与 `core::execution` 同步预留，待恢复
//! 驱动落地时装配。当前无生产消费者。

use deepseeknova_core::execution::{
    ExecutionAppend, ExecutionEventEnvelope, ExecutionLedger, LedgerAppendError, RunProjection,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::path::Path;
use std::sync::Mutex;

const LEDGER_SCHEMA_VERSION: u32 = 1;

/// SQLite-backed append-only execution ledger with transactional projection updates.
pub struct SqliteExecutionLedger {
    connection: Mutex<Connection>,
}

impl std::fmt::Debug for SqliteExecutionLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteExecutionLedger")
            .finish_non_exhaustive()
    }
}

impl SqliteExecutionLedger {
    /// Opens or creates a ledger database and applies the current schema.
    pub fn open(path: &Path) -> Result<Self, LedgerAppendError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| LedgerAppendError::Storage(error.to_string()))?;
        }
        let connection = Connection::open(path).map_err(storage_error)?;
        configure(&connection)?;
        migrate(&connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    /// Opens an isolated in-memory ledger for tests and embedded callers.
    pub fn open_in_memory() -> Result<Self, LedgerAppendError> {
        let connection = Connection::open_in_memory().map_err(storage_error)?;
        configure(&connection)?;
        migrate(&connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }
}

impl ExecutionLedger for SqliteExecutionLedger {
    fn append(&self, append: ExecutionAppend) -> Result<ExecutionEventEnvelope, LedgerAppendError> {
        let mut connection = self.connection.lock().map_err(|_| {
            LedgerAppendError::Storage("execution ledger lock poisoned".to_string())
        })?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        let projection = load_projection_tx(&transaction, &append.run_id)?;
        let next_sequence = match &projection {
            Some(projection) => {
                let expected = projection.sequence;
                if append.expected_sequence != expected {
                    return Err(LedgerAppendError::InvalidSequence {
                        expected,
                        actual: append.expected_sequence,
                    });
                }
                expected.saturating_add(1)
            }
            None => {
                if append.expected_sequence != 0 {
                    return Err(LedgerAppendError::InvalidSequence {
                        expected: 0,
                        actual: append.expected_sequence,
                    });
                }
                1
            }
        };
        let envelope = ExecutionEventEnvelope::from_append(append, next_sequence);
        let sqlite_sequence = sequence_to_sqlite(envelope.sequence)?;
        let next_projection = match projection {
            Some(mut projection) => {
                projection.apply(&envelope)?;
                projection
            }
            None => RunProjection::new(&envelope)?,
        };
        let payload = serde_json::to_string(&envelope.event)
            .map_err(|error| LedgerAppendError::Storage(error.to_string()))?;
        transaction
            .execute(
                "INSERT INTO execution_events (
                    run_id, sequence, event_id, session_id, turn_id, occurred_at,
                    path, causation_id, schema_version, payload
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    envelope.run_id,
                    sqlite_sequence,
                    envelope.event_id,
                    envelope.session_id,
                    envelope.turn_id,
                    envelope.occurred_at,
                    serde_json::to_string(&envelope.path)
                        .map_err(|error| LedgerAppendError::Storage(error.to_string()))?,
                    envelope.causation_id,
                    envelope.schema_version,
                    payload,
                ],
            )
            .map_err(storage_error)?;
        save_projection_tx(&transaction, &next_projection)?;
        transaction.commit().map_err(storage_error)?;
        Ok(envelope)
    }

    fn events(&self, run_id: &str) -> Result<Vec<ExecutionEventEnvelope>, LedgerAppendError> {
        let connection = self.connection.lock().map_err(|_| {
            LedgerAppendError::Storage("execution ledger lock poisoned".to_string())
        })?;
        let mut statement = connection
            .prepare(
                "SELECT sequence, event_id, session_id, turn_id, occurred_at,
                    path, causation_id, schema_version, payload
                 FROM execution_events
                 WHERE run_id = ?1
                 ORDER BY sequence ASC",
            )
            .map_err(storage_error)?;
        let rows = statement
            .query_map(params![run_id], |row| {
                let payload: String = row.get(8)?;
                let path: String = row.get(5)?;
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    path,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, u32>(7)?,
                    payload,
                ))
            })
            .map_err(storage_error)?;
        let mut events = Vec::new();
        for row in rows {
            let (
                sequence,
                event_id,
                session_id,
                turn_id,
                occurred_at,
                path,
                causation_id,
                schema_version,
                payload,
            ) = row.map_err(storage_error)?;
            events.push(ExecutionEventEnvelope {
                schema_version,
                event_id,
                session_id,
                run_id: run_id.to_string(),
                turn_id,
                sequence: sequence_from_sqlite(sequence)?,
                occurred_at,
                path: serde_json::from_str(&path)
                    .map_err(|error| LedgerAppendError::Storage(error.to_string()))?,
                causation_id,
                event: serde_json::from_str(&payload)
                    .map_err(|error| LedgerAppendError::Storage(error.to_string()))?,
            });
        }
        Ok(events)
    }

    fn projection(&self, run_id: &str) -> Result<Option<RunProjection>, LedgerAppendError> {
        let connection = self.connection.lock().map_err(|_| {
            LedgerAppendError::Storage("execution ledger lock poisoned".to_string())
        })?;
        load_projection(&connection, run_id)
    }
}

fn configure(connection: &Connection) -> Result<(), LedgerAppendError> {
    connection
        .execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;",
        )
        .map_err(storage_error)
}

fn migrate(connection: &Connection) -> Result<(), LedgerAppendError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS ledger_meta (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS execution_events (
                run_id TEXT NOT NULL,
                sequence INTEGER NOT NULL,
                event_id TEXT NOT NULL UNIQUE,
                session_id TEXT NOT NULL,
                turn_id TEXT,
                occurred_at TEXT NOT NULL,
                path TEXT NOT NULL,
                causation_id TEXT,
                schema_version INTEGER NOT NULL,
                payload TEXT NOT NULL,
                PRIMARY KEY (run_id, sequence)
             );
             CREATE TABLE IF NOT EXISTS execution_projection (
                run_id TEXT PRIMARY KEY NOT NULL,
                session_id TEXT NOT NULL,
                sequence INTEGER NOT NULL,
                projection TEXT NOT NULL
             );",
        )
        .map_err(storage_error)?;
    let existing: Option<String> = connection
        .query_row(
            "SELECT value FROM ledger_meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage_error)?;
    match existing {
        Some(version) if version == LEDGER_SCHEMA_VERSION.to_string() => Ok(()),
        Some(version) => Err(LedgerAppendError::UnsupportedSchema {
            actual: version.parse().unwrap_or(0),
        }),
        None => {
            connection
                .execute(
                    "INSERT INTO ledger_meta (key, value) VALUES ('schema_version', ?1)",
                    params![LEDGER_SCHEMA_VERSION.to_string()],
                )
                .map_err(storage_error)?;
            Ok(())
        }
    }
}

fn load_projection(
    connection: &Connection,
    run_id: &str,
) -> Result<Option<RunProjection>, LedgerAppendError> {
    connection
        .query_row(
            "SELECT projection FROM execution_projection WHERE run_id = ?1",
            params![run_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(storage_error)?
        .map(|projection| {
            serde_json::from_str(&projection)
                .map_err(|error| LedgerAppendError::Storage(error.to_string()))
        })
        .transpose()
}

fn load_projection_tx(
    transaction: &rusqlite::Transaction<'_>,
    run_id: &str,
) -> Result<Option<RunProjection>, LedgerAppendError> {
    transaction
        .query_row(
            "SELECT projection FROM execution_projection WHERE run_id = ?1",
            params![run_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(storage_error)?
        .map(|projection| {
            serde_json::from_str(&projection)
                .map_err(|error| LedgerAppendError::Storage(error.to_string()))
        })
        .transpose()
}

fn save_projection_tx(
    transaction: &rusqlite::Transaction<'_>,
    projection: &RunProjection,
) -> Result<(), LedgerAppendError> {
    let serialized = serde_json::to_string(projection)
        .map_err(|error| LedgerAppendError::Storage(error.to_string()))?;
    let sqlite_sequence = sequence_to_sqlite(projection.sequence)?;
    transaction
        .execute(
            "INSERT INTO execution_projection (run_id, session_id, sequence, projection)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(run_id) DO UPDATE SET
                 session_id = excluded.session_id,
                 sequence = excluded.sequence,
                 projection = excluded.projection",
            params![
                projection.run_id,
                projection.session_id,
                sqlite_sequence,
                serialized,
            ],
        )
        .map_err(storage_error)?;
    Ok(())
}

fn sequence_to_sqlite(sequence: u64) -> Result<i64, LedgerAppendError> {
    i64::try_from(sequence).map_err(|_| {
        LedgerAppendError::Storage(format!(
            "execution sequence {sequence} exceeds SQLite INTEGER range"
        ))
    })
}

fn sequence_from_sqlite(sequence: i64) -> Result<u64, LedgerAppendError> {
    u64::try_from(sequence).map_err(|_| {
        LedgerAppendError::Storage(format!(
            "execution sequence {sequence} is negative in SQLite"
        ))
    })
}

fn storage_error(error: rusqlite::Error) -> LedgerAppendError {
    LedgerAppendError::Storage(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepseeknova_core::execution::{
        ExecutionDecision, ExecutionEvent, ExecutionPath, ExecutionStatus, RecoveryDisposition,
    };

    fn append(
        ledger: &SqliteExecutionLedger,
        expected_sequence: u64,
        event: ExecutionEvent,
    ) -> ExecutionEventEnvelope {
        ledger
            .append(ExecutionAppend::new(
                "session-1",
                "run-1",
                ExecutionPath::Agent,
                expected_sequence,
                event,
            ))
            .unwrap()
    }

    #[test]
    fn appends_events_and_updates_projection_atomically() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        append(&ledger, 0, ExecutionEvent::RunAccepted { workspace: None });
        append(
            &ledger,
            1,
            ExecutionEvent::ToolRequested {
                call_id: "call-1".into(),
                tool_name: "write_file".into(),
                arguments: "{}".into(),
                node_id: None,
            },
        );
        append(
            &ledger,
            2,
            ExecutionEvent::PolicyEvaluated {
                call_id: "call-1".into(),
                decision: ExecutionDecision::Allow,
                reason: "allowed".into(),
            },
        );
        append(
            &ledger,
            3,
            ExecutionEvent::ToolStarted {
                call_id: "call-1".into(),
            },
        );

        assert_eq!(ledger.events("run-1").unwrap().len(), 4);
        let projection = ledger.projection("run-1").unwrap().unwrap();
        assert_eq!(projection.sequence, 4);
        assert_eq!(
            projection.recovery_disposition(),
            RecoveryDisposition::RequiresToolResolution {
                call_id: "call-1".into()
            }
        );
    }

    #[test]
    fn rejects_stale_sequence_without_writing_an_event() {
        let ledger = SqliteExecutionLedger::open_in_memory().unwrap();
        append(&ledger, 0, ExecutionEvent::RunAccepted { workspace: None });
        let error = ledger
            .append(ExecutionAppend::new(
                "session-1",
                "run-1",
                ExecutionPath::Agent,
                0,
                ExecutionEvent::ModelRequestStarted { model: None },
            ))
            .unwrap_err();
        assert!(matches!(error, LedgerAppendError::InvalidSequence { .. }));
        assert_eq!(ledger.events("run-1").unwrap().len(), 1);
    }

    #[test]
    fn preserves_terminal_state_across_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ledger.sqlite3");
        {
            let ledger = SqliteExecutionLedger::open(&path).unwrap();
            append(&ledger, 0, ExecutionEvent::RunAccepted { workspace: None });
            append(&ledger, 1, ExecutionEvent::RunCompleted);
        }
        let reopened = SqliteExecutionLedger::open(&path).unwrap();
        let projection = reopened.projection("run-1").unwrap().unwrap();
        assert_eq!(projection.status, ExecutionStatus::Completed);
        assert_eq!(
            projection.recovery_disposition(),
            RecoveryDisposition::Terminal {
                status: ExecutionStatus::Completed
            }
        );
    }
}

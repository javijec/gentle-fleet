//! Implementación SQLite de la autoridad de persistencia de Fleet.
//!
//! WAL, migraciones forward-only, transacciones explícitas y una sola base por
//! host/usuario. No contiene lógica de sesión ni de proveedores.

use std::path::{Path, PathBuf};

use fleet_domain::store::OperationStatus;
use fleet_domain::store::*;
use fleet_domain::{Event, SessionState, TurnState};
use rusqlite::{params, Connection, OptionalExtension, Transaction};

const MIGRATION_V1: &str = r#"
CREATE TABLE IF NOT EXISTS workspaces (
    id TEXT PRIMARY KEY,
    path TEXT NOT NULL UNIQUE
);
CREATE TABLE IF NOT EXISTS sessions (
    session_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    provider_id TEXT NOT NULL,
    state TEXT NOT NULL,
    turn_state TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS turns (
    turn_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(session_id),
    operation_id TEXT NOT NULL,
    state TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS operations (
    operation_id TEXT PRIMARY KEY,
    operation_key TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL,
    session_id TEXT NOT NULL,
    target_id TEXT,
    fingerprint TEXT NOT NULL,
    status TEXT NOT NULL,
    message TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS events (
    session_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    event_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (session_id, seq)
);
CREATE INDEX IF NOT EXISTS idx_events_session_seq ON events(session_id, seq);
CREATE TABLE IF NOT EXISTS approvals (
    approval_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    message TEXT NOT NULL,
    resources TEXT NOT NULL,
    allowed_decisions TEXT NOT NULL,
    status TEXT NOT NULL,
    native_request_id TEXT NOT NULL,
    decision TEXT,
    created_at INTEGER NOT NULL,
    resolved_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_approvals_session ON approvals(session_id, status);
"#;

pub struct SqliteStore {
    path: PathBuf,
}

impl SqliteStore {
    pub fn open(path: impl Into<PathBuf>) -> StoreResult<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                StoreError::new(
                    "store_dir_unavailable",
                    format!("{}: {e}", parent.display()),
                )
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
            }
        }
        let store = Self { path };
        store.init()?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn connection(&self) -> StoreResult<Connection> {
        Connection::open(&self.path)
            .map_err(|e| StoreError::new("store_open_failed", e.to_string()))
    }

    fn configure(connection: &Connection) -> StoreResult<()> {
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| StoreError::new("store_config_failed", e.to_string()))?;
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .map_err(|e| StoreError::new("store_config_failed", e.to_string()))?;
        connection
            .pragma_update(None, "busy_timeout", 5_000)
            .map_err(|e| StoreError::new("store_config_failed", e.to_string()))?;
        Ok(())
    }

    fn integrity(connection: &Connection) -> StoreResult<bool> {
        let result: String = connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .map_err(|e| StoreError::new("corrupt_database", e.to_string()))?;
        Ok(result == "ok")
    }
}

fn state_text(state: SessionState) -> &'static str {
    match state {
        SessionState::Running => "running",
        SessionState::Stopping => "stopping",
        SessionState::StopUnconfirmed => "stop_unconfirmed",
        SessionState::Stopped => "stopped",
        SessionState::Failed => "failed",
        SessionState::NeedsReconciliation => "needs_reconciliation",
    }
}

fn parse_state(value: &str) -> SessionState {
    match value {
        "stopping" => SessionState::Stopping,
        "stop_unconfirmed" => SessionState::StopUnconfirmed,
        "stopped" => SessionState::Stopped,
        "failed" => SessionState::Failed,
        "needs_reconciliation" => SessionState::NeedsReconciliation,
        _ => SessionState::Running,
    }
}

fn turn_text(state: TurnState) -> &'static str {
    match state {
        TurnState::Idle => "idle",
        TurnState::Running => "running",
        TurnState::AwaitingApproval => "awaiting_approval",
        TurnState::Completed => "completed",
        TurnState::Failed => "failed",
        TurnState::Interrupted => "interrupted",
        TurnState::Uncertain => "uncertain",
    }
}

fn parse_turn(value: &str) -> TurnState {
    match value {
        "idle" => TurnState::Idle,
        "awaiting_approval" => TurnState::AwaitingApproval,
        "completed" => TurnState::Completed,
        "failed" => TurnState::Failed,
        "interrupted" => TurnState::Interrupted,
        "uncertain" => TurnState::Uncertain,
        _ => TurnState::Running,
    }
}

fn operation_text(status: OperationStatus) -> &'static str {
    match status {
        OperationStatus::Reserved => "reserved",
        OperationStatus::Accepted => "accepted",
        OperationStatus::Completed => "completed",
        OperationStatus::Failed => "failed",
        OperationStatus::Uncertain => "uncertain",
    }
}

fn parse_operation(value: &str) -> OperationStatus {
    match value {
        "accepted" => OperationStatus::Accepted,
        "completed" => OperationStatus::Completed,
        "failed" => OperationStatus::Failed,
        "uncertain" => OperationStatus::Uncertain,
        _ => OperationStatus::Reserved,
    }
}

fn approval_text(status: ApprovalStatus) -> &'static str {
    match status {
        ApprovalStatus::Pending => "pending",
        ApprovalStatus::Delivered => "delivered",
        ApprovalStatus::Accepted => "accepted",
        ApprovalStatus::Rejected => "rejected",
        ApprovalStatus::Uncertain => "uncertain",
    }
}

fn parse_approval(value: &str) -> ApprovalStatus {
    match value {
        "delivered" => ApprovalStatus::Delivered,
        "accepted" => ApprovalStatus::Accepted,
        "rejected" => ApprovalStatus::Rejected,
        "uncertain" => ApprovalStatus::Uncertain,
        _ => ApprovalStatus::Pending,
    }
}

fn json_vec(value: &str) -> Vec<String> {
    serde_json::from_str(value).unwrap_or_default()
}

impl SessionStore for SqliteStore {
    fn init(&self) -> StoreResult<()> {
        let mut connection = self.connection()?;
        Self::configure(&connection)?;
        if !Self::integrity(&connection)? {
            return Err(StoreError::new(
                "corrupt_database",
                "integrity_check failed",
            ));
        }
        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|e| StoreError::new("migration_failed", e.to_string()))?;
        if version == 0 {
            let transaction = connection
                .transaction()
                .map_err(|e| StoreError::new("transaction_failed", e.to_string()))?;
            transaction
                .execute_batch(MIGRATION_V1)
                .map_err(|e| StoreError::new("migration_failed", e.to_string()))?;
            transaction
                .pragma_update(None, "user_version", 1)
                .map_err(|e| StoreError::new("migration_failed", e.to_string()))?;
            transaction
                .commit()
                .map_err(|e| StoreError::new("commit_failed", e.to_string()))?;
        }
        Ok(())
    }

    fn health(&self) -> StoreResult<StoreHealth> {
        let connection = self.connection()?;
        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|e| StoreError::new("store_read_failed", e.to_string()))?;
        Ok(StoreHealth {
            schema_version: version,
            integrity_ok: Self::integrity(&connection)?,
        })
    }

    fn save_workspace(&self, workspace: &WorkspaceRecord) -> StoreResult<()> {
        let connection = self.connection()?;
        connection
            .execute(
                "INSERT OR REPLACE INTO workspaces(id, path) VALUES (?1, ?2)",
                params![workspace.id, workspace.path],
            )
            .map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
        Ok(())
    }

    fn list_workspaces(&self) -> StoreResult<Vec<WorkspaceRecord>> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare("SELECT id, path FROM workspaces ORDER BY path")
            .map_err(|e| StoreError::new("store_read_failed", e.to_string()))?;
        let rows = statement
            .query_map([], |row| {
                Ok(WorkspaceRecord {
                    id: row.get(0)?,
                    path: row.get(1)?,
                })
            })
            .map_err(|e| StoreError::new("store_read_failed", e.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| StoreError::new("store_read_failed", e.to_string()))
    }

    fn save_session(&self, session: &SessionRecord) -> StoreResult<()> {
        let connection = self.connection()?;
        connection
            .execute(
                "INSERT INTO sessions(session_id, workspace_id, provider_id, state, turn_state, created_at, updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)
                 ON CONFLICT(session_id) DO UPDATE SET state=excluded.state,
                   turn_state=excluded.turn_state, updated_at=excluded.updated_at",
                params![
                    session.session_id,
                    session.workspace_id,
                    session.provider_id,
                    state_text(session.state),
                    turn_text(session.turn_state),
                    session.created_at_ms as i64,
                    session.updated_at_ms as i64
                ],
            )
            .map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
        Ok(())
    }

    fn get_session(&self, session_id: &str) -> StoreResult<Option<SessionRecord>> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT session_id, workspace_id, provider_id, state, turn_state, created_at, updated_at
                 FROM sessions WHERE session_id=?1",
                params![session_id],
                |row| {
                    Ok(SessionRecord {
                        session_id: row.get(0)?,
                        workspace_id: row.get(1)?,
                        provider_id: row.get(2)?,
                        state: parse_state(&row.get::<_, String>(3)?),
                        turn_state: parse_turn(&row.get::<_, String>(4)?),
                        created_at_ms: row.get::<_, i64>(5)? as u64,
                        updated_at_ms: row.get::<_, i64>(6)? as u64,
                    })
                },
            )
            .optional()
            .map_err(|e| StoreError::new("store_read_failed", e.to_string()))
    }

    fn list_sessions(&self) -> StoreResult<Vec<SessionRecord>> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare("SELECT session_id, workspace_id, provider_id, state, turn_state, created_at, updated_at FROM sessions")
            .map_err(|e| StoreError::new("store_read_failed", e.to_string()))?;
        let rows = statement
            .query_map([], |row| {
                Ok(SessionRecord {
                    session_id: row.get(0)?,
                    workspace_id: row.get(1)?,
                    provider_id: row.get(2)?,
                    state: parse_state(&row.get::<_, String>(3)?),
                    turn_state: parse_turn(&row.get::<_, String>(4)?),
                    created_at_ms: row.get::<_, i64>(5)? as u64,
                    updated_at_ms: row.get::<_, i64>(6)? as u64,
                })
            })
            .map_err(|e| StoreError::new("store_read_failed", e.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| StoreError::new("store_read_failed", e.to_string()))
    }

    fn max_seq(&self, session_id: &str) -> StoreResult<u64> {
        let connection = self.connection()?;
        let seq: Option<i64> = connection
            .query_row(
                "SELECT MAX(seq) FROM events WHERE session_id=?1",
                params![session_id],
                |row| row.get(0),
            )
            .map_err(|e| StoreError::new("store_read_failed", e.to_string()))?;
        Ok(seq.unwrap_or(0).max(0) as u64)
    }

    fn append_event(&self, event: &Event) -> StoreResult<()> {
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction()
            .map_err(|e| StoreError::new("transaction_failed", e.to_string()))?;
        append_event_tx(&transaction, event)?;
        transaction
            .commit()
            .map_err(|e| StoreError::new("commit_failed", e.to_string()))
    }

    fn read_events(
        &self,
        session_id: &str,
        since_seq: u64,
        limit: usize,
    ) -> StoreResult<Vec<Event>> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT payload FROM events WHERE session_id=?1 AND seq>?2 ORDER BY seq LIMIT ?3",
            )
            .map_err(|e| StoreError::new("store_read_failed", e.to_string()))?;
        let rows = statement
            .query_map(params![session_id, since_seq as i64, limit as i64], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|e| StoreError::new("store_read_failed", e.to_string()))?;
        let mut events = Vec::new();
        for row in rows {
            let payload = row.map_err(|e| StoreError::new("store_read_failed", e.to_string()))?;
            let event: Event = serde_json::from_str(&payload)
                .map_err(|e| StoreError::new("corrupt_event", e.to_string()))?;
            events.push(event);
        }
        Ok(events)
    }

    fn reserve_operation(&self, operation: &OperationRecord) -> StoreResult<OperationReservation> {
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction()
            .map_err(|e| StoreError::new("transaction_failed", e.to_string()))?;
        let existing: Option<(String, String, String, Option<String>)> = transaction
            .query_row(
                "SELECT fingerprint, kind, session_id, target_id FROM operations WHERE operation_key=?1",
                params![operation.operation_key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(|e| StoreError::new("store_read_failed", e.to_string()))?;
        if let Some((fingerprint, kind, session_id, _stored_target)) = existing {
            // `target_id` es un resultado para SendPrompt (turn_id), no un
            // argumento: compararlo provocaría Conflict en un reenvío válido.
            // El fingerprint ya incluye la identidad del objetivo cuando este
            // sí es parte de la solicitud (aprobaciones).
            let same = fingerprint == operation.fingerprint
                && kind == operation_kind_text(operation.kind)
                && session_id == operation.session_id;
            if same {
                let record = load_operation(&transaction, &operation.operation_key)?
                    .ok_or_else(|| StoreError::new("store_read_failed", "operation vanished"))?;
                transaction
                    .commit()
                    .map_err(|e| StoreError::new("commit_failed", e.to_string()))?;
                return Ok(OperationReservation::Existing(record));
            }
            return Ok(OperationReservation::Conflict);
        }
        transaction
            .execute(
                "INSERT INTO operations(operation_id, operation_key, kind, session_id, target_id,
                   fingerprint, status, message, created_at, updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    operation.operation_id,
                    operation.operation_key,
                    operation_kind_text(operation.kind),
                    operation.session_id,
                    operation.target_id,
                    operation.fingerprint,
                    operation_text(operation.status),
                    operation.message,
                    operation.created_at_ms as i64,
                    operation.updated_at_ms as i64
                ],
            )
            .map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
        let record = operation.clone();
        transaction
            .commit()
            .map_err(|e| StoreError::new("commit_failed", e.to_string()))?;
        Ok(OperationReservation::Created(record))
    }

    fn get_operation(&self, operation_key: &str) -> StoreResult<Option<OperationRecord>> {
        let connection = self.connection()?;
        load_operation(&connection, operation_key)
    }

    fn update_operation(
        &self,
        operation_id: &str,
        status: OperationStatus,
        message: Option<String>,
    ) -> StoreResult<()> {
        let connection = self.connection()?;
        connection
            .execute(
                "UPDATE operations SET status=?2, message=?3, updated_at=?4 WHERE operation_id=?1",
                params![
                    operation_id,
                    operation_text(status),
                    message,
                    now_ms() as i64
                ],
            )
            .map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
        Ok(())
    }

    fn save_turn(&self, turn: &TurnRecord) -> StoreResult<()> {
        let connection = self.connection()?;
        connection
            .execute(
                "INSERT INTO turns(turn_id, session_id, operation_id, state, created_at, updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(turn_id) DO UPDATE SET state=excluded.state, updated_at=excluded.updated_at",
                params![
                    turn.turn_id,
                    turn.session_id,
                    turn.operation_id,
                    turn_text(turn.state),
                    turn.created_at_ms as i64,
                    turn.updated_at_ms as i64
                ],
            )
            .map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
        Ok(())
    }

    fn get_turn(&self, turn_id: &str) -> StoreResult<Option<TurnRecord>> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT turn_id, session_id, operation_id, state, created_at, updated_at FROM turns WHERE turn_id=?1",
                params![turn_id],
                |row| {
                    Ok(TurnRecord {
                        turn_id: row.get(0)?,
                        session_id: row.get(1)?,
                        operation_id: row.get(2)?,
                        state: parse_turn(&row.get::<_, String>(3)?),
                        created_at_ms: row.get::<_, i64>(4)? as u64,
                        updated_at_ms: row.get::<_, i64>(5)? as u64,
                    })
                },
            )
            .optional()
            .map_err(|e| StoreError::new("store_read_failed", e.to_string()))
    }

    fn save_approval(&self, approval: &ApprovalRequest) -> StoreResult<()> {
        let connection = self.connection()?;
        connection
            .execute(
                "INSERT INTO approvals(approval_id, session_id, turn_id, kind, message, resources,
                   allowed_decisions, status, native_request_id, decision, created_at, resolved_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
                 ON CONFLICT(approval_id) DO UPDATE SET status=excluded.status,
                   decision=excluded.decision, resolved_at=excluded.resolved_at",
                params![
                    approval.approval_id,
                    approval.session_id,
                    approval.turn_id,
                    approval.kind,
                    approval.message,
                    serde_json::to_string(&approval.resources).unwrap_or_default(),
                    serde_json::to_string(&approval.allowed_decisions).unwrap_or_default(),
                    approval_text(approval.status),
                    approval.native_request_id,
                    approval.decision,
                    approval.created_at_ms as i64,
                    approval.resolved_at_ms.map(|value| value as i64)
                ],
            )
            .map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
        Ok(())
    }

    fn get_approval(&self, approval_id: &str) -> StoreResult<Option<ApprovalRequest>> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT * FROM approvals WHERE approval_id=?1",
                params![approval_id],
                row_to_approval,
            )
            .optional()
            .map_err(|e| StoreError::new("store_read_failed", e.to_string()))
    }

    fn list_approvals(&self, session_id: Option<&str>) -> StoreResult<Vec<ApprovalRequest>> {
        let connection = self.connection()?;
        let sql = match session_id {
            Some(_) => "SELECT * FROM approvals WHERE session_id=?1 ORDER BY created_at",
            None => "SELECT * FROM approvals ORDER BY created_at",
        };
        let mut statement = connection
            .prepare(sql)
            .map_err(|e| StoreError::new("store_read_failed", e.to_string()))?;
        let rows = match session_id {
            Some(id) => statement
                .query_map(params![id], row_to_approval)
                .map_err(|e| StoreError::new("store_read_failed", e.to_string()))?,
            None => statement
                .query_map([], row_to_approval)
                .map_err(|e| StoreError::new("store_read_failed", e.to_string()))?,
        };
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| StoreError::new("store_read_failed", e.to_string()))
    }

    fn resolve_approval(
        &self,
        approval_id: &str,
        decision: &str,
        status: ApprovalStatus,
    ) -> StoreResult<bool> {
        let connection = self.connection()?;
        let changed = connection
            .execute(
                "UPDATE approvals SET status=?2, decision=?3, resolved_at=?4
                 WHERE approval_id=?1 AND status IN ('pending','delivered')",
                params![
                    approval_id,
                    approval_text(status),
                    decision,
                    now_ms() as i64
                ],
            )
            .map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
        Ok(changed == 1)
    }

    fn update_approval_status(&self, approval_id: &str, status: ApprovalStatus) -> StoreResult<()> {
        let connection = self.connection()?;
        connection
            .execute(
                "UPDATE approvals SET status=?2 WHERE approval_id=?1",
                params![approval_id, approval_text(status)],
            )
            .map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
        Ok(())
    }
}

fn operation_kind_text(kind: fleet_domain::store::OperationKind) -> &'static str {
    match kind {
        fleet_domain::store::OperationKind::SendPrompt => "send_prompt",
        fleet_domain::store::OperationKind::AnswerApproval => "answer_approval",
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn load_operation(
    connection: &Connection,
    operation_key: &str,
) -> StoreResult<Option<OperationRecord>> {
    connection
        .query_row(
            "SELECT operation_id, operation_key, kind, session_id, target_id, fingerprint,
                    status, message, created_at, updated_at FROM operations WHERE operation_key=?1",
            params![operation_key],
            |row| {
                let kind = match row.get::<_, String>(2)?.as_str() {
                    "answer_approval" => fleet_domain::store::OperationKind::AnswerApproval,
                    _ => fleet_domain::store::OperationKind::SendPrompt,
                };
                Ok(OperationRecord {
                    operation_id: row.get(0)?,
                    operation_key: row.get(1)?,
                    kind,
                    session_id: row.get(3)?,
                    target_id: row.get(4)?,
                    fingerprint: row.get(5)?,
                    status: parse_operation(&row.get::<_, String>(6)?),
                    message: row.get(7)?,
                    created_at_ms: row.get::<_, i64>(8)? as u64,
                    updated_at_ms: row.get::<_, i64>(9)? as u64,
                })
            },
        )
        .optional()
        .map_err(|e| StoreError::new("store_read_failed", e.to_string()))
}

fn append_event_tx(transaction: &Transaction<'_>, event: &Event) -> StoreResult<()> {
    let existing: Option<String> = transaction
        .query_row(
            "SELECT event_id FROM events WHERE session_id=?1 AND seq=?2",
            params![event.session_id, event.seq as i64],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| StoreError::new("store_read_failed", e.to_string()))?;
    if let Some(event_id) = existing {
        if event_id == event.event_id {
            return Ok(()); // reentrega idempotente
        }
        return Err(StoreError::new(
            "sequence_conflict",
            format!("seq {} already used in {}", event.seq, event.session_id),
        ));
    }
    let payload = serde_json::to_string(event)
        .map_err(|e| StoreError::new("serialization_failed", e.to_string()))?;
    let kind = event_kind_name(event);
    transaction
        .execute(
            "INSERT INTO events(session_id, seq, event_id, kind, payload, created_at)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                event.session_id,
                event.seq as i64,
                event.event_id,
                kind,
                payload,
                event.created_at_ms as i64
            ],
        )
        .map(|_| ())
        .map_err(|e| StoreError::new("store_write_failed", e.to_string()))
}

fn event_kind_name(event: &Event) -> String {
    serde_json::to_value(&event.kind)
        .ok()
        .and_then(|value| {
            value
                .get("type")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "unknown".into())
}

fn row_to_approval(row: &rusqlite::Row<'_>) -> Result<ApprovalRequest, rusqlite::Error> {
    Ok(ApprovalRequest {
        approval_id: row.get(0)?,
        session_id: row.get(1)?,
        turn_id: row.get(2)?,
        kind: row.get(3)?,
        message: row.get(4)?,
        resources: json_vec(&row.get::<_, String>(5)?),
        allowed_decisions: json_vec(&row.get::<_, String>(6)?),
        status: parse_approval(&row.get::<_, String>(7)?),
        native_request_id: row.get(8)?,
        decision: row.get(9)?,
        created_at_ms: row.get::<_, i64>(10)? as u64,
        resolved_at_ms: row.get::<_, Option<i64>>(11)?.map(|value| value as u64),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fleet_domain::store::OperationKind;
    use fleet_domain::{EventKind, TurnStatus};
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_store() -> (SqliteStore, PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "gf-p2-store-{}-{}.sqlite",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let store = SqliteStore::open(path.clone()).unwrap();
        (store, path)
    }

    fn demo_event(seq: u64) -> Event {
        Event {
            event_id: format!("e{seq}"),
            session_id: "ses_1".into(),
            seq,
            created_at_ms: 1,
            kind: EventKind::TurnCompleted {
                turn_id: "turn_1".into(),
                status: TurnStatus::Completed,
            },
        }
    }

    #[test]
    fn migration_and_health_are_verified() {
        let (store, path) = temp_store();
        let health = store.health().unwrap();
        assert_eq!(health.schema_version, 1);
        assert!(health.integrity_ok);
        // Reabrir no vuelve a ejecutar la migración.
        drop(store);
        let reopened = SqliteStore::open(path).unwrap();
        assert_eq!(reopened.health().unwrap().schema_version, 1);
    }

    #[test]
    fn events_are_durable_and_paged_by_cursor() {
        let (store, _) = temp_store();
        for seq in 1..=5 {
            store.append_event(&demo_event(seq)).unwrap();
        }
        assert_eq!(store.max_seq("ses_1").unwrap(), 5);
        let page = store.read_events("ses_1", 2, 2).unwrap();
        assert_eq!(page.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![3, 4]);
        // Reentrega del mismo evento no duplica.
        store.append_event(&demo_event(3)).unwrap();
        assert_eq!(store.read_events("ses_1", 0, 10).unwrap().len(), 5);
    }

    #[test]
    fn operation_reservation_survives_reopen_and_detects_conflicts() {
        let (store, path) = temp_store();
        let operation = OperationRecord {
            operation_id: "op_1".into(),
            operation_key: "key-1".into(),
            kind: OperationKind::SendPrompt,
            session_id: "ses_1".into(),
            target_id: None,
            fingerprint: "ses_1\0hello".into(),
            status: OperationStatus::Reserved,
            message: None,
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        assert!(matches!(
            store.reserve_operation(&operation).unwrap(),
            OperationReservation::Created(_)
        ));
        store
            .update_operation("op_1", OperationStatus::Completed, None)
            .unwrap();
        drop(store);
        let reopened = SqliteStore::open(path).unwrap();
        assert!(matches!(
            reopened.reserve_operation(&operation).unwrap(),
            OperationReservation::Existing(_)
        ));
        let mut conflicting = operation.clone();
        conflicting.fingerprint = "ses_1\0other".into();
        assert_eq!(
            reopened.reserve_operation(&conflicting).unwrap(),
            OperationReservation::Conflict
        );
    }

    #[test]
    fn approval_decision_has_a_single_winner() {
        let (store, _) = temp_store();
        let approval = ApprovalRequest {
            approval_id: "appr_1".into(),
            session_id: "ses_1".into(),
            turn_id: "turn_1".into(),
            kind: "shell".into(),
            message: "allow?".into(),
            resources: vec!["echo hi".into()],
            allowed_decisions: vec!["once".into(), "reject".into()],
            status: ApprovalStatus::Pending,
            native_request_id: "per_native".into(),
            decision: None,
            created_at_ms: 1,
            resolved_at_ms: None,
        };
        store.save_approval(&approval).unwrap();
        assert!(store
            .resolve_approval("appr_1", "once", ApprovalStatus::Accepted)
            .unwrap());
        assert!(!store
            .resolve_approval("appr_1", "reject", ApprovalStatus::Rejected)
            .unwrap());
        assert_eq!(
            store.get_approval("appr_1").unwrap().unwrap().decision,
            Some("once".into())
        );
    }
}

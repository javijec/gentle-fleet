//! Implementación SQLite de la autoridad de persistencia de Fleet.
//!
//! WAL, migraciones forward-only, transacciones explícitas y una sola base por
//! host/usuario. No contiene lógica de sesión ni de proveedores.

use std::path::{Path, PathBuf};

use fleet_domain::store::OperationStatus;
use fleet_domain::store::*;
use fleet_domain::{Event, RunnerIdentity, RunnerStatus, SessionState, TurnState};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

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

const MIGRATION_V2: &str = r#"
DROP INDEX IF EXISTS idx_approvals_session;
ALTER TABLE approvals RENAME TO approvals_v1;
CREATE TABLE approvals (
    approval_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(session_id),
    turn_id TEXT NOT NULL REFERENCES turns(turn_id),
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
INSERT INTO approvals(approval_id, session_id, turn_id, kind, message, resources,
                      allowed_decisions, status, native_request_id, decision, created_at, resolved_at)
SELECT approval_id, session_id, turn_id, kind, message, resources,
       allowed_decisions, status, native_request_id, decision, created_at, resolved_at
FROM approvals_v1;
DROP TABLE approvals_v1;
CREATE INDEX idx_approvals_session ON approvals(session_id, status);
"#;

const MIGRATION_V3: &str = r#"
CREATE TABLE IF NOT EXISTS runners (
    runner_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL UNIQUE REFERENCES sessions(session_id),
    provider_id TEXT NOT NULL,
    host_id TEXT NOT NULL,
    boot_id TEXT NOT NULL,
    pid INTEGER NOT NULL,
    protocol_version TEXT NOT NULL,
    generation INTEGER NOT NULL,
    status TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    last_ack_seq INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_runners_status ON runners(status);
CREATE TABLE IF NOT EXISTS runner_events (
    runner_id TEXT NOT NULL REFERENCES runners(runner_id),
    runner_seq INTEGER NOT NULL,
    fleet_event_id TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (runner_id, runner_seq)
);
"#;

const MIGRATION_V4: &str = r#"
CREATE TABLE IF NOT EXISTS session_controls (
    session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    value TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (session_id, name)
);
"#;

pub struct SqliteStore {
    path: PathBuf,
}

impl SqliteStore {
    pub fn open(path: impl Into<PathBuf>) -> StoreResult<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            let existed = parent.exists();
            std::fs::create_dir_all(parent).map_err(|e| {
                StoreError::new(
                    "store_dir_unavailable",
                    format!("{}: {e}", parent.display()),
                )
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if !existed {
                    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                        .map_err(|e| StoreError::new("store_dir_unavailable", e.to_string()))?;
                } else {
                    let mode = std::fs::metadata(parent)
                        .map_err(|e| StoreError::new("store_dir_unavailable", e.to_string()))?
                        .permissions()
                        .mode();
                    if mode & 0o077 != 0 {
                        return Err(StoreError::new(
                            "insecure_store_dir",
                            format!(
                                "{} is accessible by other users; use a private directory",
                                parent.display()
                            ),
                        ));
                    }
                }
            }
            #[cfg(not(unix))]
            let _ = existed;
        }
        let store = Self { path };
        store.init()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&store.path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| StoreError::new("store_file_permissions", e.to_string()))?;
        }
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn connection(&self) -> StoreResult<Connection> {
        let connection = Connection::open(&self.path)
            .map_err(|e| StoreError::new("store_open_failed", e.to_string()))?;
        // Foreign keys, busy_timeout and trusted schema are connection-scoped.
        // Calling configure() only in init() silently bypassed FK checks.
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .map_err(|e| StoreError::new("store_config_failed", e.to_string()))?;
        connection
            .pragma_update(None, "busy_timeout", 5_000)
            .map_err(|e| StoreError::new("store_config_failed", e.to_string()))?;
        Ok(connection)
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
        SessionState::Recovering => "recovering",
    }
}

fn parse_state(value: &str) -> SessionState {
    match value {
        "stopping" => SessionState::Stopping,
        "stop_unconfirmed" => SessionState::StopUnconfirmed,
        "stopped" => SessionState::Stopped,
        "failed" => SessionState::Failed,
        "needs_reconciliation" | "recovering" => SessionState::NeedsReconciliation,
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
        ApprovalStatus::Dispatching => "dispatching",
        ApprovalStatus::Accepted => "accepted",
        ApprovalStatus::Rejected => "rejected",
        ApprovalStatus::Uncertain => "uncertain",
    }
}

fn parse_approval(value: &str) -> ApprovalStatus {
    match value {
        "delivered" => ApprovalStatus::Delivered,
        "dispatching" => ApprovalStatus::Dispatching,
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
        let mut version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|e| StoreError::new("migration_failed", e.to_string()))?;
        if version == 0 {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
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
            version = 1;
        }
        if version == 1 {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|e| StoreError::new("transaction_failed", e.to_string()))?;
            let orphans: i64 = transaction
                .query_row(
                    "SELECT COUNT(*) FROM approvals a
                     LEFT JOIN sessions s ON s.session_id=a.session_id
                     LEFT JOIN turns t ON t.turn_id=a.turn_id
                     WHERE s.session_id IS NULL OR t.turn_id IS NULL",
                    [],
                    |row| row.get(0),
                )
                .map_err(|e| StoreError::new("migration_failed", e.to_string()))?;
            if orphans > 0 {
                return Err(StoreError::new(
                    "migration_failed",
                    format!("{orphans} orphan approvals block the foreign-key migration"),
                ));
            }
            transaction
                .execute_batch(MIGRATION_V2)
                .map_err(|e| StoreError::new("migration_failed", e.to_string()))?;
            transaction
                .pragma_update(None, "user_version", 2)
                .map_err(|e| StoreError::new("migration_failed", e.to_string()))?;
            transaction
                .commit()
                .map_err(|e| StoreError::new("commit_failed", e.to_string()))?;
            version = 2;
        }
        if version == 2 {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|e| StoreError::new("transaction_failed", e.to_string()))?;
            transaction
                .execute_batch(MIGRATION_V3)
                .map_err(|e| StoreError::new("migration_failed", e.to_string()))?;
            transaction
                .pragma_update(None, "user_version", 3)
                .map_err(|e| StoreError::new("migration_failed", e.to_string()))?;
            transaction
                .commit()
                .map_err(|e| StoreError::new("commit_failed", e.to_string()))?;
            version = 3;
        }
        if version == 3 {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|e| StoreError::new("transaction_failed", e.to_string()))?;
            transaction
                .execute_batch(MIGRATION_V4)
                .map_err(|e| StoreError::new("migration_failed", e.to_string()))?;
            transaction
                .pragma_update(None, "user_version", 4)
                .map_err(|e| StoreError::new("migration_failed", e.to_string()))?;
            transaction
                .commit()
                .map_err(|e| StoreError::new("commit_failed", e.to_string()))?;
            version = 4;
        }
        if version != 4 {
            return Err(StoreError::new(
                "unsupported_schema_version",
                format!("unsupported SQLite schema version {version}; upgrade Fleet instead of guessing"),
            ));
        }
        Ok(())
    }

    fn recover_after_restart(&self) -> StoreResult<()> {
        let mut connection = self.connection()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| StoreError::new("transaction_failed", e.to_string()))?;
        // All prior runner handles were process-local. Classify rather than
        // re-send effects whose delivery or final result cannot be proven.
        tx.execute(
            "UPDATE operations SET status='uncertain',
               message=COALESCE(message, 'daemon restarted before a durable final result'),
               updated_at=?1
             WHERE status IN ('reserved','accepted')",
            params![now_ms() as i64],
        )
        .map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
        tx.execute(
            "UPDATE turns SET state='uncertain', updated_at=?1
             WHERE state IN ('running','awaiting_approval')",
            params![now_ms() as i64],
        )
        .map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
        tx.execute(
            "UPDATE sessions SET state='needs_reconciliation',
                turn_state=CASE
                    WHEN EXISTS(
                        SELECT 1 FROM turns
                        WHERE turns.session_id=sessions.session_id AND turns.state='uncertain'
                    ) THEN 'uncertain'
                    ELSE sessions.turn_state
                END,
                updated_at=?1
             WHERE state IN ('running','stopping','stop_unconfirmed','recovering')",
            params![now_ms() as i64],
        )
        .map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
        tx.commit()
            .map_err(|e| StoreError::new("commit_failed", e.to_string()))
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

    fn save_session_control(&self, session_id: &str, name: &str, value: &str) -> StoreResult<()> {
        let connection = self.connection()?;
        connection
            .execute(
                "INSERT INTO session_controls(session_id,name,value,updated_at)
                 VALUES (?1,?2,?3,?4)
                 ON CONFLICT(session_id,name) DO UPDATE
                 SET value=excluded.value, updated_at=excluded.updated_at",
                params![session_id, name, value, now_ms() as i64],
            )
            .map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
        Ok(())
    }

    fn list_session_controls(
        &self,
        session_id: &str,
    ) -> StoreResult<Vec<fleet_domain::RequestedControl>> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare("SELECT name, value FROM session_controls WHERE session_id=?1 ORDER BY name")
            .map_err(|e| StoreError::new("store_read_failed", e.to_string()))?;
        let rows = statement
            .query_map(params![session_id], |row| {
                Ok(fleet_domain::RequestedControl {
                    name: row.get(0)?,
                    value: row.get(1)?,
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
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| StoreError::new("transaction_failed", e.to_string()))?;
        append_event_tx(&transaction, event)?;
        transaction
            .commit()
            .map_err(|e| StoreError::new("commit_failed", e.to_string()))
    }

    fn commit_event(
        &self,
        event: &Event,
        session: &SessionRecord,
        approval: Option<&ApprovalRequest>,
    ) -> StoreResult<()> {
        let mut connection = self.connection()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| StoreError::new("transaction_failed", e.to_string()))?;
        commit_event_tx(&tx, event, session, approval)?;
        tx.commit()
            .map_err(|e| StoreError::new("commit_failed", e.to_string()))
    }

    fn commit_runner_event(
        &self,
        runner_id: &str,
        runner_seq: u64,
        event: &Event,
        session: &SessionRecord,
        approval: Option<&ApprovalRequest>,
    ) -> StoreResult<bool> {
        let mut connection = self.connection()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| StoreError::new("transaction_failed", e.to_string()))?;
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM runner_events WHERE runner_id=?1 AND runner_seq=?2)",
                params![runner_id, runner_seq as i64],
                |row| row.get(0),
            )
            .map_err(|e| StoreError::new("store_read_failed", e.to_string()))?;
        if exists {
            return Ok(false);
        }
        // The public timeline and the native event identity share one commit.
        // SIGKILL at any point before commit leaves neither side persisted.
        commit_event_tx(&tx, event, session, approval)?;
        tx.execute(
            "INSERT INTO runner_events(runner_id,runner_seq,fleet_event_id,created_at)
             VALUES(?1,?2,?3,?4)",
            params![
                runner_id,
                runner_seq as i64,
                event.event_id,
                now_ms() as i64
            ],
        )
        .map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
        tx.execute(
            "UPDATE runners SET last_ack_seq=MAX(last_ack_seq, ?2),updated_at=?3
             WHERE runner_id=?1",
            params![runner_id, runner_seq as i64, now_ms() as i64],
        )
        .map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
        tx.commit()
            .map_err(|e| StoreError::new("commit_failed", e.to_string()))?;
        Ok(true)
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
            .transaction_with_behavior(TransactionBehavior::Immediate)
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

    fn mark_session_approvals_uncertain(&self, session_id: &str) -> StoreResult<()> {
        let connection = self.connection()?;
        connection
            .execute(
                "UPDATE approvals SET status='uncertain'
             WHERE session_id=?1 AND status IN ('pending','delivered','dispatching')",
                params![session_id],
            )
            .map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
        Ok(())
    }

    fn active_turn_for_session(&self, session_id: &str) -> StoreResult<Option<TurnRecord>> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT turn_id, session_id, operation_id, state, created_at, updated_at
             FROM turns WHERE session_id=?1 AND state IN ('running','awaiting_approval','uncertain')
             ORDER BY created_at DESC LIMIT 1",
                params![session_id],
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

    fn claim_approval(
        &self,
        approval_id: &str,
        decision: &str,
        operation: &OperationRecord,
    ) -> StoreResult<ApprovalClaim> {
        let mut connection = self.connection()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| StoreError::new("transaction_failed", e.to_string()))?;
        if let Some(existing) = load_operation(&tx, &operation.operation_key)? {
            let current = tx
                .query_row(
                    "SELECT * FROM approvals WHERE approval_id=?1",
                    params![approval_id],
                    row_to_approval,
                )
                .optional()
                .map_err(|e| StoreError::new("store_read_failed", e.to_string()))?
                .ok_or_else(|| StoreError::new("not_found", "approval no longer exists"))?;
            if existing.fingerprint == operation.fingerprint
                && existing.kind == operation.kind
                && existing.session_id == operation.session_id
            {
                if existing.status == OperationStatus::Uncertain
                    || current.status == ApprovalStatus::Uncertain
                {
                    return Ok(ApprovalClaim::Uncertain(current));
                }
                return Ok(ApprovalClaim::Existing(existing, current));
            }
            return Ok(ApprovalClaim::Conflict);
        }
        let current: ApprovalRequest = tx
            .query_row(
                "SELECT * FROM approvals WHERE approval_id=?1",
                params![approval_id],
                row_to_approval,
            )
            .map_err(|e| StoreError::new("store_read_failed", e.to_string()))?;
        if current.status == ApprovalStatus::Uncertain {
            return Ok(ApprovalClaim::Uncertain(current));
        }
        let changed = tx
            .execute(
                "UPDATE approvals SET status='dispatching', decision=?2
             WHERE approval_id=?1 AND status IN ('pending','delivered')",
                params![approval_id, decision],
            )
            .map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
        if changed != 1 {
            return Ok(ApprovalClaim::Conflict);
        }
        tx.execute(
            "INSERT INTO operations(operation_id,operation_key,kind,session_id,target_id,
                fingerprint,status,message,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,'accepted',NULL,?7,?8)",
            params![
                operation.operation_id,
                operation.operation_key,
                operation_kind_text(operation.kind),
                operation.session_id,
                operation.target_id,
                operation.fingerprint,
                now_ms() as i64,
                now_ms() as i64
            ],
        )
        .map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
        let updated = tx
            .query_row(
                "SELECT * FROM approvals WHERE approval_id=?1",
                params![approval_id],
                row_to_approval,
            )
            .map_err(|e| StoreError::new("store_read_failed", e.to_string()))?;
        tx.commit()
            .map_err(|e| StoreError::new("commit_failed", e.to_string()))?;
        Ok(ApprovalClaim::Created(updated))
    }

    fn mark_approval_uncertain(&self, approval_id: &str, reason: &str) -> StoreResult<()> {
        let mut connection = self.connection()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| StoreError::new("transaction_failed", e.to_string()))?;
        let changed = tx.execute(
            "UPDATE approvals SET status='uncertain' WHERE approval_id=?1 AND status='dispatching'",
            params![approval_id],
        )
        .map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
        if changed != 1 {
            // A native confirmation may already have won. Report it so the
            // manager does not overwrite a final state with Uncertain.
            return Err(StoreError::new(
                "approval_already_resolved",
                format!("approval {approval_id} is no longer dispatching"),
            ));
        }
        tx.execute(
            "UPDATE operations SET status='uncertain', message=?2, updated_at=?3
             WHERE kind='answer_approval' AND target_id=?1 AND status='accepted'",
            params![approval_id, reason, now_ms() as i64],
        )
        .map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
        tx.commit()
            .map_err(|e| StoreError::new("commit_failed", e.to_string()))
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

    fn save_runner(&self, runner: &RunnerRecord) -> StoreResult<()> {
        let connection = self.connection()?;
        let identity = &runner.identity;
        connection.execute(
            "INSERT INTO runners(runner_id, session_id, provider_id, host_id, boot_id, pid,
                protocol_version, generation, status, endpoint, last_ack_seq, created_at, updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
             ON CONFLICT(runner_id) DO UPDATE SET status=excluded.status,
                endpoint=excluded.endpoint, boot_id=excluded.boot_id,
                generation=excluded.generation, updated_at=excluded.updated_at",
            params![
                identity.runner_id, identity.session_id, identity.provider_id, identity.host_id,
                identity.boot_id, identity.pid as i64, identity.protocol_version,
                identity.generation as i64, runner_status_text(runner.status), runner.endpoint,
                runner.last_ack_seq as i64, runner.created_at_ms as i64, runner.updated_at_ms as i64
            ],
        ).map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
        Ok(())
    }

    fn get_runner(&self, session_id: &str) -> StoreResult<Option<RunnerRecord>> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT * FROM runners WHERE session_id=?1",
                params![session_id],
                row_to_runner,
            )
            .optional()
            .map_err(|e| StoreError::new("store_read_failed", e.to_string()))
    }

    fn list_runners(&self) -> StoreResult<Vec<RunnerRecord>> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare("SELECT * FROM runners ORDER BY created_at")
            .map_err(|e| StoreError::new("store_read_failed", e.to_string()))?;
        let rows = statement
            .query_map([], row_to_runner)
            .map_err(|e| StoreError::new("store_read_failed", e.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| StoreError::new("store_read_failed", e.to_string()))
    }

    fn update_runner_status(&self, runner_id: &str, status: RunnerStatus) -> StoreResult<()> {
        let connection = self.connection()?;
        connection
            .execute(
                "UPDATE runners SET status=?2, updated_at=?3 WHERE runner_id=?1",
                params![runner_id, runner_status_text(status), now_ms() as i64],
            )
            .map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
        Ok(())
    }

    fn update_runner_ack(&self, runner_id: &str, runner_seq: u64) -> StoreResult<()> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE runners SET last_ack_seq=?2, updated_at=?3 WHERE runner_id=?1 AND last_ack_seq<?2",
            params![runner_id, runner_seq as i64, now_ms() as i64],
        ).map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
        Ok(())
    }

    fn runner_event_exists(&self, runner_id: &str, runner_seq: u64) -> StoreResult<bool> {
        let connection = self.connection()?;
        let found: Option<String> = connection
            .query_row(
                "SELECT fleet_event_id FROM runner_events WHERE runner_id=?1 AND runner_seq=?2",
                params![runner_id, runner_seq as i64],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| StoreError::new("store_read_failed", e.to_string()))?;
        Ok(found.is_some())
    }

    fn record_runner_event(
        &self,
        runner_id: &str,
        runner_seq: u64,
        fleet_event_id: &str,
    ) -> StoreResult<bool> {
        let mut connection = self.connection()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| StoreError::new("transaction_failed", e.to_string()))?;
        let changed = tx.execute(
            "INSERT OR IGNORE INTO runner_events(runner_id, runner_seq, fleet_event_id, created_at)
             VALUES(?1,?2,?3,?4)",
            params![runner_id, runner_seq as i64, fleet_event_id, now_ms() as i64],
        ).map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
        if changed == 1 {
            tx.execute(
                "UPDATE runners SET last_ack_seq=MAX(last_ack_seq,?2),updated_at=?3
                 WHERE runner_id=?1",
                params![runner_id, runner_seq as i64, now_ms() as i64],
            )
            .map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
        }
        tx.commit()
            .map_err(|e| StoreError::new("commit_failed", e.to_string()))?;
        Ok(changed == 1)
    }

    fn pending_runner_events(
        &self,
        runner_id: &str,
        since_seq: u64,
        limit: usize,
    ) -> StoreResult<Vec<(u64, String)>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT runner_seq, fleet_event_id FROM runner_events WHERE runner_id=?1 AND runner_seq>?2 ORDER BY runner_seq LIMIT ?3",
        ).map_err(|e| StoreError::new("store_read_failed", e.to_string()))?;
        let rows = statement
            .query_map(params![runner_id, since_seq as i64, limit as i64], |row| {
                Ok((row.get::<_, i64>(0)? as u64, row.get(1)?))
            })
            .map_err(|e| StoreError::new("store_read_failed", e.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| StoreError::new("store_read_failed", e.to_string()))
    }
}

/// Same transaction body for direct events and durable runner ingestion.
fn commit_event_tx(
    tx: &rusqlite::Transaction<'_>,
    event: &Event,
    session: &SessionRecord,
    approval: Option<&ApprovalRequest>,
) -> StoreResult<()> {
    // Redelivery of the same durable event is a no-op. Check BEFORE any
    // projection: re-running approval INSERT or ApprovalResolved UPDATE
    // would fail and poison a healthy manager with StorageUnavailable.
    let existing_event: Option<String> = tx
        .query_row(
            "SELECT event_id FROM events WHERE session_id=?1 AND seq=?2",
            params![event.session_id, event.seq as i64],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| StoreError::new("store_read_failed", e.to_string()))?;
    if let Some(event_id) = existing_event {
        if event_id == event.event_id {
            return Ok(()); // transaction drops without writes
        }
        return Err(StoreError::new(
            "sequence_conflict",
            format!("seq {} already used in {}", event.seq, event.session_id),
        ));
    }
    // The projections and the visible event have one durability boundary.
    if let Some(approval) = approval {
        tx.execute(
            "INSERT INTO approvals(approval_id, session_id, turn_id, kind, message, resources,
                    allowed_decisions, status, native_request_id, decision, created_at, resolved_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                approval.approval_id,
                approval.session_id,
                approval.turn_id,
                approval.kind,
                approval.message,
                serde_json::to_string(&approval.resources)
                    .map_err(|e| StoreError::new("serialization_failed", e.to_string()))?,
                serde_json::to_string(&approval.allowed_decisions)
                    .map_err(|e| StoreError::new("serialization_failed", e.to_string()))?,
                approval_text(approval.status),
                approval.native_request_id,
                approval.decision,
                approval.created_at_ms as i64,
                approval.resolved_at_ms.map(|x| x as i64),
            ],
        )
        .map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
    }
    // Session state is durable together with the event that announces it.
    tx.execute(
        "UPDATE sessions SET state=?2, turn_state=?3, updated_at=?4 WHERE session_id=?1",
        params![
            session.session_id,
            state_text(session.state),
            turn_text(session.turn_state),
            now_ms() as i64,
        ],
    )
    .map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
    match &event.kind {
        fleet_domain::EventKind::TurnCompleted { turn_id, status } => {
            let state = match status {
                fleet_domain::TurnStatus::Completed => TurnState::Completed,
                fleet_domain::TurnStatus::Failed => TurnState::Failed,
                fleet_domain::TurnStatus::Interrupted => TurnState::Interrupted,
            };
            tx.execute(
                "UPDATE turns SET state=?2, updated_at=?3 WHERE turn_id=?1",
                params![turn_id, turn_text(state), now_ms() as i64],
            )
            .map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
            let operation_status = if state == TurnState::Completed {
                OperationStatus::Completed
            } else {
                OperationStatus::Failed
            };
            tx.execute(
                "UPDATE operations SET status=?2, updated_at=?3 WHERE operation_id IN
                        (SELECT operation_id FROM turns WHERE turn_id=?1)",
                params![turn_id, operation_text(operation_status), now_ms() as i64],
            )
            .map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
        }
        fleet_domain::EventKind::TurnUncertain { turn_id, reason } => {
            tx.execute(
                "UPDATE turns SET state='uncertain', updated_at=?2 WHERE turn_id=?1",
                params![turn_id, now_ms() as i64],
            )
            .map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
            tx.execute(
                "UPDATE operations SET status='uncertain', message=?2, updated_at=?3
                     WHERE operation_id IN (SELECT operation_id FROM turns WHERE turn_id=?1)",
                params![turn_id, reason, now_ms() as i64],
            )
            .map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
        }
        fleet_domain::EventKind::ApprovalRequested { turn_id, .. } => {
            tx.execute(
                "UPDATE turns SET state='awaiting_approval', updated_at=?2
                     WHERE turn_id=?1",
                params![turn_id, now_ms() as i64],
            )
            .map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
        }
        fleet_domain::EventKind::ApprovalResolved {
            approval_id,
            decision,
            ..
        } => {
            let status = if decision == "reject" {
                "rejected"
            } else {
                "accepted"
            };
            let changed = tx
                .execute(
                    "UPDATE approvals SET status=?2, decision=?3, resolved_at=?4
                     WHERE approval_id=?1 AND status='dispatching' AND decision=?3",
                    params![approval_id, status, decision, now_ms() as i64],
                )
                .map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
            if changed != 1 {
                return Err(StoreError::new(
                    "approval_conflict",
                    "approval is not awaiting native delivery confirmation",
                ));
            }
            tx.execute(
                "UPDATE operations SET status='completed', updated_at=?2
                     WHERE kind='answer_approval' AND target_id=?1 AND status='accepted'",
                params![approval_id, now_ms() as i64],
            )
            .map_err(|e| StoreError::new("store_write_failed", e.to_string()))?;
        }
        _ => {}
    }
    append_event_tx(tx, event)?;
    Ok(())
}

fn runner_status_text(status: RunnerStatus) -> &'static str {
    match status {
        RunnerStatus::Starting => "starting",
        RunnerStatus::Active => "active",
        RunnerStatus::Disconnected => "disconnected",
        RunnerStatus::Recovering => "recovering",
        RunnerStatus::Stopping => "stopping",
        RunnerStatus::StopUnconfirmed => "stop_unconfirmed",
        RunnerStatus::Stopped => "stopped",
        RunnerStatus::Lost => "lost",
        RunnerStatus::Uncertain => "uncertain",
    }
}

fn parse_runner_status(value: &str) -> RunnerStatus {
    match value {
        "starting" => RunnerStatus::Starting,
        "active" => RunnerStatus::Active,
        "disconnected" => RunnerStatus::Disconnected,
        "recovering" => RunnerStatus::Recovering,
        "stopping" => RunnerStatus::Stopping,
        "stop_unconfirmed" => RunnerStatus::StopUnconfirmed,
        "stopped" => RunnerStatus::Stopped,
        "lost" => RunnerStatus::Lost,
        _ => RunnerStatus::Uncertain,
    }
}

fn row_to_runner(row: &rusqlite::Row<'_>) -> Result<RunnerRecord, rusqlite::Error> {
    Ok(RunnerRecord {
        identity: RunnerIdentity {
            runner_id: row.get(0)?,
            session_id: row.get(1)?,
            provider_id: row.get(2)?,
            host_id: row.get(3)?,
            boot_id: row.get(4)?,
            pid: row.get::<_, i64>(5)? as u32,
            protocol_version: row.get(6)?,
            generation: row.get::<_, i64>(7)? as u64,
        },
        status: parse_runner_status(&row.get::<_, String>(8)?),
        endpoint: row.get(9)?,
        last_ack_seq: row.get::<_, i64>(10)? as u64,
        created_at_ms: row.get::<_, i64>(11)? as u64,
        updated_at_ms: row.get::<_, i64>(12)? as u64,
    })
}

fn operation_kind_text(kind: fleet_domain::store::OperationKind) -> &'static str {
    match kind {
        fleet_domain::store::OperationKind::SendPrompt => "send_prompt",
        fleet_domain::store::OperationKind::AnswerApproval => "answer_approval",
        fleet_domain::store::OperationKind::ImportHistory => "import_history",
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
                    "import_history" => fleet_domain::store::OperationKind::ImportHistory,
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
        let path = std::env::temp_dir()
            .join(format!(
                "gf-p2-store-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::SeqCst)
            ))
            .join("fleet.sqlite");
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
    fn runner_event_and_dedup_are_one_atomic_commit() {
        let (store, _) = temp_store();
        let workspace = WorkspaceRecord {
            id: "ws_1".into(),
            path: "/tmp/demo".into(),
        };
        store.save_workspace(&workspace).unwrap();
        let session = SessionRecord {
            session_id: "ses_1".into(),
            workspace_id: workspace.id,
            provider_id: "fake".into(),
            state: SessionState::Running,
            turn_state: TurnState::Idle,
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        store.save_session(&session).unwrap();
        let runner = RunnerRecord {
            identity: RunnerIdentity {
                runner_id: "runner_1".into(),
                session_id: session.session_id.clone(),
                provider_id: "fake".into(),
                host_id: "local".into(),
                boot_id: "boot_1".into(),
                pid: 123,
                protocol_version: "fleet-runner-v1".into(),
                generation: 1,
            },
            status: RunnerStatus::Active,
            endpoint: "test".into(),
            last_ack_seq: 0,
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        store.save_runner(&runner).unwrap();
        let event = Event {
            event_id: "ses_1-e1".into(),
            session_id: session.session_id.clone(),
            seq: 1,
            created_at_ms: 1,
            kind: EventKind::AgentNotice {
                message: "hello".into(),
            },
        };
        assert!(store
            .commit_runner_event("runner_1", 1, &event, &session, None)
            .unwrap());
        assert!(!store
            .commit_runner_event("runner_1", 1, &event, &session, None)
            .unwrap());
        assert_eq!(store.read_events("ses_1", 0, 100).unwrap().len(), 1);
        assert!(store.runner_event_exists("runner_1", 1).unwrap());
        assert_eq!(store.get_runner("ses_1").unwrap().unwrap().last_ack_seq, 1);

        // Conflict rolls back BOTH the event and dedup marker.
        let conflicting = Event {
            event_id: "ses_1-other".into(),
            ..event
        };
        let error = store
            .commit_runner_event("runner_1", 2, &conflicting, &session, None)
            .unwrap_err();
        assert_eq!(error.code, "sequence_conflict");
        assert!(!store.runner_event_exists("runner_1", 2).unwrap());
        assert_eq!(store.read_events("ses_1", 0, 100).unwrap().len(), 1);
        assert_eq!(store.get_runner("ses_1").unwrap().unwrap().last_ack_seq, 1);
    }

    #[test]
    fn foreign_keys_are_enforced_on_each_new_connection() {
        let (store, _path) = temp_store();
        let record = SessionRecord {
            session_id: "invalid-session".into(),
            workspace_id: "missing-workspace".into(),
            provider_id: "fake".into(),
            state: SessionState::Running,
            turn_state: TurnState::Idle,
            created_at_ms: now_ms(),
            updated_at_ms: now_ms(),
        };
        let error = store.save_session(&record).unwrap_err();
        assert_eq!(error.code, "store_write_failed");
    }

    #[test]
    fn migration_and_health_are_verified() {
        let (store, path) = temp_store();
        let health = store.health().unwrap();
        assert_eq!(health.schema_version, 4);
        assert!(health.integrity_ok);
        // Reabrir no vuelve a ejecutar la migración.
        drop(store);
        let reopened = SqliteStore::open(path).unwrap();
        assert_eq!(reopened.health().unwrap().schema_version, 4);
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
        store
            .save_workspace(&WorkspaceRecord {
                id: "ws_1".into(),
                path: "/tmp/workspace".into(),
            })
            .unwrap();
        store
            .save_session(&SessionRecord {
                session_id: "ses_1".into(),
                workspace_id: "ws_1".into(),
                provider_id: "fake".into(),
                state: SessionState::Running,
                turn_state: TurnState::Running,
                created_at_ms: 1,
                updated_at_ms: 1,
            })
            .unwrap();
        store
            .save_turn(&TurnRecord {
                turn_id: "turn_1".into(),
                session_id: "ses_1".into(),
                operation_id: "op_1".into(),
                state: TurnState::Running,
                created_at_ms: 1,
                updated_at_ms: 1,
            })
            .unwrap();
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

    fn seed_session(store: &SqliteStore) {
        store
            .save_workspace(&WorkspaceRecord {
                id: "ws_1".into(),
                path: "/tmp/workspace".into(),
            })
            .unwrap();
        store
            .save_session(&SessionRecord {
                session_id: "ses_1".into(),
                workspace_id: "ws_1".into(),
                provider_id: "fake".into(),
                state: SessionState::Running,
                turn_state: TurnState::Running,
                created_at_ms: 1,
                updated_at_ms: 1,
            })
            .unwrap();
        store
            .save_turn(&TurnRecord {
                turn_id: "turn_1".into(),
                session_id: "ses_1".into(),
                operation_id: "op_1".into(),
                state: TurnState::Running,
                created_at_ms: 1,
                updated_at_ms: 1,
            })
            .unwrap();
    }

    #[test]
    fn commit_event_redelivery_is_idempotent() {
        let (store, _) = temp_store();
        seed_session(&store);
        let approval = ApprovalRequest {
            approval_id: "appr_1".into(),
            session_id: "ses_1".into(),
            turn_id: "turn_1".into(),
            kind: "shell".into(),
            message: "allow?".into(),
            resources: vec!["echo hi".into()],
            allowed_decisions: vec!["once".into()],
            status: ApprovalStatus::Pending,
            native_request_id: "per_native".into(),
            decision: None,
            created_at_ms: 1,
            resolved_at_ms: None,
        };
        let event = Event {
            event_id: "ses_1-e3".into(),
            session_id: "ses_1".into(),
            seq: 3,
            created_at_ms: 1,
            kind: EventKind::ApprovalRequested {
                approval_id: "appr_1".into(),
                turn_id: "turn_1".into(),
                kind: "shell".into(),
                message: "allow?".into(),
                resources: vec!["echo hi".into()],
                allowed_decisions: vec!["once".into()],
                options: vec![],
            },
        };
        let projection = SessionRecord {
            session_id: "ses_1".into(),
            workspace_id: "ws_1".into(),
            provider_id: "fake".into(),
            state: SessionState::Running,
            turn_state: TurnState::AwaitingApproval,
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        store
            .commit_event(&event, &projection, Some(&approval))
            .unwrap();
        store
            .commit_event(&event, &projection, Some(&approval))
            .unwrap();
        assert_eq!(store.read_events("ses_1", 0, 10).unwrap().len(), 1);
        assert!(store.get_approval("appr_1").unwrap().is_some());
        let mut conflicting = event.clone();
        conflicting.event_id = "ses_1-other".into();
        let error = store
            .commit_event(&conflicting, &projection, None)
            .unwrap_err();
        assert_eq!(error.code, "sequence_conflict");
    }

    #[test]
    fn approvals_require_existing_session_and_turn() {
        let (store, _) = temp_store();
        let approval = ApprovalRequest {
            approval_id: "appr_orphan".into(),
            session_id: "ghost_session".into(),
            turn_id: "ghost_turn".into(),
            kind: "shell".into(),
            message: "allow?".into(),
            resources: vec![],
            allowed_decisions: vec!["once".into()],
            status: ApprovalStatus::Pending,
            native_request_id: "per_x".into(),
            decision: None,
            created_at_ms: 1,
            resolved_at_ms: None,
        };
        let error = store.save_approval(&approval).unwrap_err();
        assert_eq!(error.code, "store_write_failed");
        assert!(store.get_approval("appr_orphan").unwrap().is_none());
    }

    #[test]
    fn concurrent_reservation_is_idempotent() {
        let (first, path) = temp_store();
        let second = SqliteStore::open(path).unwrap();
        let operation = OperationRecord {
            operation_id: "op_race".into(),
            operation_key: "race-key".into(),
            kind: OperationKind::SendPrompt,
            session_id: "ses_1".into(),
            target_id: Some("turn_1".into()),
            fingerprint: "ses_1\0hello".into(),
            status: OperationStatus::Reserved,
            message: None,
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        let for_a = operation.clone();
        let for_b = operation;
        let a = std::thread::spawn(move || first.reserve_operation(&for_a).unwrap());
        let b = std::thread::spawn(move || second.reserve_operation(&for_b).unwrap());
        let results = [a.join().unwrap(), b.join().unwrap()];
        assert_eq!(
            results
                .iter()
                .filter(|r| matches!(r, OperationReservation::Created(_)))
                .count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|r| matches!(r, OperationReservation::Existing(..)))
                .count(),
            1
        );
    }

    #[test]
    fn uncertain_after_native_confirmation_is_rejected() {
        let (store, _) = temp_store();
        seed_session(&store);
        let approval = ApprovalRequest {
            approval_id: "appr_1".into(),
            session_id: "ses_1".into(),
            turn_id: "turn_1".into(),
            kind: "shell".into(),
            message: "allow?".into(),
            resources: vec![],
            allowed_decisions: vec!["once".into()],
            status: ApprovalStatus::Pending,
            native_request_id: "per_native".into(),
            decision: None,
            created_at_ms: 1,
            resolved_at_ms: None,
        };
        store.save_approval(&approval).unwrap();
        store
            .resolve_approval("appr_1", "once", ApprovalStatus::Accepted)
            .unwrap();
        let error = store
            .mark_approval_uncertain("appr_1", "late failure")
            .unwrap_err();
        assert_eq!(error.code, "approval_already_resolved");
        assert_eq!(
            store.get_approval("appr_1").unwrap().unwrap().status,
            ApprovalStatus::Accepted
        );
    }

    #[test]
    fn runner_identity_ack_and_event_dedup_are_durable() {
        let (store, path) = temp_store();
        seed_session(&store);
        let identity = fleet_domain::RunnerIdentity {
            runner_id: "runner-1".into(),
            session_id: "ses_1".into(),
            provider_id: "fake".into(),
            host_id: "host-1".into(),
            boot_id: "boot-1".into(),
            pid: 42,
            protocol_version: "fleet-runner-v1".into(),
            generation: 1,
        };
        store
            .save_runner(&RunnerRecord {
                identity: identity.clone(),
                status: RunnerStatus::Active,
                endpoint: "/tmp/runner.sock".into(),
                last_ack_seq: 0,
                created_at_ms: 1,
                updated_at_ms: 1,
            })
            .unwrap();
        store
            .record_runner_event("runner-1", 1, "ses_1-e3")
            .unwrap();
        assert!(store.runner_event_exists("runner-1", 1).unwrap());
        assert!(!store
            .record_runner_event("runner-1", 1, "ses_1-e3")
            .unwrap());
        assert_eq!(store.get_runner("ses_1").unwrap().unwrap().last_ack_seq, 1);
        drop(store);
        let reopened = SqliteStore::open(path).unwrap();
        assert_eq!(
            reopened
                .get_runner("ses_1")
                .unwrap()
                .unwrap()
                .identity
                .boot_id,
            "boot-1"
        );
        assert_eq!(
            reopened
                .pending_runner_events("runner-1", 1, 10)
                .unwrap()
                .len(),
            0
        );
    }
    #[test]
    fn deferred_session_controls_survive_reopen() {
        let (store, path) = temp_store();
        let session = SessionRecord {
            session_id: "session-controls".into(),
            workspace_id: "ws-controls".into(),
            provider_id: "codex".into(),
            state: SessionState::Running,
            turn_state: TurnState::Idle,
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        store.save_workspace(&WorkspaceRecord {
            workspace_id: "ws-controls".into(),
            path: "/tmp/test-controls".into(),
        }).unwrap();
        store.save_session(&session).unwrap();
        store.save_session_control("session-controls", "effort", "high").unwrap();
        store.save_session_control("session-controls", "effort", "medium").unwrap();
        drop(store);
        let store = SqliteStore::open(path).unwrap();
        let controls = store.list_session_controls("session-controls").unwrap();
        assert_eq!(controls.len(), 1);
        assert_eq!(controls[0].name, "effort");
        assert_eq!(controls[0].value, "medium");
    }

}

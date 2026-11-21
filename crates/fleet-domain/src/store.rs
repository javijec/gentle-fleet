//! Interfaz de almacenamiento pequeña. El dominio no conoce SQL.

use serde::{Deserialize, Serialize};

use crate::{
    Event, OperationId, ProviderId, RunnerIdentity, RunnerStatus, SessionId, SessionState, TurnId,
    TurnState, WorkspaceId,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreError {
    pub code: String,
    pub message: String,
}

impl StoreError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

pub type StoreResult<T> = Result<T, StoreError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerRecord {
    pub identity: RunnerIdentity,
    pub status: RunnerStatus,
    pub endpoint: String,
    pub last_ack_seq: u64,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreHealth {
    pub schema_version: u32,
    pub integrity_ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRecord {
    pub id: WorkspaceId,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub session_id: SessionId,
    pub workspace_id: WorkspaceId,
    pub provider_id: ProviderId,
    pub state: SessionState,
    pub turn_state: TurnState,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationKind {
    SendPrompt,
    AnswerApproval,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationStatus {
    Reserved,
    Accepted,
    Completed,
    Failed,
    Uncertain,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationRecord {
    pub operation_id: OperationId,
    pub operation_key: String,
    pub kind: OperationKind,
    pub session_id: SessionId,
    pub target_id: Option<String>,
    pub fingerprint: String,
    pub status: OperationStatus,
    pub message: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationReservation {
    Created(OperationRecord),
    Existing(OperationRecord),
    Conflict,
}

/// A decision claim includes the idempotency reservation and approval status
/// in ONE transaction. An existing key must never be dispatched twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalClaim {
    Created(ApprovalRequest),
    Existing(OperationRecord, ApprovalRequest),
    /// La decisión anterior quedó incierta: requiere reconciliación, no reintento.
    Uncertain(ApprovalRequest),
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnRecord {
    pub turn_id: TurnId,
    pub session_id: SessionId,
    pub operation_id: OperationId,
    pub state: TurnState,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalStatus {
    Pending,
    Delivered,
    /// Decision durably claimed but not yet confirmed by the native provider.
    Dispatching,
    Accepted,
    Rejected,
    Uncertain,
}

impl ApprovalRequest {
    pub fn status_text(&self) -> &'static str {
        match self.status {
            ApprovalStatus::Pending => "pending",
            ApprovalStatus::Delivered => "delivered",
            ApprovalStatus::Dispatching => "dispatching",
            ApprovalStatus::Accepted => "accepted",
            ApprovalStatus::Rejected => "rejected",
            ApprovalStatus::Uncertain => "uncertain",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub approval_id: String,
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub kind: String,
    pub message: String,
    pub resources: Vec<String>,
    pub allowed_decisions: Vec<String>,
    pub status: ApprovalStatus,
    /// Identificador nativo opaco; SQLite lo conserva, la API nunca lo expone.
    #[serde(skip)]
    pub native_request_id: String,
    pub decision: Option<String>,
    pub created_at_ms: u64,
    pub resolved_at_ms: Option<u64>,
}

/// Autoridad de persistencia. Una sola implementación real (SQLite) y
/// implementaciones falsas deterministas en tests.
pub trait SessionStore: Send + Sync {
    fn init(&self) -> StoreResult<()>;
    fn health(&self) -> StoreResult<StoreHealth>;
    /// Reclassify in-flight effects as uncertain after daemon restart.
    /// Never redeliver an operation as a side effect of loading the DB.
    fn recover_after_restart(&self) -> StoreResult<()>;

    fn save_workspace(&self, workspace: &WorkspaceRecord) -> StoreResult<()>;
    fn list_workspaces(&self) -> StoreResult<Vec<WorkspaceRecord>>;

    fn save_session(&self, session: &SessionRecord) -> StoreResult<()>;
    fn get_session(&self, session_id: &str) -> StoreResult<Option<SessionRecord>>;
    fn list_sessions(&self) -> StoreResult<Vec<SessionRecord>>;

    fn max_seq(&self, session_id: &str) -> StoreResult<u64>;
    fn append_event(&self, event: &Event) -> StoreResult<()>;
    /// Commit an event and its session/turn/operation/approval projection atomically.
    /// The caller publishes to clients ONLY after this succeeds.
    fn commit_event(
        &self,
        event: &Event,
        session: &SessionRecord,
        approval: Option<&ApprovalRequest>,
    ) -> StoreResult<()>;
    /// Atomically commit the public event + projections + runner dedup key.
    /// false means a previously committed runner sequence was redelivered.
    fn commit_runner_event(
        &self,
        runner_id: &str,
        runner_seq: u64,
        event: &Event,
        session: &SessionRecord,
        approval: Option<&ApprovalRequest>,
    ) -> StoreResult<bool>;
    fn read_events(
        &self,
        session_id: &str,
        since_seq: u64,
        limit: usize,
    ) -> StoreResult<Vec<Event>>;

    fn reserve_operation(&self, operation: &OperationRecord) -> StoreResult<OperationReservation>;
    fn get_operation(&self, operation_key: &str) -> StoreResult<Option<OperationRecord>>;
    fn update_operation(
        &self,
        operation_id: &str,
        status: OperationStatus,
        message: Option<String>,
    ) -> StoreResult<()>;

    fn save_turn(&self, turn: &TurnRecord) -> StoreResult<()>;
    fn get_turn(&self, turn_id: &str) -> StoreResult<Option<TurnRecord>>;
    /// Turno no terminal más reciente de una sesión, para reattachment.
    fn active_turn_for_session(&self, session_id: &str) -> StoreResult<Option<TurnRecord>>;
    fn mark_session_approvals_uncertain(&self, session_id: &str) -> StoreResult<()>;

    fn save_approval(&self, approval: &ApprovalRequest) -> StoreResult<()>;
    fn claim_approval(
        &self,
        approval_id: &str,
        decision: &str,
        operation: &OperationRecord,
    ) -> StoreResult<ApprovalClaim>;
    fn mark_approval_uncertain(&self, approval_id: &str, reason: &str) -> StoreResult<()>;
    fn get_approval(&self, approval_id: &str) -> StoreResult<Option<ApprovalRequest>>;
    fn list_approvals(&self, session_id: Option<&str>) -> StoreResult<Vec<ApprovalRequest>>;
    /// Devuelve `true` solo para la primera decisión ganadora.
    fn resolve_approval(
        &self,
        approval_id: &str,
        decision: &str,
        status: ApprovalStatus,
    ) -> StoreResult<bool>;
    fn update_approval_status(&self, approval_id: &str, status: ApprovalStatus) -> StoreResult<()>;

    // ---- P3: runner identity, ACK and durable spool evidence ----------------
    fn save_runner(&self, runner: &RunnerRecord) -> StoreResult<()>;
    fn get_runner(&self, session_id: &str) -> StoreResult<Option<RunnerRecord>>;
    fn list_runners(&self) -> StoreResult<Vec<RunnerRecord>>;
    fn update_runner_status(&self, runner_id: &str, status: RunnerStatus) -> StoreResult<()>;
    fn update_runner_ack(&self, runner_id: &str, runner_seq: u64) -> StoreResult<()>;
    /// `true` if this `(runner_id, runner_seq)` was already ingested.
    fn runner_event_exists(&self, runner_id: &str, runner_seq: u64) -> StoreResult<bool>;
    /// Records that a runner event was ingested. Returns `false` on duplicate.
    fn record_runner_event(
        &self,
        runner_id: &str,
        runner_seq: u64,
        fleet_event_id: &str,
    ) -> StoreResult<bool>;
    fn pending_runner_events(
        &self,
        runner_id: &str,
        since_seq: u64,
        limit: usize,
    ) -> StoreResult<Vec<(u64, String)>>;
}

//! Interfaz de almacenamiento pequeña. El dominio no conoce SQL.

use serde::{Deserialize, Serialize};

use crate::{
    Event, OperationId, ProviderId, SessionId, SessionState, TurnId, TurnState, WorkspaceId,
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
    Accepted,
    Rejected,
    Uncertain,
}

impl ApprovalRequest {
    pub fn status_text(&self) -> &'static str {
        match self.status {
            ApprovalStatus::Pending => "pending",
            ApprovalStatus::Delivered => "delivered",
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
    /// Identificador nativo opaco; se guarda pero nunca se publica al cliente.
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

    fn save_workspace(&self, workspace: &WorkspaceRecord) -> StoreResult<()>;
    fn list_workspaces(&self) -> StoreResult<Vec<WorkspaceRecord>>;

    fn save_session(&self, session: &SessionRecord) -> StoreResult<()>;
    fn get_session(&self, session_id: &str) -> StoreResult<Option<SessionRecord>>;
    fn list_sessions(&self) -> StoreResult<Vec<SessionRecord>>;

    fn max_seq(&self, session_id: &str) -> StoreResult<u64>;
    fn append_event(&self, event: &Event) -> StoreResult<()>;
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

    fn save_approval(&self, approval: &ApprovalRequest) -> StoreResult<()>;
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
}

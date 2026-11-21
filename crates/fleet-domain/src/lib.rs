//! Tipos de dominio de Fleet. Sin I/O, sin proveedores y sin persistencia.

use serde::{Deserialize, Serialize};

pub mod store;

pub type ProviderId = String;
pub type WorkspaceId = String;
pub type SessionId = String;
pub type TurnId = String;
pub type OperationId = String;
pub type RunnerId = String;
pub type HostId = String;

/// Versión nativa de un proveedor, conservada como texto y como números.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderVersion {
    pub text: String,
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl ProviderVersion {
    pub fn parse(text: impl Into<String>) -> Self {
        let text = text.into();
        let mut numbers = [0u32; 3];
        for (slot, part) in numbers.iter_mut().zip(text.split('.').take(3)) {
            let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
            *slot = digits.parse().unwrap_or(0);
        }
        Self {
            text,
            major: numbers[0],
            minor: numbers[1],
            patch: numbers[2],
        }
    }
}

/// Rango de compatibilidad: `>= min`, `< max_exclusive`.
///
/// Permite actualizaciones compatibles dentro de la misma serie mayor sin
/// exigir una nueva fixture por cada versión patch o minor (P0-D5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionRange {
    pub min_major: u32,
    pub min_minor: u32,
    pub max_major_exclusive: u32,
}

impl VersionRange {
    pub fn compatible(&self, version: &ProviderVersion) -> bool {
        version.major >= self.min_major
            && (version.major > self.min_major || version.minor >= self.min_minor)
            && version.major < self.max_major_exclusive
    }
}

/// Modalidad de propiedad del proceso del agente (P0-D3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunnerMode {
    /// El Runner levanta y posee el proceso.
    Owned,
    /// El Runner se adjunta a un servidor existente del usuario.
    Attached,
    /// El driver resuelve: attach si existe servicio, si no, levanta proceso.
    Auto,
}

/// Capacidades declaradas por versión del proveedor (P0-D2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCapabilities {
    pub streaming_text: bool,
    pub tool_events: bool,
    pub session_history: bool,
    pub native_turn_ids: bool,
    pub cancel_turn: bool,
    pub approvals: bool,
    pub can_own_process: bool,
    pub can_attach_service: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderDescriptor {
    pub id: ProviderId,
    pub version: ProviderVersion,
    pub capabilities: ProviderCapabilities,
    pub mode: RunnerMode,
    pub availability: ProviderAvailability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState {
    Running,
    Stopping,
    StopUnconfirmed,
    Stopped,
    Failed,
    /// Recuperada de SQLite sin runner verificado tras reinicio (P2).
    NeedsReconciliation,
    /// Runner verificado y proveedor activo tras reattachment (P3).
    Recovering,
}

/// Estado del turno, propiedad del SessionManager.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnState {
    Idle,
    Running,
    AwaitingApproval,
    Completed,
    Failed,
    Interrupted,
    Uncertain,
}

impl TurnState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TurnState::Completed | TurnState::Failed | TurnState::Interrupted
        )
    }

    pub fn accepts_prompt(self) -> bool {
        self != TurnState::Running
            && self != TurnState::AwaitingApproval
            && self != TurnState::Uncertain
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderAvailability {
    Available,
    Unavailable,
    Unknown,
}

/// Estado operativo del runner, distinto del estado lógico de la sesión.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunnerStatus {
    Starting,
    Active,
    Disconnected,
    Recovering,
    Stopping,
    Stopped,
    Lost,
    Uncertain,
}

/// Identidad estable de arranque. El PID es informativo y nunca suficiente.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerIdentity {
    pub runner_id: RunnerId,
    pub session_id: SessionId,
    pub provider_id: ProviderId,
    pub host_id: HostId,
    pub boot_id: String,
    pub pid: u32,
    pub protocol_version: String,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnStatus {
    Completed,
    Failed,
    Interrupted,
}

// Eventos nativos traducidos por los drivers. Son datos puros: el protocolo
// daemon-runner los serializa sin depender de `fleet-agent`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEvent {
    pub kind: DomainEvent,
}

/// Evento numerado por el runner. `runner_seq` NO es el `seq` público Fleet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerEvent {
    pub runner_id: RunnerId,
    pub runner_seq: u64,
    pub event: AgentEvent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DomainEvent {
    TurnStarted {
        turn_id: TurnId,
    },
    AssistantDelta {
        turn_id: TurnId,
        text: String,
    },
    AssistantMessage {
        turn_id: TurnId,
        text: String,
    },
    ToolStarted {
        turn_id: TurnId,
        name: String,
        input: String,
    },
    ToolOutput {
        turn_id: TurnId,
        name: String,
        output: String,
    },
    ToolCompleted {
        turn_id: TurnId,
        name: String,
        success: bool,
    },
    TurnCompleted {
        turn_id: TurnId,
        status: TurnStatus,
    },
    /// Error terminal del turno; el único `AgentError` que cierra un turno.
    TurnFailed {
        turn_id: TurnId,
        message: String,
    },
    /// Información del proveedor que debe mostrarse sin cerrar el turno.
    AgentNotice {
        message: String,
    },
    /// Error informativo que NO cierra el turno.
    AgentError {
        message: String,
    },
    /// Solicitud interactiva nativa traducida a dominio por el driver.
    ApprovalRequested {
        native_request_id: String,
        kind: String,
        message: String,
        resources: Vec<String>,
        allowed_decisions: Vec<String>,
    },
    /// Confirmación nativa de que la decisión fue entregada.
    ApprovalResolved {
        native_request_id: String,
        decision: String,
    },
}

/// Evento público y tipado. `seq` lo asigna Fleet, nunca el proveedor (P0-D6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub event_id: String,
    pub session_id: SessionId,
    pub seq: u64,
    pub created_at_ms: u64,
    pub kind: EventKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventKind {
    SessionCreated {
        provider_id: ProviderId,
        workspace_id: WorkspaceId,
    },
    SessionStateChanged {
        state: SessionState,
    },
    TurnStarted {
        turn_id: TurnId,
    },
    UserMessage {
        turn_id: TurnId,
        text: String,
    },
    AssistantDelta {
        turn_id: TurnId,
        text: String,
    },
    AssistantMessage {
        turn_id: TurnId,
        text: String,
    },
    ToolStarted {
        turn_id: TurnId,
        name: String,
        input: String,
    },
    ToolOutput {
        turn_id: TurnId,
        name: String,
        output: String,
    },
    ToolCompleted {
        turn_id: TurnId,
        name: String,
        success: bool,
    },
    TurnCompleted {
        turn_id: TurnId,
        status: TurnStatus,
    },
    /// Información del proveedor que NO cierra el turno.
    AgentNotice {
        message: String,
    },
    /// Error informativo que NO cierra el turno.
    AgentError {
        message: String,
    },
    /// El turno quedó sin evidencia suficiente; no se reenvía.
    TurnUncertain {
        turn_id: TurnId,
        reason: String,
    },
    /// Un suscriptor lento fue desconectado con límite explícito.
    Backpressure {
        limit: usize,
    },
    ApprovalRequested {
        approval_id: String,
        turn_id: TurnId,
        kind: String,
        message: String,
        resources: Vec<String>,
        allowed_decisions: Vec<String>,
    },
    ApprovalResolved {
        approval_id: String,
        turn_id: TurnId,
        decision: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_range_allows_compatible_updates() {
        let range = VersionRange {
            min_major: 2,
            min_minor: 3,
            max_major_exclusive: 3,
        };
        assert!(range.compatible(&ProviderVersion::parse("2.3.2")));
        assert!(range.compatible(&ProviderVersion::parse("2.9.0")));
        assert!(!range.compatible(&ProviderVersion::parse("2.2.9")));
        assert!(!range.compatible(&ProviderVersion::parse("3.0.0")));
    }
}

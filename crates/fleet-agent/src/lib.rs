//! Contrato `AgentDriver` y registro de proveedores.
//!
//! El SessionManager solo conoce este módulo: nunca importa un driver concreto.
//! Los identificadores nativos se conservan dentro del driver y jamás aparecen
//! en `AgentEvent` ni en el wire Fleet.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

pub mod fake;

use fleet_domain::{
    ProviderDescriptor, ProviderId, ProviderVersion, RunnerMode, SessionState, TurnId, TurnStatus,
    VersionRange,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentError {
    pub code: String,
    pub message: String,
}

impl AgentError {
    pub fn unsupported(what: &str) -> Self {
        Self {
            code: "unsupported_capability".into(),
            message: format!("{what} is not supported by this driver version"),
        }
    }
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchContext {
    pub workspace: PathBuf,
    pub mode: RunnerMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentInput {
    pub text: String,
    pub turn_id: TurnId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendReceipt {
    pub accepted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryEvidence {
    pub session_alive: bool,
    pub detail: String,
}

/// Evento de dominio emitido por un driver, sin `seq` ni IDs Fleet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentEvent {
    pub kind: DomainEvent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
    AgentError {
        message: String,
    },
}

/// Contrato mínimo de P1. Las operaciones no soportadas devuelven
/// `AgentError::unsupported`, nunca un fallo silencioso.
pub trait AgentDriver: Send {
    fn descriptor(&self) -> &ProviderDescriptor;

    fn open(&mut self, ctx: LaunchContext) -> Result<String, AgentError>;

    fn send(&mut self, input: AgentInput) -> Result<SendReceipt, AgentError>;

    /// Devuelve `None` cuando no hay eventos listos dentro del timeout.
    fn next_event(&mut self, timeout: Duration) -> Result<Option<AgentEvent>, AgentError>;

    fn close(&mut self) -> Result<(), AgentError>;

    fn inspect_recovery(&self) -> RecoveryEvidence;

    fn answer(&mut self, _request_id: String, _decision: String) -> Result<(), AgentError> {
        Err(AgentError::unsupported("interactive approvals (P2)"))
    }

    fn interrupt(&mut self, _turn_id: TurnId) -> Result<(), AgentError> {
        Err(AgentError::unsupported("cancel_turn (P2/P4)"))
    }

    fn session_state(&self) -> SessionState {
        SessionState::Running
    }
}

/// Fábrica registrada por proveedor. `compatibility` aplica el rango de
/// versiones sin bloquear actualizaciones compatibles (P0-D5).
pub trait DriverFactory: Send + Sync {
    fn id(&self) -> ProviderId;
    fn compatibility(&self) -> VersionRange;
    fn create(&self) -> Box<dyn AgentDriver>;

    fn supports(&self, version: &ProviderVersion) -> bool {
        self.compatibility().compatible(version)
    }
}

#[derive(Default)]
pub struct ProviderRegistry {
    factories: HashMap<ProviderId, Box<dyn DriverFactory>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Punto de extensión: Codex y Pi se registran aquí en P4 sin tocar
    /// SessionManager.
    pub fn register(&mut self, factory: Box<dyn DriverFactory>) {
        self.factories.insert(factory.id(), factory);
    }

    pub fn contains(&self, id: &str) -> bool {
        self.factories.contains_key(id)
    }

    pub fn create(&self, id: &str) -> Result<Box<dyn AgentDriver>, AgentError> {
        match self.factories.get(id) {
            Some(factory) => Ok(factory.create()),
            None => Err(AgentError {
                code: "unknown_provider".into(),
                message: format!("no driver registered for `{id}`"),
            }),
        }
    }

    pub fn ids(&self) -> Vec<ProviderId> {
        let mut ids: Vec<_> = self.factories.keys().cloned().collect();
        ids.sort();
        ids
    }

    pub fn supports(&self, id: &str, version: &ProviderVersion) -> Result<bool, AgentError> {
        match self.factories.get(id) {
            Some(factory) => Ok(factory.supports(version)),
            None => Err(AgentError {
                code: "unknown_provider".into(),
                message: format!("no driver registered for `{id}`"),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_provider_is_reported() {
        let registry = ProviderRegistry::new();
        match registry.create("codex") {
            Ok(_) => panic!("unknown provider must fail"),
            Err(err) => assert_eq!(err.code, "unknown_provider"),
        }
    }
}

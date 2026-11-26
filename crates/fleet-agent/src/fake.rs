//! Driver determinista para pruebas de contrato; sin procesos ni red.

use std::collections::VecDeque;
use std::time::Duration;

use fleet_domain::{
    ProviderAvailability, ProviderCapabilities, ProviderDescriptor, ProviderVersion, RunnerMode,
    SessionState, TurnStatus, VersionRange,
};

use super::{
    AgentDriver, AgentError, AgentEvent, AgentInput, DomainEvent, DriverFactory, LaunchContext,
    RecoveryEvidence, SendReceipt,
};

/// Palancas deterministicas para reproducir carreras y fallos de driver.
#[derive(Debug, Clone, Default)]
pub struct FakeConfig {
    pub fail_send: bool,
    pub block_send: bool,
    pub block_close: bool,
    pub slow_close_ms: u64,
    pub fail_close: bool,
    pub emit_provider_error: bool,
    pub emit_terminal_failure: bool,
    pub emit_approval: bool,
    pub fail_answer: bool,
    /// P4.1-A.2: palancas para probar interrupción, errores y carreras.
    pub fail_interrupt: bool,
    pub interrupt_without_terminal: bool,
    /// El turno no termina hasta que llegue una interrupción explícita.
    pub defer_terminal_until_interrupt: bool,
    /// Pausa entre el primer delta y el resto: permite matar fleetd con un
    /// turno activo y comprobar que el runner sigue produciendo eventos.
    pub slow_turn_ms: u64,
}

pub struct FakeDriver {
    descriptor: ProviderDescriptor,
    config: FakeConfig,
    pending: VecDeque<AgentEvent>,
    deferred: VecDeque<AgentEvent>,
    turns: u64,
    opened: bool,
    closed: bool,
}

impl Default for FakeDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeDriver {
    pub fn new() -> Self {
        Self::with_config(FakeConfig::default())
    }

    pub fn with_config(config: FakeConfig) -> Self {
        let capabilities = ProviderCapabilities {
            streaming_text: true,
            tool_events: true,
            session_history: true,
            native_turn_ids: true,
            cancel_turn: true,
            approvals: false,
            can_own_process: false,
            can_attach_service: false,
        };
        Self {
            descriptor: ProviderDescriptor {
                id: "fake".into(),
                version: ProviderVersion::parse("0.1.0"),
                capabilities,
                mode: RunnerMode::Owned,
                availability: ProviderAvailability::Available,
                controls: vec![],
            },
            config,
            pending: VecDeque::new(),
            deferred: VecDeque::new(),
            turns: 0,
            opened: false,
            closed: false,
        }
    }

    fn event(kind: DomainEvent) -> AgentEvent {
        AgentEvent { kind }
    }
}

impl AgentDriver for FakeDriver {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn open(&mut self, ctx: LaunchContext) -> Result<String, AgentError> {
        if !ctx.workspace.is_dir() {
            return Err(AgentError {
                code: "invalid_workspace".into(),
                message: format!("{:?} is not a directory", ctx.workspace),
            });
        }
        self.opened = true;
        Ok("native-fake-session".into())
    }

    fn send(&mut self, input: AgentInput) -> Result<SendReceipt, AgentError> {
        if !self.opened || self.closed {
            return Err(AgentError {
                code: "driver_closed".into(),
                message: "fake driver is not open".into(),
            });
        }
        if self.config.block_send {
            std::thread::sleep(Duration::from_secs(60));
        }
        if self.config.fail_send {
            // El valor es una credencial FICTICIA inyectada por el E2E de
            // redacción; nunca se escribe en logs ni se devuelve sin sanitizar.
            let injected = std::env::var("FLEET_FAKE_ERROR_SECRET").unwrap_or_default();
            let message = if injected.is_empty() {
                "injected fake send failure".to_string()
            } else {
                format!("injected fake send failure api_key={injected}")
            };
            return Err(AgentError {
                code: "driver_send_failed".into(),
                message,
            });
        }
        self.turns += 1;
        let turn = input.turn_id;
        if self.config.emit_approval {
            self.pending
                .push_back(Self::event(DomainEvent::ApprovalRequested {
                    native_request_id: format!("native-{turn}"),
                    kind: "shell".into(),
                    message: "allow fake command?".into(),
                    resources: vec![input.text.clone()],
                    allowed_decisions: vec!["once".into(), "reject".into()],
                    options: vec![
                        fleet_domain::ApprovalOptionSpec {
                            option_id: "once".into(),
                            label: "Allow once".into(),
                            args: vec![],
                        },
                        fleet_domain::ApprovalOptionSpec {
                            option_id: "reject".into(),
                            label: "Deny".into(),
                            args: vec![],
                        },
                    ],
                }));
        }
        if self.config.emit_provider_error {
            self.pending
                .push_back(Self::event(DomainEvent::AgentNotice {
                    message: "provider notice before completion".into(),
                }));
        }
        self.pending
            .push_back(Self::event(DomainEvent::AssistantDelta {
                turn_id: turn.clone(),
                text: "Hello ".into(),
            }));
        if self.config.slow_turn_ms > 0 {
            std::thread::sleep(Duration::from_millis(self.config.slow_turn_ms));
        }
        self.pending
            .push_back(Self::event(DomainEvent::AssistantDelta {
                turn_id: turn.clone(),
                text: "from fake".into(),
            }));
        self.pending
            .push_back(Self::event(DomainEvent::ToolStarted {
                turn_id: turn.clone(),
                name: "fake_echo".into(),
                input: input.text.clone(),
            }));
        self.pending.push_back(Self::event(DomainEvent::ToolOutput {
            turn_id: turn.clone(),
            name: "fake_echo".into(),
            output: "ok".into(),
        }));
        self.pending
            .push_back(Self::event(DomainEvent::ToolCompleted {
                turn_id: turn.clone(),
                name: "fake_echo".into(),
                success: true,
            }));
        self.pending
            .push_back(Self::event(DomainEvent::AssistantMessage {
                turn_id: turn.clone(),
                text: "Hello from fake".into(),
            }));
        if self.config.defer_terminal_until_interrupt {
            // Sin terminal propio: la única terminalidad válida vendrá de
            // `interrupt` (P4.1-A.2).
        } else if self.config.emit_terminal_failure {
            self.pending.push_back(Self::event(DomainEvent::TurnFailed {
                turn_id: turn.clone(),
                message: "injected terminal failure".into(),
            }));
        } else {
            self.pending
                .push_back(Self::event(DomainEvent::TurnCompleted {
                    turn_id: turn,
                    status: TurnStatus::Completed,
                }));
        }
        if self.config.emit_approval {
            // No text, tools or terminal result may occur until the actual
            // permission decision is received. A premature TurnCompleted would
            // hide bugs in SessionManager's approval state machine.
            let request = self.pending.pop_front().expect("approval was queued");
            self.deferred.append(&mut self.pending);
            self.pending.push_back(request);
        }
        Ok(SendReceipt { accepted: true })
    }

    fn answer(&mut self, native_request_id: String, decision: String) -> Result<(), AgentError> {
        if self.config.fail_answer {
            return Err(AgentError {
                code: "approval_delivery_failed".into(),
                message: "injected fake answer failure".into(),
            });
        }
        if self.config.block_send {
            std::thread::sleep(Duration::from_secs(60));
        }
        self.pending
            .push_back(Self::event(DomainEvent::ApprovalResolved {
                native_request_id: native_request_id.clone(),
                decision: decision.clone(),
            }));
        if decision == "reject" {
            // Rejection prevents the deferred fake tool command from running.
            self.deferred.clear();
            // Native test request identifiers use native-{turn_id}.
            // Rejection is terminal without running the deferred tool.
            let turn_id = native_request_id
                .strip_prefix("native-")
                .unwrap_or(&native_request_id)
                .to_string();
            self.pending
                .push_back(Self::event(DomainEvent::TurnCompleted {
                    turn_id,
                    status: TurnStatus::Interrupted,
                }));
        } else {
            self.pending.append(&mut self.deferred);
        }
        Ok(())
    }

    fn interrupt(&mut self, turn_id: String) -> Result<(), AgentError> {
        if self.config.fail_interrupt {
            return Err(AgentError {
                code: "interrupt_delivery_failed".into(),
                message: "injected fake interrupt failure".into(),
            });
        }
        self.deferred.clear();
        if !self.config.interrupt_without_terminal {
            self.pending
                .push_back(Self::event(DomainEvent::TurnCompleted {
                    turn_id,
                    status: TurnStatus::Interrupted,
                }));
        }
        Ok(())
    }

    fn next_event(&mut self, timeout: Duration) -> Result<Option<AgentEvent>, AgentError> {
        if let Some(event) = self.pending.pop_front() {
            return Ok(Some(event));
        }
        std::thread::sleep(timeout.min(Duration::from_millis(10)));
        Ok(None)
    }

    fn close(&mut self) -> Result<(), AgentError> {
        if self.config.block_close {
            std::thread::sleep(Duration::from_secs(60));
        }
        if self.config.slow_close_ms != 0 {
            std::thread::sleep(Duration::from_millis(self.config.slow_close_ms));
        }
        if self.config.fail_close {
            return Err(AgentError {
                code: "driver_close_failed".into(),
                message: "injected fake close failure".into(),
            });
        }
        self.closed = true;
        Ok(())
    }

    fn inspect_recovery(&self) -> RecoveryEvidence {
        RecoveryEvidence {
            session_alive: self.opened && !self.closed,
            detail: format!("fake driver turns={}", self.turns),
        }
    }

    fn session_state(&self) -> SessionState {
        if self.closed {
            SessionState::Stopped
        } else {
            SessionState::Running
        }
    }
}

/// Fábrica simple: permite probar el flujo completo sin modelo real.
pub struct FakeFactory(FakeConfig);

impl FakeFactory {
    pub fn new() -> Self {
        Self(FakeConfig::default())
    }

    pub fn with_config(config: FakeConfig) -> Self {
        Self(config)
    }
}

impl Default for FakeFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl DriverFactory for FakeFactory {
    fn id(&self) -> String {
        "fake".into()
    }
    fn compatibility(&self) -> VersionRange {
        VersionRange {
            min_major: 0,
            min_minor: 1,
            max_major_exclusive: 1,
        }
    }
    fn create(&self) -> Box<dyn AgentDriver> {
        Box::new(FakeDriver::with_config(self.0.clone()))
    }
    fn metadata(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            id: "fake".into(),
            version: ProviderVersion::parse("0.1.0"),
            capabilities: ProviderCapabilities {
                streaming_text: true,
                tool_events: true,
                session_history: true,
                native_turn_ids: true,
                cancel_turn: true,
                approvals: false,
                can_own_process: false,
                can_attach_service: false,
            },
            mode: RunnerMode::Owned,
            availability: ProviderAvailability::Available,
            controls: vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_ends_with_turn_completed() {
        let dir = std::env::temp_dir();
        let mut driver = FakeDriver::new();
        driver
            .open(LaunchContext {
                workspace: dir,
                mode: RunnerMode::Owned,
            })
            .unwrap();
        driver
            .send(AgentInput {
                text: "hi".into(),
                turn_id: "turn-1".into(),
                controls: vec![],
            })
            .unwrap();
        let mut kinds = Vec::new();
        while let Some(event) = driver.next_event(Duration::from_millis(1)).unwrap() {
            kinds.push(event.kind);
        }
        assert_eq!(kinds.len(), 7);
        assert!(matches!(
            kinds.last(),
            Some(DomainEvent::TurnCompleted {
                status: TurnStatus::Completed,
                ..
            })
        ));
        let factory = FakeFactory::new();
        assert!(factory.supports(&ProviderVersion::parse("0.9.0")));
        assert!(!factory.supports(&ProviderVersion::parse("1.0.0")));
    }
}

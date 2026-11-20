//! Driver determinista para pruebas de contrato; sin procesos ni red.

use std::collections::VecDeque;
use std::time::Duration;

use fleet_domain::{
    ProviderCapabilities, ProviderDescriptor, ProviderVersion, RunnerMode, SessionState,
    TurnStatus, VersionRange,
};

use super::{
    AgentDriver, AgentError, AgentEvent, AgentInput, DomainEvent, DriverFactory, LaunchContext,
    RecoveryEvidence, SendReceipt,
};

pub struct FakeDriver {
    descriptor: ProviderDescriptor,
    pending: VecDeque<AgentEvent>,
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
        let capabilities = ProviderCapabilities {
            streaming_text: true,
            tool_events: true,
            session_history: true,
            native_turn_ids: true,
            cancel_turn: false,
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
            },
            pending: VecDeque::new(),
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
        self.turns += 1;
        let turn = input.turn_id;
        let input_text = input.text.clone();
        self.pending
            .push_back(Self::event(DomainEvent::TurnStarted {
                turn_id: turn.clone(),
            }));
        self.pending
            .push_back(Self::event(DomainEvent::AssistantDelta {
                turn_id: turn.clone(),
                text: "Hello ".into(),
            }));
        self.pending
            .push_back(Self::event(DomainEvent::AssistantDelta {
                turn_id: turn.clone(),
                text: "from fake".into(),
            }));
        self.pending
            .push_back(Self::event(DomainEvent::ToolStarted {
                turn_id: turn.clone(),
                name: "fake_echo".into(),
                input: input_text,
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
        self.pending
            .push_back(Self::event(DomainEvent::TurnCompleted {
                turn_id: turn,
                status: TurnStatus::Completed,
            }));
        Ok(SendReceipt { accepted: true })
    }

    fn next_event(&mut self, timeout: Duration) -> Result<Option<AgentEvent>, AgentError> {
        if let Some(event) = self.pending.pop_front() {
            return Ok(Some(event));
        }
        std::thread::sleep(timeout.min(Duration::from_millis(10)));
        Ok(None)
    }

    fn close(&mut self) -> Result<(), AgentError> {
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
pub struct FakeFactory;

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
        Box::new(FakeDriver::new())
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
            })
            .unwrap();
        let mut kinds = Vec::new();
        while let Some(event) = driver.next_event(Duration::from_millis(1)).unwrap() {
            kinds.push(event.kind);
        }
        assert_eq!(kinds.len(), 8);
        assert!(matches!(
            kinds.last(),
            Some(DomainEvent::TurnCompleted {
                status: TurnStatus::Completed,
                ..
            })
        ));
        assert!(FakeFactory.supports(&ProviderVersion::parse("0.9.0")));
        assert!(!FakeFactory.supports(&ProviderVersion::parse("1.0.0")));
    }
}

//! Abstracción Runner usada por `SessionManager`.
//!
//! En P3 el runner real es un proceso independiente (`fleet-runner`); aquí vive
//! el contrato y un launcher en proceso para pruebas deterministas.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use fleet_agent::fake::FakeFactory;
use fleet_agent::ProviderRegistry;
use fleet_agent::{AgentError, AgentInput, LaunchContext};
use fleet_domain::store::RunnerRecord;
use fleet_domain::{
    AgentEvent, DomainEvent, HostId, ProviderId, RunnerEvent, RunnerIdentity, RunnerMode,
    RunnerStatus, SessionId,
};

pub const IN_PROCESS_RUNNER_WIRE: &str = "fleet-runner-inprocess-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerCommand {
    Prompt {
        text: String,
        turn_id: String,
        operation_key: String,
        controls: Vec<fleet_domain::RequestedControl>,
    },
    Answer {
        approval_id: String,
        native_request_id: String,
        decision: String,
        operation_key: String,
        arguments: Vec<fleet_domain::ApprovalArgument>,
    },
    Interrupt {
        turn_id: String,
    },
    Stop,
}

/// Manejador de un runner. Todos los métodos son seguros para llamar desde el
/// hilo de ingress y desde el SessionManager simultáneamente.
pub trait RunnerHandle: Send + Sync {
    fn identity(&self) -> &RunnerIdentity;
    fn submit(&self, command: RunnerCommand) -> Result<(), AgentError>;
    fn recv(&self, timeout: Duration) -> Result<Option<RunnerEvent>, AgentError>;
    fn ack(&self, runner_seq: u64) -> Result<(), AgentError>;
    /// Probe the provider, not merely whether its IPC socket still exists.
    fn provider_alive(&self) -> Result<bool, AgentError> {
        Ok(true) // local FakeDriver contract; process runner overrides it
    }

    /// Notas durables del runner (p.ej. saturación del spool). Son eventos
    /// informativos, nunca decisiones ni respuestas de permiso.
    fn notices(&self) -> Vec<String> {
        Vec::new()
    }
    fn close(&self) -> Result<(), AgentError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerLaunchContext {
    pub session_id: SessionId,
    pub provider_id: ProviderId,
    pub workspace: PathBuf,
    pub mode: RunnerMode,
    pub existing: Option<RunnerRecord>,
    pub ask_shell: bool,
    pub fail_answer: bool,
}

pub trait RunnerLauncher: Send + Sync {
    fn launch(&self, ctx: RunnerLaunchContext) -> Result<Box<dyn RunnerHandle>, AgentError>;
}

struct InProcessHandle {
    identity: RunnerIdentity,
    commands: SyncSender<RunnerCommand>,
    events: Mutex<Receiver<RunnerEvent>>,
    closed: Mutex<Receiver<()>>,
}

impl RunnerHandle for InProcessHandle {
    fn identity(&self) -> &RunnerIdentity {
        &self.identity
    }

    fn submit(&self, command: RunnerCommand) -> Result<(), AgentError> {
        self.commands
            .try_send(command)
            .map_err(|error| match error {
                TrySendError::Full(_) => AgentError {
                    code: "runner_busy".into(),
                    message: "runner command queue full (backpressure)".into(),
                },
                TrySendError::Disconnected(_) => AgentError {
                    code: "runner_gone".into(),
                    message: "runner stopped".into(),
                },
            })
    }

    fn recv(&self, timeout: Duration) -> Result<Option<RunnerEvent>, AgentError> {
        match self
            .events
            .lock()
            .expect("runner events poisoned")
            .recv_timeout(timeout)
        {
            Ok(event) => Ok(Some(event)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(AgentError {
                code: "runner_gone".into(),
                message: "in-process runner stopped".into(),
            }),
        }
    }

    fn ack(&self, _runner_seq: u64) -> Result<(), AgentError> {
        Ok(()) // el canal en proceso entrega una sola vez
    }

    fn close(&self) -> Result<(), AgentError> {
        let _ = self.commands.try_send(RunnerCommand::Stop);
        match self
            .closed
            .lock()
            .expect("runner close poisoned")
            .recv_timeout(Duration::from_millis(500))
        {
            Ok(()) => Ok(()),
            Err(_) => Err(AgentError {
                code: "StopUnconfirmed".into(),
                message: "in-process runner did not confirm close".into(),
            }),
        }
    }
}

/// Launcher en proceso: conserva el comportamiento P1/P2 para tests y como
/// fallback de contract tests, sin implicar que el runner real viva en fleetd.
pub struct InProcessRunnerLauncher {
    registry: ProviderRegistry,
    host_id: HostId,
}

impl InProcessRunnerLauncher {
    pub fn new(registry: ProviderRegistry, host_id: HostId) -> Self {
        Self { registry, host_id }
    }
}

impl RunnerLauncher for InProcessRunnerLauncher {
    fn launch(&self, ctx: RunnerLaunchContext) -> Result<Box<dyn RunnerHandle>, AgentError> {
        if ctx.existing.is_some() {
            return Err(AgentError {
                code: "reattach_unavailable".into(),
                message: "in-process launcher cannot reattach a persistent runner".into(),
            });
        }
        let mut driver = self.registry.create(&ctx.provider_id)?;
        driver.open(LaunchContext {
            workspace: ctx.workspace,
            mode: ctx.mode,
        })?;
        let (commands, command_rx) = mpsc::sync_channel(16);
        let (events_tx, events_rx) = mpsc::channel();
        let (closed_tx, closed_rx) = mpsc::channel();
        let runner_id = format!("runner-{}", ctx.session_id);
        let generation = 1;
        let identity = RunnerIdentity {
            runner_id: runner_id.clone(),
            session_id: ctx.session_id.clone(),
            provider_id: ctx.provider_id.clone(),
            host_id: self.host_id.clone(),
            boot_id: format!("boot-{}", std::process::id()),
            pid: std::process::id(),
            protocol_version: IN_PROCESS_RUNNER_WIRE.into(),
            generation,
        };
        spawn_in_process_driver(driver, runner_id, command_rx, events_tx, closed_tx);
        Ok(Box::new(InProcessHandle {
            identity,
            commands,
            events: Mutex::new(events_rx),
            closed: Mutex::new(closed_rx),
        }))
    }
}

fn spawn_in_process_driver(
    mut driver: Box<dyn fleet_agent::AgentDriver>,
    runner_id: String,
    commands: Receiver<RunnerCommand>,
    events: Sender<RunnerEvent>,
    closed: Sender<()>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut runner_seq = 0u64;
        loop {
            match commands.recv_timeout(Duration::from_millis(20)) {
                Ok(RunnerCommand::Prompt {
                    text,
                    turn_id,
                    operation_key: _,
                    controls,
                }) => {
                    if let Err(error) = driver.send(AgentInput {
                        text,
                        turn_id,
                        controls,
                    }) {
                        emit(
                            &events,
                            &runner_id,
                            &mut runner_seq,
                            DomainEvent::AgentError {
                                message: error.to_string(),
                            },
                        );
                    }
                }
                Ok(RunnerCommand::Answer {
                    native_request_id,
                    decision,
                    arguments,
                    ..
                }) => {
                    if let Err(error) =
                        driver.answer_with_args(native_request_id, decision, arguments)
                    {
                        emit(
                            &events,
                            &runner_id,
                            &mut runner_seq,
                            DomainEvent::AgentError {
                                message: error.to_string(),
                            },
                        );
                    }
                }
                Ok(RunnerCommand::Interrupt { turn_id }) => {
                    // P4.1-A.2: Interrupt es una operación real, no un no-op.
                    if let Err(error) = driver.interrupt(turn_id) {
                        emit(
                            &events,
                            &runner_id,
                            &mut runner_seq,
                            DomainEvent::AgentError {
                                message: error.to_string(),
                            },
                        );
                    }
                }
                Ok(RunnerCommand::Stop) => {
                    let _ = driver.close();
                    let _ = closed.send(());
                    break;
                }
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
            match driver.next_event(Duration::from_millis(50)) {
                Ok(Some(AgentEvent { kind })) => emit(&events, &runner_id, &mut runner_seq, kind),
                Ok(None) => {}
                Err(error) => emit(
                    &events,
                    &runner_id,
                    &mut runner_seq,
                    DomainEvent::AgentError {
                        message: error.to_string(),
                    },
                ),
            }
        }
    })
}

fn emit(events: &Sender<RunnerEvent>, runner_id: &str, runner_seq: &mut u64, kind: DomainEvent) {
    *runner_seq += 1;
    let _ = events.send(RunnerEvent {
        runner_id: runner_id.to_string(),
        runner_seq: *runner_seq,
        event: AgentEvent { kind },
    });
}

/// Conveniencia para tests que necesitan el launcher fake sin registry global.
pub fn fake_launcher(host_id: HostId) -> InProcessRunnerLauncher {
    let mut registry = ProviderRegistry::new();
    registry.register(Box::new(FakeFactory::new()));
    InProcessRunnerLauncher::new(registry, host_id)
}

pub fn disconnected_status() -> RunnerStatus {
    RunnerStatus::Disconnected
}

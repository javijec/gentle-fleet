//! SessionManager y timeline en memoria. No conoce proveedores concretos,
//! bases de datos, sockets ni clientes (D02, D06, P0-D6).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fleet_agent::{
    AgentDriver, AgentError, AgentEvent, AgentInput, DomainEvent, LaunchContext, ProviderRegistry,
};
use fleet_domain::{
    Event, EventKind, ProviderId, RunnerMode, SessionId, SessionState, WorkspaceId,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSummary {
    pub id: WorkspaceId,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: SessionId,
    pub workspace_id: WorkspaceId,
    pub provider_id: ProviderId,
    pub state: SessionState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptReceipt {
    pub operation_id: String,
    pub session_id: SessionId,
    pub turn_id: String,
}

enum DriverCommand {
    Prompt { text: String, turn_id: String },
    Close { done: Sender<()> },
}

struct DriverHandle {
    commands: Sender<DriverCommand>,
    join: Option<JoinHandle<()>>,
}

struct SessionRecord {
    summary: SessionSummary,
    timeline: Vec<Event>,
    next_seq: u64,
    driver: Option<DriverHandle>,
}

#[derive(Default)]
struct BusState {
    next_subscriber: u64,
    subscribers: HashMap<u64, Sender<Event>>,
}

/// Difusión en memoria: cada suscriptor recibe cada evento una sola vez.
#[derive(Default)]
pub struct EventBus {
    state: Mutex<BusState>,
}

impl EventBus {
    pub fn subscribe(&self) -> (u64, Receiver<Event>) {
        let (tx, rx) = mpsc::channel();
        let mut state = self.state.lock().expect("event bus poisoned");
        state.next_subscriber += 1;
        let id = state.next_subscriber;
        state.subscribers.insert(id, tx);
        (id, rx)
    }

    pub fn unsubscribe(&self, id: u64) {
        let mut state = self.state.lock().expect("event bus poisoned");
        state.subscribers.remove(&id);
    }

    pub fn publish(&self, event: Event) {
        let mut state = self.state.lock().expect("event bus poisoned");
        state
            .subscribers
            .retain(|_, tx| tx.send(event.clone()).is_ok());
    }
}

#[derive(Default)]
struct ManagerState {
    workspaces: Vec<WorkspaceSummary>,
    sessions: HashMap<SessionId, SessionRecord>,
    operations: HashMap<String, PromptReceipt>,
    next_ids: u64,
}

pub struct SessionManager {
    registry: ProviderRegistry,
    bus: EventBus,
    state: Mutex<ManagerState>,
}

impl SessionManager {
    pub fn new(registry: ProviderRegistry) -> Self {
        Self {
            registry,
            bus: EventBus::default(),
            state: Mutex::new(ManagerState::default()),
        }
    }

    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    pub fn registered_providers(&self) -> Vec<ProviderId> {
        self.registry.ids()
    }

    fn next_id(&self, prefix: &str) -> String {
        let mut state = self.state.lock().expect("manager poisoned");
        state.next_ids += 1;
        format!("{prefix}_{:08x}", state.next_ids)
    }

    pub fn list_workspaces(&self) -> Vec<WorkspaceSummary> {
        self.state
            .lock()
            .expect("manager poisoned")
            .workspaces
            .clone()
    }

    pub fn register_workspace(&self, raw_path: &str) -> Result<WorkspaceSummary, AgentError> {
        let path = Path::new(raw_path).canonicalize().map_err(|e| AgentError {
            code: "invalid_workspace".into(),
            message: format!("cannot resolve `{raw_path}`: {e}"),
        })?;
        if !path.is_dir() {
            return Err(AgentError {
                code: "invalid_workspace".into(),
                message: format!("`{}` is not a directory", path.display()),
            });
        }
        let mut state = self.state.lock().expect("manager poisoned");
        if let Some(existing) = state.workspaces.iter().find(|w| w.path == path) {
            return Ok(existing.clone());
        }
        let workspace = WorkspaceSummary {
            id: self.next_id_locked(&mut state, "ws"),
            path,
        };
        state.workspaces.push(workspace.clone());
        Ok(workspace)
    }

    fn next_id_locked(&self, state: &mut ManagerState, prefix: &str) -> String {
        state.next_ids += 1;
        format!("{prefix}_{:08x}", state.next_ids)
    }

    pub fn list_sessions(&self) -> Vec<SessionSummary> {
        self.state
            .lock()
            .expect("manager poisoned")
            .sessions
            .values()
            .map(|s| s.summary.clone())
            .collect()
    }

    pub fn create_session(
        self: &Arc<Self>,
        workspace_id: &str,
        provider_id: &str,
    ) -> Result<SessionSummary, AgentError> {
        let workspace = {
            let state = self.state.lock().expect("manager poisoned");
            state
                .workspaces
                .iter()
                .find(|w| w.id == workspace_id)
                .cloned()
                .ok_or_else(|| AgentError {
                    code: "not_found".into(),
                    message: format!("unknown workspace `{workspace_id}`"),
                })?
        };

        if !self.registry.contains(provider_id) {
            return Err(AgentError {
                code: "unknown_provider".into(),
                message: format!("no driver registered for `{provider_id}`"),
            });
        }
        let mut driver = self.registry.create(provider_id)?;
        let session_id = self.next_id("ses");
        driver.open(LaunchContext {
            workspace: workspace.path.clone(),
            mode: RunnerMode::Auto,
        })?;

        let summary = SessionSummary {
            id: session_id.clone(),
            workspace_id: workspace.id.clone(),
            provider_id: provider_id.to_string(),
            state: SessionState::Running,
        };
        let (commands, rx) = mpsc::channel();
        let handle = DriverHandle {
            commands,
            join: Some(self.spawn_driver(session_id.clone(), driver, rx)),
        };
        {
            let mut state = self.state.lock().expect("manager poisoned");
            state.sessions.insert(
                session_id.clone(),
                SessionRecord {
                    summary: summary.clone(),
                    timeline: Vec::new(),
                    next_seq: 1,
                    driver: Some(handle),
                },
            );
        }
        self.publish_local(
            &session_id,
            EventKind::SessionCreated {
                provider_id: provider_id.to_string(),
                workspace_id: workspace.id,
            },
        );
        self.publish_local(
            &session_id,
            EventKind::SessionStateChanged {
                state: SessionState::Running,
            },
        );
        Ok(summary)
    }

    fn spawn_driver(
        self: &Arc<Self>,
        session_id: SessionId,
        mut driver: Box<dyn AgentDriver>,
        commands: Receiver<DriverCommand>,
    ) -> JoinHandle<()> {
        let manager = Arc::clone(self);
        thread::spawn(move || loop {
            match commands.recv_timeout(Duration::from_millis(20)) {
                Ok(DriverCommand::Prompt { text, turn_id }) => {
                    if let Err(error) = driver.send(AgentInput { text, turn_id }) {
                        manager.publish_local(
                            &session_id,
                            EventKind::AgentError {
                                message: error.to_string(),
                            },
                        );
                    }
                }
                Ok(DriverCommand::Close { done }) => {
                    if let Err(error) = driver.close() {
                        manager.publish_local(
                            &session_id,
                            EventKind::AgentError {
                                message: error.to_string(),
                            },
                        );
                    }
                    let _ = done.send(());
                    break;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            match driver.next_event(Duration::from_millis(50)) {
                Ok(Some(AgentEvent { kind })) => {
                    manager.publish_agent(&session_id, kind);
                }
                Ok(None) => {}
                Err(error) => manager.publish_local(
                    &session_id,
                    EventKind::AgentError {
                        message: error.to_string(),
                    },
                ),
            }
        })
    }

    fn next_event(&self, session_id: &SessionId, kind: EventKind) -> Event {
        let mut state = self.state.lock().expect("manager poisoned");
        let record = state
            .sessions
            .get_mut(session_id)
            .expect("session disappeared while publishing");
        let seq = record.next_seq;
        record.next_seq += 1;
        let event = Event {
            event_id: format!("{session_id}-e{seq}"),
            session_id: session_id.clone(),
            seq,
            created_at_ms: now_ms(),
            kind,
        };
        record.timeline.push(event.clone());
        event
    }

    fn publish_local(&self, session_id: &SessionId, kind: EventKind) {
        let event = self.next_event(session_id, kind);
        self.bus.publish(event);
    }

    fn publish_agent(&self, session_id: &SessionId, kind: DomainEvent) {
        let fleet_kind = match kind {
            DomainEvent::TurnStarted { turn_id } => EventKind::TurnStarted { turn_id },
            DomainEvent::AssistantDelta { turn_id, text } => {
                EventKind::AssistantDelta { turn_id, text }
            }
            DomainEvent::AssistantMessage { turn_id, text } => {
                EventKind::AssistantMessage { turn_id, text }
            }
            DomainEvent::ToolStarted {
                turn_id,
                name,
                input,
            } => EventKind::ToolStarted {
                turn_id,
                name,
                input,
            },
            DomainEvent::ToolOutput {
                turn_id,
                name,
                output,
            } => EventKind::ToolOutput {
                turn_id,
                name,
                output,
            },
            DomainEvent::ToolCompleted {
                turn_id,
                name,
                success,
            } => EventKind::ToolCompleted {
                turn_id,
                name,
                success,
            },
            DomainEvent::TurnCompleted { turn_id, status } => {
                EventKind::TurnCompleted { turn_id, status }
            }
            DomainEvent::AgentError { message } => EventKind::AgentError { message },
        };
        self.publish_local(session_id, fleet_kind);
    }

    /// Devuelve el recibo ya almacenado si la misma operación se reenvía.
    /// Nunca envía dos veces un prompt por una clave repetida (P0-D4).
    pub fn send_prompt(
        &self,
        session_id: &str,
        text: String,
        operation_key: &str,
    ) -> Result<PromptReceipt, AgentError> {
        if let Some(existing) = self
            .state
            .lock()
            .expect("manager poisoned")
            .operations
            .get(operation_key)
        {
            return Ok(existing.clone());
        }
        let (command_tx, turn_id) = {
            let mut state = self.state.lock().expect("manager poisoned");
            let turn_id = self.next_id_locked(&mut state, "turn");
            let record = state
                .sessions
                .get_mut(session_id)
                .ok_or_else(|| AgentError {
                    code: "not_found".into(),
                    message: format!("unknown session `{session_id}`"),
                })?;
            let handle = record.driver.as_ref().ok_or_else(|| AgentError {
                code: "session_stopped".into(),
                message: format!("session `{session_id}` is stopped"),
            })?;
            (handle.commands.clone(), turn_id)
        };
        command_tx
            .send(DriverCommand::Prompt {
                text: text.clone(),
                turn_id: turn_id.clone(),
            })
            .map_err(|_| AgentError {
                code: "runner_gone".into(),
                message: "driver command channel is closed".into(),
            })?;
        self.publish_local(
            &session_id.to_string(),
            EventKind::UserMessage {
                turn_id: turn_id.clone(),
                text,
            },
        );
        let receipt = PromptReceipt {
            operation_id: self.next_id("op"),
            session_id: session_id.to_string(),
            turn_id,
        };
        self.state
            .lock()
            .expect("manager poisoned")
            .operations
            .insert(operation_key.to_string(), receipt.clone());
        Ok(receipt)
    }

    /// Suscribe primero y después entrega el snapshot: no hay hueco entre
    /// snapshot y eventos en vivo. El receptor deduce por `seq` (P0-D6).
    pub fn subscribe(
        &self,
        session_id: &str,
        since_seq: u64,
    ) -> Result<(u64, Vec<Event>, Receiver<Event>), AgentError> {
        let (subscriber_id, receiver) = self.bus.subscribe();
        let snapshot = {
            let state = self.state.lock().expect("manager poisoned");
            let record = state.sessions.get(session_id).ok_or_else(|| AgentError {
                code: "not_found".into(),
                message: format!("unknown session `{session_id}`"),
            })?;
            record
                .timeline
                .iter()
                .filter(|event| event.seq > since_seq)
                .cloned()
                .collect()
        };
        Ok((subscriber_id, snapshot, receiver))
    }

    pub fn stop_session(&self, session_id: &str) -> Result<(), AgentError> {
        let handle = {
            let mut state = self.state.lock().expect("manager poisoned");
            let record = state
                .sessions
                .get_mut(session_id)
                .ok_or_else(|| AgentError {
                    code: "not_found".into(),
                    message: format!("unknown session `{session_id}`"),
                })?;
            record.driver.take().ok_or_else(|| AgentError {
                code: "session_stopped".into(),
                message: format!("session `{session_id}` already stopped"),
            })?
        };
        let (done_tx, done_rx) = mpsc::channel();
        let _ = handle.commands.send(DriverCommand::Close { done: done_tx });
        let _ = done_rx.recv_timeout(Duration::from_secs(5));
        if let Some(join) = handle.join {
            let _ = join.join();
        }
        {
            let mut state = self.state.lock().expect("manager poisoned");
            if let Some(record) = state.sessions.get_mut(session_id) {
                record.summary.state = SessionState::Stopped;
            }
        }
        self.publish_local(
            &session_id.to_string(),
            EventKind::SessionStateChanged {
                state: SessionState::Stopped,
            },
        );
        Ok(())
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use fleet_agent::fake::FakeDriver;
    use fleet_agent::DriverFactory;
    use fleet_domain::{ProviderCapabilities, ProviderVersion, TurnStatus, VersionRange};

    struct FakeFactory;

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

    fn manager() -> Arc<SessionManager> {
        let mut registry = ProviderRegistry::new();
        registry.register(Box::new(FakeFactory));
        Arc::new(SessionManager::new(registry))
    }

    fn collect_until_turn_end(rx: &Receiver<Event>) -> Vec<Event> {
        let mut events = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(event) => {
                    let done = matches!(
                        event.kind,
                        EventKind::TurnCompleted {
                            status: TurnStatus::Completed,
                            ..
                        }
                    );
                    events.push(event);
                    if done {
                        return events;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        panic!("turn did not complete");
    }

    #[test]
    fn two_subscribers_see_the_same_sequence() {
        let manager = manager();
        let dir = std::env::temp_dir();
        let workspace = manager.register_workspace(&dir.to_string_lossy()).unwrap();
        let session = manager.create_session(&workspace.id, "fake").unwrap();
        let (sub_a, snapshot_a, rx_a) = manager.subscribe(&session.id, 0).unwrap();
        let (sub_b, snapshot_b, rx_b) = manager.subscribe(&session.id, 0).unwrap();
        assert_eq!(snapshot_a, snapshot_b);
        assert_eq!(snapshot_a.len(), 2);

        manager
            .send_prompt(&session.id, "hi".into(), "op-1")
            .unwrap();

        let events_a = collect_until_turn_end(&rx_a);
        let events_b = collect_until_turn_end(&rx_b);
        assert_eq!(
            events_a.iter().map(|e| e.seq).collect::<Vec<_>>(),
            events_b.iter().map(|e| e.seq).collect::<Vec<_>>()
        );
        let seqs: Vec<u64> = events_a.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, (3..3 + seqs.len() as u64).collect::<Vec<_>>());
        assert!(events_a
            .iter()
            .any(|e| matches!(e.kind, EventKind::AssistantDelta { .. })));
        assert!(events_a
            .iter()
            .any(|e| matches!(e.kind, EventKind::ToolCompleted { .. })));
        manager.bus().unsubscribe(sub_a);
        manager.bus().unsubscribe(sub_b);
        manager.stop_session(&session.id).unwrap();
    }

    #[test]
    fn duplicate_operation_key_is_not_resent() {
        let manager = manager();
        let dir = std::env::temp_dir();
        let workspace = manager.register_workspace(&dir.to_string_lossy()).unwrap();
        let session = manager.create_session(&workspace.id, "fake").unwrap();
        let first = manager
            .send_prompt(&session.id, "hi".into(), "same-key")
            .unwrap();
        let second = manager
            .send_prompt(&session.id, "hi".into(), "same-key")
            .unwrap();
        assert_eq!(first, second);
        manager.stop_session(&session.id).unwrap();
    }

    #[test]
    fn capabilities_are_data_not_hardcoded_in_manager() {
        let capabilities = ProviderCapabilities::default();
        assert!(!capabilities.approvals);
        assert_eq!(ProviderVersion::parse("2.3.2").minor, 3);
    }
}

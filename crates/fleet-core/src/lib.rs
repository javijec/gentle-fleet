//! SessionManager y timeline en memoria. No conoce proveedores concretos,
//! bases de datos, sockets ni clientes (D02, D06, P0-D6).
//!
//! P1.1: el bus está particionado por sesión, los turnos son propiedad del
//! SessionManager y las operaciones se reservan de forma atómica.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fleet_agent::{
    AgentDriver, AgentError, AgentEvent, AgentInput, DomainEvent, LaunchContext, ProviderRegistry,
};
use fleet_domain::{
    Event, EventKind, ProviderDescriptor, ProviderId, RunnerMode, SessionId, SessionState,
    TurnState, TurnStatus, WorkspaceId,
};

/// Capacidad por defecto de la cola de cada suscriptor.
pub const SUBSCRIBER_LIMIT: usize = 256;

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
    pub turn_state: TurnState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptReceipt {
    pub operation_id: String,
    pub session_id: SessionId,
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OperationStatus {
    Reserved,
    Succeeded,
    Failed { code: String, message: String },
}

struct OperationRecord {
    receipt: PromptReceipt,
    fingerprint: String,
    status: OperationStatus,
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
    active_turn: Option<String>,
    current_operation: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriberSignal {
    /// La cola alcanzó su límite; el suscriptor se desconecta con aviso.
    Overflow,
}

struct Subscriber {
    session_id: SessionId,
    tx: SyncSender<Event>,
    signal_tx: Sender<SubscriberSignal>,
    cancelled: Arc<AtomicBool>,
}

#[derive(Default)]
struct BusState {
    next_subscriber: u64,
    subscribers: HashMap<u64, Subscriber>,
}

/// Bus particionado por `SessionId`: un evento nunca sale de su sesión.
pub struct EventBus {
    state: Mutex<BusState>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self {
            state: Mutex::new(BusState::default()),
        }
    }
}

/// Suscripción con cola acotada y cancelación explícita.
pub struct Subscription {
    pub id: u64,
    pub session_id: SessionId,
    pub limit: usize,
    events: Receiver<Event>,
    signals: Receiver<SubscriberSignal>,
    pub cancelled: Arc<AtomicBool>,
}

/// Identidad barata de una suscripción, para que la conexión pueda cancelarla
/// mientras el hilo que reenvía eventos posee el receptor real.
pub struct SubscriptionHandle {
    pub id: u64,
    pub session_id: SessionId,
    pub cancelled: Arc<AtomicBool>,
}

impl SubscriptionHandle {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}

impl Subscription {
    pub fn handle(&self) -> SubscriptionHandle {
        SubscriptionHandle {
            id: self.id,
            session_id: self.session_id.clone(),
            cancelled: Arc::clone(&self.cancelled),
        }
    }

    pub fn recv_event(&self, timeout: Duration) -> Result<Option<Event>, RecvTimeoutError> {
        self.events.recv_timeout(timeout).map(Some)
    }

    pub fn recv_signal(
        &self,
        timeout: Duration,
    ) -> Result<Option<SubscriberSignal>, RecvTimeoutError> {
        self.signals.recv_timeout(timeout).map(Some)
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}

impl EventBus {
    pub fn subscribe(&self, session_id: &str, limit: usize) -> Subscription {
        let (tx, events) = mpsc::sync_channel(limit);
        let (signal_tx, signals) = mpsc::channel();
        let mut state = self.state.lock().expect("event bus poisoned");
        state.next_subscriber += 1;
        let id = state.next_subscriber;
        let cancelled = Arc::new(AtomicBool::new(false));
        state.subscribers.insert(
            id,
            Subscriber {
                session_id: session_id.to_string(),
                tx,
                signal_tx,
                cancelled: Arc::clone(&cancelled),
            },
        );
        Subscription {
            id,
            session_id: session_id.to_string(),
            limit,
            events,
            signals,
            cancelled,
        }
    }

    pub fn unsubscribe(&self, id: u64) {
        let mut state = self.state.lock().expect("event bus poisoned");
        state.subscribers.remove(&id);
    }

    /// Entrega el evento solo a suscriptores de su propia sesión. Un lento
    /// se desconecta con `SubscriberSignal::Overflow`, nunca en silencio.
    pub fn publish(&self, event: &Event) {
        let mut state = self.state.lock().expect("event bus poisoned");
        let mut overflowed = Vec::new();
        state.subscribers.retain(|id, subscriber| {
            if subscriber.cancelled.load(Ordering::SeqCst) {
                return false;
            }
            if subscriber.session_id != event.session_id {
                return true;
            }
            match subscriber.tx.try_send(event.clone()) {
                Ok(()) => true,
                Err(mpsc::TrySendError::Full(_)) => {
                    subscriber.cancelled.store(true, Ordering::SeqCst);
                    let _ = subscriber.signal_tx.send(SubscriberSignal::Overflow);
                    overflowed.push(*id);
                    false
                }
                Err(mpsc::TrySendError::Disconnected(_)) => false,
            }
        });
        let _ = overflowed;
    }

    pub fn subscriber_count(&self) -> usize {
        self.state
            .lock()
            .expect("event bus poisoned")
            .subscribers
            .len()
    }
}

#[derive(Default)]
struct ManagerState {
    workspaces: Vec<WorkspaceSummary>,
    sessions: HashMap<SessionId, SessionRecord>,
    operations: HashMap<String, OperationRecord>,
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

    pub fn provider_descriptors(&self) -> Vec<ProviderDescriptor> {
        self.registry.descriptors()
    }

    fn next_id_locked(state: &mut ManagerState, prefix: &str) -> String {
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
            id: Self::next_id_locked(&mut state, "ws"),
            path,
        };
        state.workspaces.push(workspace.clone());
        Ok(workspace)
    }

    pub fn list_sessions(&self) -> Vec<SessionSummary> {
        self.state
            .lock()
            .expect("manager poisoned")
            .sessions
            .values()
            .map(|session| session.summary.clone())
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
        driver.open(LaunchContext {
            workspace: workspace.path.clone(),
            mode: RunnerMode::Auto,
        })?;

        let summary = {
            let mut state = self.state.lock().expect("manager poisoned");
            let session_id = Self::next_id_locked(&mut state, "ses");
            let summary = SessionSummary {
                id: session_id.clone(),
                workspace_id: workspace.id.clone(),
                provider_id: provider_id.to_string(),
                state: SessionState::Running,
                turn_state: TurnState::Idle,
            };
            let (commands, rx) = mpsc::channel();
            let join = self.spawn_driver(session_id.clone(), driver, rx);
            state.sessions.insert(
                session_id.clone(),
                SessionRecord {
                    summary: summary.clone(),
                    timeline: Vec::new(),
                    next_seq: 1,
                    driver: Some(DriverHandle {
                        commands,
                        join: Some(join),
                    }),
                    active_turn: None,
                    current_operation: None,
                },
            );
            self.publish_locked(
                &mut state,
                &session_id,
                EventKind::SessionCreated {
                    provider_id: provider_id.to_string(),
                    workspace_id: workspace.id,
                },
            );
            self.publish_locked(
                &mut state,
                &session_id,
                EventKind::SessionStateChanged {
                    state: SessionState::Running,
                },
            );
            summary
        };
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
                    if let Err(error) = driver.send(AgentInput {
                        text,
                        turn_id: turn_id.clone(),
                    }) {
                        manager.fail_turn(&session_id, &turn_id, &error);
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
                Ok(Some(event)) => manager.publish_agent(&session_id, event),
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

    fn publish_locked(&self, state: &mut ManagerState, session_id: &str, kind: EventKind) {
        let record = state
            .sessions
            .get_mut(session_id)
            .expect("session disappeared while publishing");
        let seq = record.next_seq;
        record.next_seq += 1;
        let event = Event {
            event_id: format!("{session_id}-e{seq}"),
            session_id: session_id.to_string(),
            seq,
            created_at_ms: now_ms(),
            kind,
        };
        record.timeline.push(event.clone());
        // El bus se publica con el lock de sesión tomado: snapshot y streaming
        // no pueden perder ni duplicar eventos (P1.1).
        self.bus.publish(&event);
    }

    fn publish_local(&self, session_id: &str, kind: EventKind) {
        let mut state = self.state.lock().expect("manager poisoned");
        self.publish_locked(&mut state, session_id, kind);
    }

    fn fail_turn(&self, session_id: &str, turn_id: &str, error: &AgentError) {
        let mut state = self.state.lock().expect("manager poisoned");
        let operation_id = {
            let Some(record) = state.sessions.get_mut(session_id) else {
                return;
            };
            if record.active_turn.as_deref() != Some(turn_id) {
                return;
            }
            record.active_turn = None;
            record.summary.turn_state = TurnState::Failed;
            record.current_operation.take()
        };
        self.publish_locked(
            &mut state,
            session_id,
            EventKind::TurnCompleted {
                turn_id: turn_id.to_string(),
                status: TurnStatus::Failed,
            },
        );
        self.publish_locked(
            &mut state,
            session_id,
            EventKind::AgentError {
                message: error.to_string(),
            },
        );
        if let Some(operation_id) = operation_id {
            if let Some(operation) = state.operations.get_mut(&operation_id) {
                operation.status = OperationStatus::Failed {
                    code: error.code.clone(),
                    message: error.message.clone(),
                };
            }
        }
    }

    fn publish_agent(&self, session_id: &str, event: AgentEvent) {
        let mut state = self.state.lock().expect("manager poisoned");
        let (kind, operation_update) = {
            let Some(record) = state.sessions.get_mut(session_id) else {
                return;
            };
            let active = record.active_turn.clone();
            let mut update = None;
            let kind = match event.kind {
                // El turno lo inicia el SessionManager al reservar la operación.
                DomainEvent::TurnStarted { .. } => return,
                DomainEvent::AssistantDelta { turn_id, text } => {
                    if active.as_deref() != Some(&turn_id) {
                        return;
                    }
                    EventKind::AssistantDelta { turn_id, text }
                }
                DomainEvent::AssistantMessage { turn_id, text } => {
                    if active.as_deref() != Some(&turn_id) {
                        return;
                    }
                    EventKind::AssistantMessage { turn_id, text }
                }
                DomainEvent::ToolStarted {
                    turn_id,
                    name,
                    input,
                } => {
                    if active.as_deref() != Some(&turn_id) {
                        return;
                    }
                    EventKind::ToolStarted {
                        turn_id,
                        name,
                        input,
                    }
                }
                DomainEvent::ToolOutput {
                    turn_id,
                    name,
                    output,
                } => {
                    if active.as_deref() != Some(&turn_id) {
                        return;
                    }
                    EventKind::ToolOutput {
                        turn_id,
                        name,
                        output,
                    }
                }
                DomainEvent::ToolCompleted {
                    turn_id,
                    name,
                    success,
                } => {
                    if active.as_deref() != Some(&turn_id) {
                        return;
                    }
                    EventKind::ToolCompleted {
                        turn_id,
                        name,
                        success,
                    }
                }
                DomainEvent::TurnCompleted { turn_id, status } => {
                    if active.as_deref() != Some(&turn_id) {
                        return;
                    }
                    let terminal = match status {
                        TurnStatus::Completed => TurnState::Completed,
                        TurnStatus::Failed => TurnState::Failed,
                        TurnStatus::Interrupted => TurnState::Interrupted,
                    };
                    let operation_id = record.current_operation.take();
                    update = operation_id.map(|id| {
                        (
                            id,
                            if terminal == TurnState::Completed {
                                OperationStatus::Succeeded
                            } else {
                                OperationStatus::Failed {
                                    code: "turn_not_completed".into(),
                                    message: format!("turn ended as {terminal:?}"),
                                }
                            },
                        )
                    });
                    record.active_turn = None;
                    record.summary.turn_state = terminal;
                    EventKind::TurnCompleted { turn_id, status }
                }
                DomainEvent::TurnFailed { turn_id, message } => {
                    if active.as_deref() != Some(&turn_id) {
                        return;
                    }
                    update = record.current_operation.take().map(|id| {
                        (
                            id,
                            OperationStatus::Failed {
                                code: "turn_failed".into(),
                                message: message.clone(),
                            },
                        )
                    });
                    record.active_turn = None;
                    record.summary.turn_state = TurnState::Failed;
                    EventKind::TurnCompleted {
                        turn_id,
                        status: TurnStatus::Failed,
                    }
                }
                DomainEvent::AgentNotice { message } => EventKind::AgentNotice { message },
                DomainEvent::AgentError { message } => EventKind::AgentError { message },
            };
            (kind, update)
        };
        self.publish_locked(&mut state, session_id, kind);
        if let Some((operation_id, status)) = operation_update {
            if let Some(operation) = state.operations.get_mut(&operation_id) {
                operation.status = status;
            }
        }
    }

    /// Reserva la operación, valida el estado del turno y despacha una sola vez.
    pub fn send_prompt(
        &self,
        session_id: &str,
        text: String,
        operation_key: &str,
    ) -> Result<PromptReceipt, AgentError> {
        let text_for_command = text.clone();
        let fingerprint = format!("{session_id}\0{text}");
        let (command_tx, turn_id, operation_id, receipt) = {
            let mut state = self.state.lock().expect("manager poisoned");
            if let Some(existing) = state.operations.get(operation_key) {
                if existing.fingerprint == fingerprint {
                    return Ok(existing.receipt.clone());
                }
                return Err(AgentError {
                    code: "Conflict".into(),
                    message: format!(
                        "operation key `{operation_key}` was used with different arguments"
                    ),
                });
            }
            let turn_id = Self::next_id_locked(&mut state, "turn");
            let operation_id = Self::next_id_locked(&mut state, "op");
            let receipt = PromptReceipt {
                operation_id: operation_id.clone(),
                session_id: session_id.to_string(),
                turn_id: turn_id.clone(),
            };
            let command_tx = {
                let record = state
                    .sessions
                    .get_mut(session_id)
                    .ok_or_else(|| AgentError {
                        code: "not_found".into(),
                        message: format!("unknown session `{session_id}`"),
                    })?;
                if record.driver.is_none() || record.summary.state != SessionState::Running {
                    return Err(AgentError {
                        code: "session_stopped".into(),
                        message: format!("session `{session_id}` is stopped"),
                    });
                }
                if !record.summary.turn_state.accepts_prompt() {
                    return Err(AgentError {
                        code: "TurnAlreadyRunning".into(),
                        message: format!(
                            "session `{session_id}` has turn in state {:?}",
                            record.summary.turn_state
                        ),
                    });
                }
                let command_tx = record
                    .driver
                    .as_ref()
                    .expect("driver checked")
                    .commands
                    .clone();
                record.active_turn = Some(turn_id.clone());
                record.current_operation = Some(operation_id.clone());
                record.summary.turn_state = TurnState::Running;
                command_tx
            };
            // Reserva ANTES de despachar.
            state.operations.insert(
                operation_key.to_string(),
                OperationRecord {
                    receipt: receipt.clone(),
                    fingerprint,
                    status: OperationStatus::Reserved,
                },
            );
            self.publish_locked(
                &mut state,
                session_id,
                EventKind::TurnStarted {
                    turn_id: turn_id.clone(),
                },
            );
            self.publish_locked(
                &mut state,
                session_id,
                EventKind::UserMessage {
                    turn_id: turn_id.clone(),
                    text,
                },
            );
            (command_tx, turn_id, operation_id, receipt)
        };

        if let Err(error) = command_tx.send(DriverCommand::Prompt {
            text: text_for_command,
            turn_id,
        }) {
            self.fail_turn(
                session_id,
                &receipt.turn_id,
                &AgentError {
                    code: "DispatchFailed".into(),
                    message: format!("driver command channel closed: {error}"),
                },
            );
            return Err(AgentError {
                code: "DispatchFailed".into(),
                message: "prompt was not dispatched; no automatic retry will be attempted".into(),
            });
        }
        let _ = operation_id;
        Ok(receipt)
    }

    /// Suscribe antes de copiar el snapshot; publicaciones concurrentes quedan
    /// bloqueadas por el lock de sesión, así que no hay hueco ni duplicado.
    pub fn subscribe(
        &self,
        session_id: &str,
        since_seq: u64,
        limit: usize,
    ) -> Result<(Subscription, Vec<Event>), AgentError> {
        let state = self.state.lock().expect("manager poisoned");
        let record = state.sessions.get(session_id).ok_or_else(|| AgentError {
            code: "not_found".into(),
            message: format!("unknown session `{session_id}`"),
        })?;
        let subscription = self.bus.subscribe(session_id, limit);
        let snapshot = record
            .timeline
            .iter()
            .filter(|event| event.seq > since_seq)
            .cloned()
            .collect();
        Ok((subscription, snapshot))
    }

    /// Cierre acotado: nunca espera indefinidamente a un driver bloqueado.
    pub fn stop_session(&self, session_id: &str) -> Result<(), AgentError> {
        let (handle, active_turn, operation_id) = {
            let mut state = self.state.lock().expect("manager poisoned");
            let record = state
                .sessions
                .get_mut(session_id)
                .ok_or_else(|| AgentError {
                    code: "not_found".into(),
                    message: format!("unknown session `{session_id}`"),
                })?;
            let handle = record.driver.take().ok_or_else(|| AgentError {
                code: "session_stopped".into(),
                message: format!("session `{session_id}` already stopped"),
            })?;
            let active_turn = record.active_turn.take();
            let operation_id = record.current_operation.take();
            record.summary.state = SessionState::Stopped;
            if let Some(turn_id) = &active_turn {
                record.summary.turn_state = TurnState::Uncertain;
                self.publish_locked(
                    &mut state,
                    session_id,
                    EventKind::TurnUncertain {
                        turn_id: turn_id.clone(),
                        reason: "session stopped while turn was active".into(),
                    },
                );
            }
            self.publish_locked(
                &mut state,
                session_id,
                EventKind::SessionStateChanged {
                    state: SessionState::Stopped,
                },
            );
            if let Some(operation_id) = operation_id.clone() {
                if let Some(operation) = state.operations.get_mut(&operation_id) {
                    operation.status = OperationStatus::Failed {
                        code: "session_stopped".into(),
                        message: "session stopped before a terminal turn result".into(),
                    };
                }
            }
            (handle, active_turn, operation_id)
        };
        let _ = (active_turn, operation_id);
        let (done_tx, done_rx) = mpsc::channel();
        if handle
            .commands
            .send(DriverCommand::Close { done: done_tx })
            .is_err()
            || done_rx.recv_timeout(Duration::from_millis(500)).is_err()
        {
            self.publish_local(
                session_id,
                EventKind::AgentNotice {
                    message: "driver did not acknowledge close within 500ms; thread detached"
                        .into(),
                },
            );
        }
        // No se hace join incondicional: un driver bloqueado no debe colgar el cierre.
        drop(handle.join);
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
    use fleet_agent::fake::{FakeConfig, FakeDriver, FakeFactory};
    use fleet_agent::DriverFactory;
    use fleet_domain::{ProviderCapabilities, ProviderVersion, VersionRange};

    struct ConfiguredFactory(FakeConfig);

    impl DriverFactory for ConfiguredFactory {
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
            FakeFactory::new().metadata()
        }
    }

    fn manager(config: FakeConfig) -> Arc<SessionManager> {
        let mut registry = ProviderRegistry::new();
        registry.register(Box::new(ConfiguredFactory(config)));
        Arc::new(SessionManager::new(registry))
    }

    fn workspace(manager: &SessionManager) -> WorkspaceSummary {
        manager
            .register_workspace(&std::env::temp_dir().to_string_lossy())
            .unwrap()
    }

    fn session(manager: &Arc<SessionManager>, workspace_id: &str) -> SessionSummary {
        manager.create_session(workspace_id, "fake").unwrap()
    }

    fn collect_terminal(subscription: &Subscription) -> Vec<Event> {
        let mut events = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match subscription.recv_event(Duration::from_millis(100)) {
                Ok(Some(event)) => {
                    let terminal = matches!(event.kind, EventKind::TurnCompleted { .. });
                    events.push(event);
                    if terminal {
                        return events;
                    }
                }
                Ok(None) => {}
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        panic!("turn did not reach a terminal event");
    }

    #[test]
    fn sessions_are_isolated_and_sequences_are_per_session() {
        let manager = manager(FakeConfig::default());
        let ws = workspace(&manager);
        let a = session(&manager, &ws.id);
        let b = session(&manager, &ws.id);
        let (sub_a, _) = manager.subscribe(&a.id, 0, SUBSCRIBER_LIMIT).unwrap();
        let (sub_b, _) = manager.subscribe(&b.id, 0, SUBSCRIBER_LIMIT).unwrap();
        manager.send_prompt(&a.id, "only A".into(), "op-a").unwrap();

        let events_a = collect_terminal(&sub_a);
        assert!(events_a.iter().all(|event| event.session_id == a.id));
        assert_eq!(events_a.first().unwrap().seq, 3);
        let idle_b = matches!(
            sub_b.recv_event(Duration::from_millis(120)),
            Ok(None) | Err(RecvTimeoutError::Timeout)
        );
        assert!(idle_b, "session B received events from session A");
        manager.stop_session(&a.id).unwrap();
        manager.stop_session(&b.id).unwrap();
    }

    #[test]
    fn two_subscribers_see_the_same_sequence() {
        let manager = manager(FakeConfig::default());
        let ws = workspace(&manager);
        let session = session(&manager, &ws.id);
        let (sub_a, snapshot_a) = manager.subscribe(&session.id, 0, SUBSCRIBER_LIMIT).unwrap();
        let (sub_b, snapshot_b) = manager.subscribe(&session.id, 0, SUBSCRIBER_LIMIT).unwrap();
        assert_eq!(snapshot_a, snapshot_b);
        assert_eq!(snapshot_a.len(), 2);

        manager
            .send_prompt(&session.id, "hi".into(), "op-1")
            .unwrap();
        let events_a = collect_terminal(&sub_a);
        let events_b = collect_terminal(&sub_b);
        assert_eq!(
            events_a.iter().map(|e| e.seq).collect::<Vec<_>>(),
            events_b.iter().map(|e| e.seq).collect::<Vec<_>>()
        );
        assert_eq!(events_a.first().unwrap().seq, 3);
        sub_a.cancel();
        sub_b.cancel();
        manager.stop_session(&session.id).unwrap();
    }

    #[test]
    fn second_prompt_returns_turn_already_running() {
        let manager = manager(FakeConfig::default());
        let ws = workspace(&manager);
        let session = session(&manager, &ws.id);
        let (_sub, _) = manager.subscribe(&session.id, 0, SUBSCRIBER_LIMIT).unwrap();
        manager
            .send_prompt(&session.id, "first".into(), "op-1")
            .unwrap();
        let error = manager
            .send_prompt(&session.id, "second".into(), "op-2")
            .unwrap_err();
        assert_eq!(error.code, "TurnAlreadyRunning");
        manager.stop_session(&session.id).unwrap();
    }

    #[test]
    fn operation_key_conflict_and_reservation_are_atomic() {
        let manager = manager(FakeConfig::default());
        let ws = workspace(&manager);
        let session = session(&manager, &ws.id);
        let (sub, _) = manager.subscribe(&session.id, 0, SUBSCRIBER_LIMIT).unwrap();
        let first = manager
            .send_prompt(&session.id, "hi".into(), "same-key")
            .unwrap();
        let same = manager
            .send_prompt(&session.id, "hi".into(), "same-key")
            .unwrap();
        assert_eq!(first, same);
        let conflict = manager
            .send_prompt(&session.id, "different".into(), "same-key")
            .unwrap_err();
        assert_eq!(conflict.code, "Conflict");
        let _ = collect_terminal(&sub);
        manager.stop_session(&session.id).unwrap();
    }

    #[test]
    fn key_cannot_be_reused_for_another_session() {
        let manager = manager(FakeConfig::default());
        let ws = workspace(&manager);
        let a = session(&manager, &ws.id);
        let b = session(&manager, &ws.id);
        manager
            .send_prompt(&a.id, "hi".into(), "global-key")
            .unwrap();
        let error = manager
            .send_prompt(&b.id, "hi".into(), "global-key")
            .unwrap_err();
        assert_eq!(error.code, "Conflict");
        manager.stop_session(&a.id).unwrap();
        manager.stop_session(&b.id).unwrap();
    }

    #[test]
    fn provider_error_is_not_a_terminal_turn() {
        let manager = manager(FakeConfig {
            emit_provider_error: true,
            ..FakeConfig::default()
        });
        let ws = workspace(&manager);
        let session = session(&manager, &ws.id);
        let (sub, _) = manager.subscribe(&session.id, 0, SUBSCRIBER_LIMIT).unwrap();
        manager
            .send_prompt(&session.id, "hi".into(), "op-1")
            .unwrap();
        let events = collect_terminal(&sub);
        assert!(events
            .iter()
            .any(|event| matches!(event.kind, EventKind::AgentNotice { .. })));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, EventKind::TurnCompleted { .. }))
                .count(),
            1
        );
        manager.stop_session(&session.id).unwrap();
    }

    #[test]
    fn terminal_failure_from_driver_is_reported() {
        let manager = manager(FakeConfig {
            emit_terminal_failure: true,
            ..FakeConfig::default()
        });
        let ws = workspace(&manager);
        let session = session(&manager, &ws.id);
        let (sub, _) = manager.subscribe(&session.id, 0, SUBSCRIBER_LIMIT).unwrap();
        manager
            .send_prompt(&session.id, "hi".into(), "op-1")
            .unwrap();
        let events = collect_terminal(&sub);
        assert!(events.iter().any(|event| matches!(
            event.kind,
            EventKind::TurnCompleted {
                status: TurnStatus::Failed,
                ..
            }
        )));
        assert_eq!(manager.list_sessions()[0].turn_state, TurnState::Failed);
        manager.stop_session(&session.id).unwrap();
    }

    #[test]
    fn slow_subscriber_is_disconnected_with_overflow_signal() {
        let bus = EventBus::default();
        let subscription = bus.subscribe("ses_slow", 2);
        let other = bus.subscribe("ses_other", 2);
        for seq in 1..=4 {
            bus.publish(&Event {
                event_id: format!("e{seq}"),
                session_id: "ses_slow".into(),
                seq,
                created_at_ms: 0,
                kind: EventKind::AgentNotice {
                    message: "flood".into(),
                },
            });
        }
        assert_eq!(
            subscription
                .recv_signal(Duration::from_millis(100))
                .unwrap(),
            Some(SubscriberSignal::Overflow)
        );
        assert_eq!(bus.subscriber_count(), 1);
        assert_eq!(other.session_id, "ses_other");
    }

    #[test]
    fn stop_with_blocked_driver_is_bounded() {
        let manager = manager(FakeConfig {
            block_close: true,
            ..FakeConfig::default()
        });
        let ws = workspace(&manager);
        let session = session(&manager, &ws.id);
        let (sub, _) = manager.subscribe(&session.id, 0, SUBSCRIBER_LIMIT).unwrap();
        manager
            .send_prompt(&session.id, "hi".into(), "op-1")
            .unwrap();
        let _ = collect_terminal(&sub);
        let started = std::time::Instant::now();
        manager.stop_session(&session.id).unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(manager.list_sessions()[0].state, SessionState::Stopped);
    }

    #[test]
    fn descriptors_come_from_registry() {
        let manager = manager(FakeConfig::default());
        let descriptors = manager.provider_descriptors();
        assert_eq!(descriptors.len(), 1);
        assert_eq!(descriptors[0].id, "fake");
        assert_eq!(descriptors[0].version, ProviderVersion::parse("0.1.0"));
        assert_eq!(
            descriptors[0].capabilities,
            ProviderCapabilities {
                streaming_text: true,
                tool_events: true,
                session_history: true,
                native_turn_ids: true,
                cancel_turn: false,
                approvals: false,
                can_own_process: false,
                can_attach_service: false,
            }
        );
    }
}

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
use fleet_domain::store::{
    ApprovalClaim, ApprovalRequest, ApprovalStatus, OperationKind, OperationRecord, OperationReservation,
    OperationStatus, SessionStore, StoreError, TurnRecord,
};
use fleet_domain::{
    Event, EventKind, ProviderDescriptor, ProviderId, RunnerMode, SessionId, SessionState,
    TurnState, TurnStatus, WorkspaceId,
};

/// Capacidad por defecto de la cola de cada suscriptor.
pub const SUBSCRIBER_LIMIT: usize = 256;
/// Página máxima devuelta por una lectura de timeline.
pub const TIMELINE_PAGE: usize = 256;

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

enum DriverCommand {
    Prompt {
        text: String,
        turn_id: String,
    },
    Answer {
        approval_id: String,
        native_request_id: String,
        decision: String,
    },
    Close {
        done: Sender<Result<(), AgentError>>,
    },
}

struct DriverHandle {
    commands: Sender<DriverCommand>,
    join: Option<JoinHandle<()>>,
}

struct SessionRecord {
    summary: SessionSummary,
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
                    // Comunicar Overflow antes de cancelar, para no perder el motivo.
                    let _ = subscriber.signal_tx.send(SubscriberSignal::Overflow);
                    subscriber.cancelled.store(true, Ordering::SeqCst);
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
    approvals: HashMap<String, ApprovalRequest>,
    next_ids: u64,
}

pub struct SessionManager {
    registry: ProviderRegistry,
    store: Arc<dyn SessionStore>,
    bus: EventBus,
    state: Mutex<ManagerState>,
}

impl SessionManager {
    pub fn new(
        registry: ProviderRegistry,
        store: Arc<dyn SessionStore>,
    ) -> Result<Self, AgentError> {
        let manager = Self {
            registry,
            store,
            bus: EventBus::default(),
            state: Mutex::new(ManagerState {
                workspaces: Vec::new(),
                sessions: HashMap::new(),
                approvals: HashMap::new(),
                next_ids: 0,
            }),
        };
        manager.load_from_store()?;
        Ok(manager)
    }

    fn load_from_store(&self) -> Result<(), AgentError> {
        let workspaces = self.store.list_workspaces().map_err(to_agent_error)?;
        let sessions = self.store.list_sessions().map_err(to_agent_error)?;
        let approvals = self.store.list_approvals(None).map_err(to_agent_error)?;
        let mut state = self.state.lock().expect("manager poisoned");
        // Tras reinicio, continuar en 0 colisionaría con IDs ya persistidos.
        if !sessions.is_empty() {
            state.next_ids = now_ms().saturating_mul(1000);
        }
        state.workspaces = workspaces
            .into_iter()
            .map(|workspace| WorkspaceSummary {
                id: workspace.id,
                path: PathBuf::from(workspace.path),
            })
            .collect();
        for stored in sessions {
            let next_seq = self
                .store
                .max_seq(&stored.session_id)
                .map_err(to_agent_error)?
                + 1;
            // Sin runner verificado, una sesión antigua no puede fingir que sigue ejecutándose.
            let (session_state, turn_state) = match stored.state {
                SessionState::Running | SessionState::Stopping | SessionState::StopUnconfirmed => {
                    (SessionState::NeedsReconciliation, TurnState::Uncertain)
                }
                other => (other, stored.turn_state),
            };
            state.sessions.insert(
                stored.session_id.clone(),
                SessionRecord {
                    summary: SessionSummary {
                        id: stored.session_id,
                        workspace_id: stored.workspace_id,
                        provider_id: stored.provider_id,
                        state: session_state,
                        turn_state,
                    },
                    next_seq,
                    driver: None,
                    active_turn: None,
                    current_operation: None,
                },
            );
        }
        state.approvals = approvals
            .into_iter()
            .map(|approval| (approval.approval_id.clone(), approval))
            .collect();
        Ok(())
    }

    pub fn store_health(&self) -> Result<fleet_domain::store::StoreHealth, AgentError> {
        self.store.health().map_err(to_agent_error)
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

    fn next_id(&self, prefix: &str) -> String {
        let mut state = self.state.lock().expect("manager poisoned");
        Self::next_id_locked(&mut state, prefix)
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
        self.store
            .save_workspace(&fleet_domain::store::WorkspaceRecord {
                id: workspace.id.clone(),
                path: workspace.path.to_string_lossy().into_owned(),
            })
            .map_err(to_agent_error)?;
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
            let next_seq = self.store.max_seq(&session_id).map_err(to_agent_error)? + 1;
            self.store
                .save_session(&fleet_domain::store::SessionRecord {
                    session_id: session_id.clone(),
                    workspace_id: workspace.id.clone(),
                    provider_id: provider_id.to_string(),
                    state: SessionState::Running,
                    turn_state: TurnState::Idle,
                    created_at_ms: now_ms(),
                    updated_at_ms: now_ms(),
                })
                .map_err(to_agent_error)?;
            let (commands, rx) = mpsc::channel();
            let join = self.spawn_driver(session_id.clone(), driver, rx);
            state.sessions.insert(
                session_id.clone(),
                SessionRecord {
                    summary: summary.clone(),
                    next_seq,
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
            )?;
            self.publish_locked(
                &mut state,
                &session_id,
                EventKind::SessionStateChanged {
                    state: SessionState::Running,
                },
            )?;
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
                Ok(DriverCommand::Answer {
                    approval_id,
                    native_request_id,
                    decision,
                }) => {
                    if let Err(error) = driver.answer(native_request_id, decision) {
                        manager.mark_approval_uncertain(&session_id, &approval_id, &error);
                    }
                }
                Ok(DriverCommand::Close { done }) => {
                    let outcome = driver.close();
                    manager.finish_stop(&session_id, &outcome);
                    let _ = done.send(outcome);
                    break;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            match driver.next_event(Duration::from_millis(50)) {
                Ok(Some(event)) => {
                    if let Err(error) = manager.publish_agent(&session_id, event) {
                        eprintln!("agent event could not be persisted: {error}");
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    let _ = manager.publish_local(
                        &session_id,
                        EventKind::AgentError {
                            message: error.to_string(),
                        },
                    );
                }
            }
        })
    }

    /// The event, session projection and any turn/approval/operation transition
    /// are durable in ONE SQLite transaction before any observer receives it.
    fn publish_locked(
        &self,
        state: &mut ManagerState,
        session_id: &str,
        kind: EventKind,
    ) -> Result<(), AgentError> {
        self.publish_locked_with_approval(state, session_id, kind, None)
    }

    fn publish_locked_with_approval(
        &self,
        state: &mut ManagerState,
        session_id: &str,
        kind: EventKind,
        approval: Option<&ApprovalRequest>,
    ) -> Result<(), AgentError> {
        let record = state
            .sessions
            .get_mut(session_id)
            .expect("session disappeared while publishing");
        let seq = record.next_seq;
        let event = Event {
            event_id: format!("{session_id}-e{seq}"),
            session_id: session_id.to_string(),
            seq,
            created_at_ms: now_ms(),
            kind,
        };
        let projection = fleet_domain::store::SessionRecord {
            session_id: session_id.to_string(),
            workspace_id: record.summary.workspace_id.clone(),
            provider_id: record.summary.provider_id.clone(),
            state: record.summary.state,
            turn_state: record.summary.turn_state,
            created_at_ms: now_ms(),
            updated_at_ms: now_ms(),
        };
        self.store
            .commit_event(&event, &projection, approval)
            .map_err(to_agent_error)?;
        // Never advance seq on a failed commit: replay must remain gap-free.
        record.next_seq += 1;
        self.bus.publish(&event);
        Ok(())
    }

    fn publish_local(&self, session_id: &str, kind: EventKind) -> Result<(), AgentError> {
        let mut state = self.state.lock().expect("manager poisoned");
        self.publish_locked(&mut state, session_id, kind)
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
        let _ = self.publish_locked(
            &mut state,
            session_id,
            EventKind::TurnCompleted {
                turn_id: turn_id.to_string(),
                status: TurnStatus::Failed,
            },
        );
        let _ = self.publish_locked(
            &mut state,
            session_id,
            EventKind::AgentError {
                message: error.to_string(),
            },
        );
        drop(state);
        if let Some(operation_id) = operation_id {
            let _ = self.store.update_operation(
                &operation_id,
                OperationStatus::Failed,
                Some(error.message.clone()),
            );
        }
    }

    fn publish_agent(&self, session_id: &str, event: AgentEvent) -> Result<(), AgentError> {
        let next_approval_id = self.next_id("appr");
        let mut state = self.state.lock().expect("manager poisoned");
        let (kind, _operation_update, new_approval, approval_update) = {
            let Some(record) = state.sessions.get_mut(session_id) else {
                return Ok(());
            };
            let active = record.active_turn.clone();
            let mut operation_update = None;
            let mut new_approval = None;
            let mut approval_update = None;
            let scoped_turn = |turn_id: &str| -> bool { active.as_deref() == Some(turn_id) };
            let kind = match event.kind {
                DomainEvent::TurnStarted { .. } => return Ok(()),
                DomainEvent::AssistantDelta { turn_id, text } => {
                    if !scoped_turn(&turn_id) {
                        return Ok(());
                    }
                    EventKind::AssistantDelta { turn_id, text }
                }
                DomainEvent::AssistantMessage { turn_id, text } => {
                    if !scoped_turn(&turn_id) {
                        return Ok(());
                    }
                    EventKind::AssistantMessage { turn_id, text }
                }
                DomainEvent::ToolStarted {
                    turn_id,
                    name,
                    input,
                } => {
                    if !scoped_turn(&turn_id) {
                        return Ok(());
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
                    if !scoped_turn(&turn_id) {
                        return Ok(());
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
                    if !scoped_turn(&turn_id) {
                        return Ok(());
                    }
                    EventKind::ToolCompleted {
                        turn_id,
                        name,
                        success,
                    }
                }
                DomainEvent::TurnCompleted { turn_id, status } => {
                    if !scoped_turn(&turn_id) {
                        return Ok(());
                    }
                    let terminal = match status {
                        TurnStatus::Completed => TurnState::Completed,
                        TurnStatus::Failed => TurnState::Failed,
                        TurnStatus::Interrupted => TurnState::Interrupted,
                    };
                    operation_update = record.current_operation.take().map(|id| {
                        if terminal == TurnState::Completed {
                            (id, OperationStatus::Completed, None)
                        } else {
                            (
                                id,
                                OperationStatus::Failed,
                                Some(format!("turn ended as {terminal:?}")),
                            )
                        }
                    });
                    record.active_turn = None;
                    record.summary.turn_state = terminal;
                    EventKind::TurnCompleted { turn_id, status }
                }
                DomainEvent::TurnFailed { turn_id, message } => {
                    if !scoped_turn(&turn_id) {
                        return Ok(());
                    }
                    operation_update = record
                        .current_operation
                        .take()
                        .map(|id| (id, OperationStatus::Failed, Some(message.clone())));
                    record.active_turn = None;
                    record.summary.turn_state = TurnState::Failed;
                    EventKind::TurnCompleted {
                        turn_id,
                        status: TurnStatus::Failed,
                    }
                }
                DomainEvent::ApprovalRequested {
                    native_request_id,
                    kind,
                    message,
                    resources,
                    allowed_decisions,
                } => {
                    let Some(turn_id) = active.clone() else {
                        return Ok(());
                    };
                    let approval_id = next_approval_id;
                    new_approval = Some(ApprovalRequest {
                        approval_id: approval_id.clone(),
                        session_id: session_id.to_string(),
                        turn_id: turn_id.clone(),
                        kind: kind.clone(),
                        message: message.clone(),
                        resources: resources.clone(),
                        allowed_decisions: allowed_decisions.clone(),
                        status: ApprovalStatus::Pending,
                        native_request_id,
                        decision: None,
                        created_at_ms: now_ms(),
                        resolved_at_ms: None,
                    });
                    record.summary.turn_state = TurnState::AwaitingApproval;
                    EventKind::ApprovalRequested {
                        approval_id,
                        turn_id,
                        kind,
                        message,
                        resources,
                        allowed_decisions,
                    }
                }
                DomainEvent::ApprovalResolved {
                    native_request_id,
                    decision,
                } => {
                    let Some(approval) = state
                        .approvals
                        .values()
                        .find(|a| a.native_request_id == native_request_id)
                        .cloned()
                    else {
                        return Ok(());
                    };
                    if approval.status != ApprovalStatus::Dispatching
                        || approval.decision.as_deref() != Some(decision.as_str())
                        || approval.session_id != session_id
                    {
                        return Ok(());
                    }
                    let status = if decision == "reject" {
                        ApprovalStatus::Rejected
                    } else {
                        ApprovalStatus::Accepted
                    };
                    approval_update = Some((approval.clone(), status));
                    EventKind::ApprovalResolved {
                        approval_id: approval.approval_id,
                        turn_id: approval.turn_id,
                        decision,
                    }
                }
                DomainEvent::AgentNotice { message } => EventKind::AgentNotice { message },
                DomainEvent::AgentError { message } => EventKind::AgentError { message },
            };
            (kind, operation_update, new_approval, approval_update)
        };

        // Keep the manager lock across the native event transition; no other
        // client may change the approval between checking and committing it.
        self.publish_locked_with_approval(
            &mut state,
            session_id,
            kind,
            new_approval.as_ref(),
        )?;
        if let Some(approval) = new_approval {
            state.approvals.insert(approval.approval_id.clone(), approval);
        }
        if let Some((approval, status)) = approval_update {
            let mut resolved = approval;
            resolved.status = status;
            resolved.resolved_at_ms = Some(now_ms());
            state.approvals.insert(resolved.approval_id.clone(), resolved);
        }
        Ok(())
    }

    pub fn list_approvals(&self, session_id: Option<&str>) -> Vec<ApprovalRequest> {
        let state = self.state.lock().expect("manager poisoned");
        state
            .approvals
            .values()
            .filter(|approval| session_id.is_none_or(|id| approval.session_id == id))
            .cloned()
            .collect()
    }

    pub fn get_approval(&self, approval_id: &str) -> Option<ApprovalRequest> {
        self.state
            .lock()
            .expect("manager poisoned")
            .approvals
            .get(approval_id)
            .cloned()
    }

    pub fn get_operation(
        &self,
        operation_key: &str,
    ) -> Result<Option<OperationRecord>, AgentError> {
        self.store
            .get_operation(operation_key)
            .map_err(to_agent_error)
    }

    /// Página durable desde un cursor; no carga la timeline completa en RAM.
    pub fn read_timeline(
        &self,
        session_id: &str,
        since_seq: u64,
        limit: usize,
    ) -> Result<Vec<Event>, AgentError> {
        let limit = limit.clamp(1, TIMELINE_PAGE);
        self.store
            .read_events(session_id, since_seq, limit)
            .map_err(to_agent_error)
    }

    /// Atomically claim the approval and its operation. The HTTP result is
    /// NOT known here: only a native driver event can confirm final delivery.
    pub fn answer_approval(
        &self,
        approval_id: &str,
        decision: String,
        operation_key: &str,
    ) -> Result<ApprovalRequest, AgentError> {
        let mut state = self.state.lock().expect("manager poisoned");
        let approval = state.approvals.get(approval_id).cloned().ok_or_else(|| AgentError {
            code: "not_found".into(),
            message: format!("unknown approval `{approval_id}`"),
        })?;
        if !approval.allowed_decisions.contains(&decision) {
            return Err(AgentError {
                code: "InvalidDecision".into(),
                message: format!("`{decision}` is not allowed for `{approval_id}`"),
            });
        }
        let operation = OperationRecord {
            operation_id: Self::next_id_locked(&mut state, "op"),
            operation_key: operation_key.to_string(),
            kind: OperationKind::AnswerApproval,
            session_id: approval.session_id.clone(),
            target_id: Some(approval_id.to_string()),
            fingerprint: format!("{}\0{}\0{}", approval.session_id, approval_id, decision),
            status: OperationStatus::Reserved,
            message: None,
            created_at_ms: now_ms(),
            updated_at_ms: now_ms(),
        };
        let claimed = self.store
            .claim_approval(approval_id, &decision, &operation)
            .map_err(to_agent_error)?;
        let updated = match claimed {
            ApprovalClaim::Existing(existing, approval) => {
                if existing.status == OperationStatus::Uncertain {
                    return Err(AgentError {
                        code: "NeedsReconciliation".into(),
                        message: "approval delivery uncertain; no automatic retry".into(),
                    });
                }
                return Ok(approval); // same operation key: no second dispatch
            }
            ApprovalClaim::Conflict => return Err(AgentError {
                code: "Conflict".into(),
                message: "approval already claimed or operation key has different parameters".into(),
            }),
            ApprovalClaim::Created(updated) => updated,
        };
        state.approvals.insert(approval_id.to_string(), updated.clone());
        let command_tx = state.sessions.get(&approval.session_id)
            .and_then(|record| record.driver.as_ref().map(|d| d.commands.clone()));
        drop(state);

        let result = command_tx.ok_or_else(|| AgentError {
            code: "driver_unavailable".into(),
            message: "no live driver to deliver approval".into(),
        }).and_then(|tx| tx.send(DriverCommand::Answer {
            approval_id: approval_id.to_string(),
            native_request_id: approval.native_request_id.clone(),
            decision: decision.clone(),
        }).map_err(|error| AgentError {
            code: "approval_delivery_failed".into(),
            message: error.to_string(),
        }));
        if let Err(error) = result {
            self.mark_approval_uncertain(&approval.session_id, approval_id, &error);
            return Err(AgentError {
                code: "NeedsReconciliation".into(),
                message: error.message,
            });
        }
        Ok(updated) // Dispatching: NOT Accepted/Rejected until native confirmation
    }

    pub fn mark_approval_uncertain(&self, session_id: &str, approval_id: &str, error: &AgentError) {
        if let Err(store_error) = self.store.mark_approval_uncertain(approval_id, &error.message) {
            eprintln!("could not persist uncertain approval: {store_error:?}");
        }
        if let Some(approval) = self.state.lock().expect("manager poisoned")
            .approvals.get_mut(approval_id)
        {
            approval.status = ApprovalStatus::Uncertain;
        }
        let _ = self.publish_local(
            session_id,
            EventKind::AgentError {
                message: format!("approval decision delivery uncertain: {error}"),
            },
        );
    }

    /// Validate before reserving; a rejected prompt never consumes its key.
    /// Reservation and session state check are serialized by the manager lock.
    pub fn send_prompt(
        &self,
        session_id: &str,
        text: String,
        operation_key: &str,
    ) -> Result<PromptReceipt, AgentError> {
        let mut state = self.state.lock().expect("manager poisoned");
        let fingerprint = format!("{session_id}\0{text}");
        // Idempotent responses must work even after a daemon restart when the
        // session has no runner. They never dispatch the original prompt again.
        if let Some(existing) = self.store.get_operation(operation_key).map_err(to_agent_error)? {
            if existing.kind != OperationKind::SendPrompt
                || existing.session_id != session_id
                || existing.fingerprint != fingerprint
            {
                return Err(AgentError {
                    code: "Conflict".into(),
                    message: "operation key was used with different arguments".into(),
                });
            }
            if existing.status == OperationStatus::Uncertain
                || existing.status == OperationStatus::Reserved
            {
                return Err(AgentError {
                    code: "NeedsReconciliation".into(),
                    message: "previous prompt outcome is uncertain; no automatic retry".into(),
                });
            }
            if existing.status == OperationStatus::Failed {
                return Err(AgentError {
                    code: "PreviousOperationFailed".into(),
                    message: existing.message.unwrap_or_else(|| "previous dispatch failed".into()),
                });
            }
            return Ok(PromptReceipt {
                operation_id: existing.operation_id,
                session_id: existing.session_id,
                turn_id: existing.target_id.unwrap_or_default(),
            });
        }
        {
            let record = state.sessions.get(session_id).ok_or_else(|| AgentError {
                code: "not_found".into(),
                message: format!("unknown session `{session_id}`"),
            })?;
            if record.driver.is_none() || record.summary.state != SessionState::Running {
                return Err(AgentError {
                    code: "session_stopped".into(),
                    message: format!("session `{session_id}` does not have a running driver"),
                });
            }
            if !record.summary.turn_state.accepts_prompt() {
                return Err(AgentError {
                    code: "TurnAlreadyRunning".into(),
                    message: format!("session `{session_id}` has active turn"),
                });
            }
        }
        let turn_id = Self::next_id_locked(&mut state, "turn");
        let operation_id = Self::next_id_locked(&mut state, "op");
        let operation = OperationRecord {
            operation_id: operation_id.clone(),
            operation_key: operation_key.to_string(),
            kind: OperationKind::SendPrompt,
            session_id: session_id.to_string(),
            target_id: Some(turn_id.clone()),
            fingerprint,
            status: OperationStatus::Reserved,
            message: None,
            created_at_ms: now_ms(),
            updated_at_ms: now_ms(),
        };
        match self.store.reserve_operation(&operation).map_err(to_agent_error)? {
            OperationReservation::Created(_) => {}
            OperationReservation::Conflict => return Err(AgentError {
                code: "Conflict".into(),
                message: "operation key belongs to a different request".into(),
            }),
            OperationReservation::Existing(existing) => {
                return Ok(PromptReceipt {
                    operation_id: existing.operation_id,
                    session_id: existing.session_id,
                    turn_id: existing.target_id.unwrap_or_default(),
                });
            }
        }
        // A failed turn insert cannot leave an operation pretending to have
        // been accepted: explicitly mark it failed before returning.
        if let Err(error) = self.store.save_turn(&TurnRecord {
            turn_id: turn_id.clone(),
            session_id: session_id.to_string(),
            operation_id: operation_id.clone(),
            state: TurnState::Running,
            created_at_ms: now_ms(),
            updated_at_ms: now_ms(),
        }) {
            self.store.update_operation(
                &operation_id,
                OperationStatus::Failed,
                Some("failed to persist turn before dispatch".into()),
            ).map_err(to_agent_error)?;
            return Err(to_agent_error(error));
        }
        let command_tx = {
            let record = state.sessions.get_mut(session_id).expect("session checked");
            record.active_turn = Some(turn_id.clone());
            record.current_operation = Some(operation_id.clone());
            record.summary.turn_state = TurnState::Running;
            record.driver.as_ref().expect("driver checked").commands.clone()
        };
        // The prompt cannot reach the provider before both startup events are
        // committed. A failure makes the operation uncertain, never replayable.
        for kind in [
            EventKind::TurnStarted { turn_id: turn_id.clone() },
            EventKind::UserMessage { turn_id: turn_id.clone(), text: text.clone() },
        ] {
            if let Err(error) = self.publish_locked(&mut state, session_id, kind) {
                self.store.update_operation(
                    &operation_id, OperationStatus::Uncertain,
                    Some("prompt events were not fully persisted".into()),
                ).map_err(to_agent_error)?;
                if let Some(record) = state.sessions.get_mut(session_id) {
                    record.summary.turn_state = TurnState::Uncertain;
                    record.active_turn = None;
                    record.current_operation = None;
                }
                return Err(error);
            }
        }
        self.store.update_operation(
            &operation_id, OperationStatus::Accepted, None,
        ).map_err(to_agent_error)?;
        drop(state);
        if let Err(error) = command_tx.send(DriverCommand::Prompt {
            text,
            turn_id: turn_id.clone(),
        }) {
            self.fail_turn(
                session_id,
                &turn_id,
                &AgentError {
                    code: "DispatchFailed".into(),
                    message: format!("driver command channel closed: {error}"),
                },
            );
            return Err(AgentError {
                code: "DispatchFailed".into(),
                message: "prompt was not dispatched; it will not be retried automatically".into(),
            });
        }
        Ok(PromptReceipt {
            operation_id,
            session_id: session_id.to_string(),
            turn_id,
        })
    }

    /// Suscribe antes de copiar el snapshot; publicaciones concurrentes quedan
    /// bloqueadas por el lock de sesión, así que no hay hueco ni duplicado.
    pub fn subscribe(
        &self,
        session_id: &str,
        since_seq: u64,
        limit: usize,
    ) -> Result<(Subscription, Vec<Event>, u64), AgentError> {
        let state = self.state.lock().expect("manager poisoned");
        if !state.sessions.contains_key(session_id) {
            return Err(AgentError {
                code: "not_found".into(),
                message: format!("unknown session `{session_id}`"),
            });
        }
        let cutoff = self.store.max_seq(session_id).map_err(to_agent_error)?;
        if since_seq > cutoff {
            return Err(AgentError {
                code: "InvalidCursor".into(),
                message: format!("cursor {since_seq} is ahead of persisted seq {cutoff}"),
            });
        }
        let subscription = self.bus.subscribe(session_id, limit);
        let snapshot = match self.store.read_events(session_id, since_seq, TIMELINE_PAGE) {
            Ok(events) => events,
            Err(error) => {
                self.bus.unsubscribe(subscription.id);
                return Err(to_agent_error(error));
            }
        };
        // Capture the durable high water mark under the publication lock:
        // the daemon can page to this point before consuming buffered live events.
        Ok((subscription, snapshot, cutoff))
    }

    fn persist_session_state(&self, session_id: &str) {
        let state = self.state.lock().expect("manager poisoned");
        let _ = self.persist_session_state_locked(&state, session_id);
    }

    fn persist_session_state_locked(
        &self,
        state: &ManagerState,
        session_id: &str,
    ) -> Result<(), AgentError> {
        let record = state.sessions.get(session_id).ok_or_else(|| AgentError {
            code: "not_found".into(),
            message: format!("unknown session `{session_id}`"),
        })?;
        self.store
            .save_session(&fleet_domain::store::SessionRecord {
                session_id: session_id.to_string(),
                workspace_id: record.summary.workspace_id.clone(),
                provider_id: record.summary.provider_id.clone(),
                state: record.summary.state,
                turn_state: record.summary.turn_state,
                created_at_ms: now_ms(),
                updated_at_ms: now_ms(),
            })
            .map_err(to_agent_error)
    }

    /// El worker confirma el cierre real, incluso si el cliente agotó su plazo.
    fn finish_stop(&self, session_id: &str, result: &Result<(), AgentError>) {
        let mut state = self.state.lock().expect("manager poisoned");
        let Some(record) = state.sessions.get_mut(session_id) else {
            return;
        };
        if !matches!(
            record.summary.state,
            SessionState::Stopping | SessionState::StopUnconfirmed
        ) {
            return;
        }
        let new_state = if result.is_ok() {
            SessionState::Stopped
        } else {
            SessionState::StopUnconfirmed
        };
        record.summary.state = new_state;
        let _ = self.publish_locked(
            &mut state,
            session_id,
            EventKind::SessionStateChanged { state: new_state },
        );
        if let Err(error) = result {
            let _ = self.publish_locked(
                &mut state,
                session_id,
                EventKind::AgentError {
                    message: format!("driver close could not be confirmed: {error}"),
                },
            );
        }
        drop(state);
        self.persist_session_state(session_id);
    }

    fn mark_stop_unconfirmed(&self, session_id: &str, reason: &str) {
        let mut state = self.state.lock().expect("manager poisoned");
        let Some(record) = state.sessions.get_mut(session_id) else {
            return;
        };
        if record.summary.state != SessionState::Stopping {
            return;
        }
        record.summary.state = SessionState::StopUnconfirmed;
        let _ = self.publish_locked(
            &mut state,
            session_id,
            EventKind::SessionStateChanged {
                state: SessionState::StopUnconfirmed,
            },
        );
        let _ = self.publish_locked(
            &mut state,
            session_id,
            EventKind::AgentNotice {
                message: reason.to_string(),
            },
        );
        drop(state);
        self.persist_session_state(session_id);
    }

    /// Cierre acotado: Stopped solo significa cierre confirmado.
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
            record.summary.state = SessionState::Stopping;
            if let Some(turn_id) = &active_turn {
                record.summary.turn_state = TurnState::Uncertain;
                let _ = self.publish_locked(
                    &mut state,
                    session_id,
                    EventKind::TurnUncertain {
                        turn_id: turn_id.clone(),
                        reason: "session stopped while turn was active".into(),
                    },
                );
            }
            let _ = self.publish_locked(
                &mut state,
                session_id,
                EventKind::SessionStateChanged {
                    state: SessionState::Stopping,
                },
            );
            if let Some(operation_id) = operation_id.clone() {
                let _ = self.store.update_operation(
                    &operation_id,
                    OperationStatus::Uncertain,
                    Some("session stopped before a terminal turn result".into()),
                );
            }
            (handle, active_turn, operation_id)
        };
        self.persist_session_state(session_id);
        let _ = (active_turn, operation_id);
        let (done_tx, done_rx) = mpsc::channel();
        let outcome = match handle.commands.send(DriverCommand::Close { done: done_tx }) {
            Ok(()) => match done_rx.recv_timeout(Duration::from_millis(500)) {
                Ok(result) => result,
                Err(_) => Err(AgentError {
                    code: "StopUnconfirmed".into(),
                    message: "driver has not confirmed close within 500ms; execution may continue"
                        .into(),
                }),
            },
            Err(_) => Err(AgentError {
                code: "StopUnconfirmed".into(),
                message: "driver command channel closed; cannot confirm termination".into(),
            }),
        };
        if let Err(error) = &outcome {
            self.mark_stop_unconfirmed(session_id, &error.message);
        }
        // El worker puede confirmar Stopped más tarde; no hacemos join bloqueante.
        drop(handle.join);
        outcome
    }
}

fn to_agent_error(error: StoreError) -> AgentError {
    AgentError {
        code: error.code,
        message: error.message,
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

    fn unique_db_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "gf-p2-core-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

    fn manager_at(config: FakeConfig, path: std::path::PathBuf) -> Arc<SessionManager> {
        let mut registry = ProviderRegistry::new();
        registry.register(Box::new(ConfiguredFactory(config)));
        let store = Arc::new(fleet_store_sqlite::SqliteStore::open(path).unwrap());
        Arc::new(SessionManager::new(registry, store).unwrap())
    }

    fn manager(config: FakeConfig) -> Arc<SessionManager> {
        manager_at(config, unique_db_path())
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
        let (sub_a, _, _) = manager.subscribe(&a.id, 0, SUBSCRIBER_LIMIT).unwrap();
        let (sub_b, _, _) = manager.subscribe(&b.id, 0, SUBSCRIBER_LIMIT).unwrap();
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
        let (sub_a, snapshot_a, _) = manager.subscribe(&session.id, 0, SUBSCRIBER_LIMIT).unwrap();
        let (sub_b, snapshot_b, _) = manager.subscribe(&session.id, 0, SUBSCRIBER_LIMIT).unwrap();
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
        let (_sub, _, _) = manager.subscribe(&session.id, 0, SUBSCRIBER_LIMIT).unwrap();
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
        let (sub, _, _) = manager.subscribe(&session.id, 0, SUBSCRIBER_LIMIT).unwrap();
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
        let (sub, _, _) = manager.subscribe(&session.id, 0, SUBSCRIBER_LIMIT).unwrap();
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
        let (sub, _, _) = manager.subscribe(&session.id, 0, SUBSCRIBER_LIMIT).unwrap();
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
        let (sub, _, _) = manager.subscribe(&session.id, 0, SUBSCRIBER_LIMIT).unwrap();
        manager
            .send_prompt(&session.id, "hi".into(), "op-1")
            .unwrap();
        let _ = collect_terminal(&sub);
        let started = std::time::Instant::now();
        let error = manager.stop_session(&session.id).unwrap_err();
        assert_eq!(error.code, "StopUnconfirmed");
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(
            manager.list_sessions()[0].state,
            SessionState::StopUnconfirmed
        );
    }

    #[test]
    fn confirmed_stop_is_stopped() {
        let manager = manager(FakeConfig::default());
        let ws = workspace(&manager);
        let ses = session(&manager, &ws.id);
        manager.stop_session(&ses.id).unwrap();
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

    #[test]
    fn timeline_and_operations_survive_manager_restart() {
        let path = unique_db_path();
        let manager = manager_at(FakeConfig::default(), path.clone());
        let ws = workspace(&manager);
        let ses = session(&manager, &ws.id);
        let (subscription, _, _) = manager.subscribe(&ses.id, 0, SUBSCRIBER_LIMIT).unwrap();
        let receipt = manager
            .send_prompt(&ses.id, "remember me".into(), "durable-key")
            .unwrap();
        let _ = collect_terminal(&subscription);
        drop(subscription);
        drop(manager);

        let reopened = manager_at(FakeConfig::default(), path);
        let restored = reopened
            .list_sessions()
            .into_iter()
            .find(|item| item.id == ses.id)
            .expect("session must be restored");
        assert_eq!(restored.state, SessionState::NeedsReconciliation);
        assert_eq!(restored.turn_state, TurnState::Uncertain);
        let events = reopened.read_timeline(&ses.id, 0, 256).unwrap();
        assert!(events.len() >= 4);
        assert!(events
            .iter()
            .any(|event| matches!(event.kind, EventKind::UserMessage { .. })));
        let reused = reopened
            .send_prompt(&ses.id, "remember me".into(), "durable-key")
            .unwrap();
        assert_eq!(reused, receipt);
    }

    #[test]
    fn approval_is_resolved_once_and_provider_sees_the_decision() {
        let manager = manager(FakeConfig {
            emit_approval: true,
            ..FakeConfig::default()
        });
        let ws = workspace(&manager);
        let ses = session(&manager, &ws.id);
        let (subscription, _, _) = manager.subscribe(&ses.id, 0, SUBSCRIBER_LIMIT).unwrap();
        manager.send_prompt(&ses.id, "run".into(), "op-1").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let approval = loop {
            match subscription.recv_event(Duration::from_millis(100)) {
                Ok(Some(event)) => {
                    if let EventKind::ApprovalRequested { approval_id, .. } = event.kind {
                        break approval_id;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                other => panic!("unexpected approval wait result: {other:?}"),
            }
            assert!(
                deadline.elapsed() < Duration::from_secs(5),
                "no approval request"
            );
        };
        let resolved = manager
            .answer_approval(&approval, "once".into(), "answer-key")
            .unwrap();
        assert_eq!(resolved.status_text(), "dispatching");
        let duplicate = manager
            .answer_approval(&approval, "reject".into(), "answer-key-2")
            .unwrap_err();
        assert_eq!(duplicate.code, "Conflict");
        let saw_confirmation = loop {
            match subscription.recv_event(Duration::from_millis(200)) {
                Ok(Some(Event {
                    kind: EventKind::ApprovalResolved { decision, .. },
                    ..
                })) => {
                    break decision == "once";
                }
                Err(RecvTimeoutError::Timeout) => {}
                other => panic!("unexpected confirmation: {other:?}"),
            }
        };
        assert!(saw_confirmation);
        assert_eq!(
            manager.get_approval(&approval).unwrap().status_text(),
            "accepted"
        );
    }

    #[test]
    fn rejected_approval_does_not_run_the_command() {
        let manager = manager(FakeConfig {
            emit_approval: true,
            ..FakeConfig::default()
        });
        let ws = workspace(&manager);
        let ses = session(&manager, &ws.id);
        let (subscription, _, _) = manager.subscribe(&ses.id, 0, SUBSCRIBER_LIMIT).unwrap();
        manager.send_prompt(&ses.id, "run".into(), "op-1").unwrap();
        let approval = loop {
            match subscription.recv_event(Duration::from_millis(100)) {
                Ok(Some(Event {
                    kind: EventKind::ApprovalRequested { approval_id, .. },
                    ..
                })) => {
                    break approval_id;
                }
                Ok(Some(_)) | Ok(None) => {}
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => panic!("subscription closed"),
            }
        };
        let resolved = manager
            .answer_approval(&approval, "reject".into(), "answer-key")
            .unwrap();
        assert_eq!(resolved.status_text(), "dispatching");
        assert!(!manager.list_approvals(Some(&ses.id)).is_empty());
    }
}

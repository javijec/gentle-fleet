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

pub mod runner;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fleet_agent::{redact_provider_error, AgentError, ProviderRegistry};
use fleet_domain::store::{
    ApprovalClaim, ApprovalRequest, ApprovalStatus, OperationKind, OperationRecord,
    OperationReservation, OperationStatus, RunnerRecord, SessionStore, StoreError, TurnRecord,
};
use fleet_domain::{
    AgentEvent, ControlState, DomainEvent, Event, EventKind, HostId, ProviderDescriptor,
    ProviderId, RunnerEvent, RunnerMode, RunnerStatus, SessionId, SessionState, TurnState,
    TurnStatus, WorkspaceId,
};
use runner::{RunnerCommand, RunnerHandle, RunnerLaunchContext, RunnerLauncher};

/// Valida campos tipados ANTES de reservar una aprobación irreversible.
fn check_approval_fields(
    option: Option<&fleet_domain::ApprovalOptionSpec>,
    args: &[fleet_domain::ApprovalArgument],
) -> Result<(), AgentError> {
    let Some(option) = option else {
        if args.is_empty() {
            return Ok(()); // aprobaciones anteriores a P4, solo decisión simple
        }
        return Err(AgentError {
            code: "InvalidArgument".into(),
            message: "approval has no declared argument schema".into(),
        });
    };
    let mut seen = std::collections::HashSet::new();
    for input in args {
        if !seen.insert(input.name.as_str()) {
            return Err(AgentError {
                code: "InvalidArgument".into(),
                message: "duplicate approval argument".into(),
            });
        }
        let field = option
            .args
            .iter()
            .find(|spec| spec.name == input.name)
            .ok_or_else(|| AgentError {
                code: "InvalidArgument".into(),
                message: "approval argument was not offered".into(),
            })?;
        if input.values.is_empty()
            || input.values.iter().any(String::is_empty)
            || (field.kind == "text" && input.values.len() != 1)
            || !matches!(field.kind.as_str(), "text" | "text_list")
        {
            return Err(AgentError {
                code: "InvalidArgument".into(),
                message: "invalid approval argument values".into(),
            });
        }
    }
    for required in option.args.iter().filter(|field| field.required) {
        if !seen.contains(required.name.as_str()) {
            return Err(AgentError {
                code: "InvalidArgument".into(),
                message: "required approval argument is missing".into(),
            });
        }
    }
    Ok(())
}

/// Capacidad por defecto de la cola de cada suscriptor.
pub const SUBSCRIBER_LIMIT: usize = 256;
/// Página máxima devuelta por una lectura de timeline.
pub const TIMELINE_PAGE: usize = 256;

/// Huella de aprobación: conserva el formato histórico si no hay argumentos.
/// Con argumentos, serializa nombres y valores con longitudes para impedir
/// colisiones por separadores; el orden de argumentos no cambia la identidad.
fn approval_answer_fingerprint(
    session_id: &str,
    approval_id: &str,
    decision: &str,
    args: &[fleet_domain::ApprovalArgument],
) -> String {
    let mut fingerprint = format!("{session_id}\0{approval_id}\0{decision}");
    if args.is_empty() {
        return fingerprint;
    }
    fingerprint.push_str("\0typed-v1");
    let mut ordered = args.to_vec();
    ordered.sort_by(|a, b| a.name.cmp(&b.name));
    for arg in &ordered {
        fingerprint.push_str(&format!("\0{}:{}", arg.name.len(), arg.name));
        fingerprint.push_str(&format!("\0{}", arg.values.len()));
        for value in &arg.values {
            fingerprint.push_str(&format!("\0{}:{}", value.len(), value));
        }
    }
    fingerprint
}

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

struct SessionRecord {
    summary: SessionSummary,
    next_seq: u64,
    runner: Option<Arc<dyn RunnerHandle>>,
    ingress: Option<JoinHandle<()>>,
    active_turn: Option<String>,
    current_operation: Option<String>,
    /// Controles deseados por el usuario; se aplican al SIGUIENTE turno.
    /// En memoria: no se persisten porque no son una autoridad durable.
    desired_controls: Vec<fleet_domain::RequestedControl>,
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
    /// Once a durable commit fails, reject further new effects until restart.
    storage_fault: Option<String>,
}

pub struct SessionManager {
    registry: ProviderRegistry,
    store: Arc<dyn SessionStore>,
    launcher: Arc<dyn RunnerLauncher>,
    host_id: HostId,
    bus: EventBus,
    state: Mutex<ManagerState>,
}

impl SessionManager {
    fn ensure_writable(state: &ManagerState) -> Result<(), AgentError> {
        if let Some(reason) = &state.storage_fault {
            return Err(AgentError {
                code: "StorageUnavailable".into(),
                message: format!(
                    "Fleet storage failed; restart and reconcile before new operations: {reason}"
                ),
            });
        }
        Ok(())
    }

    /// Valida un destino bajo el bloqueo de ManagerState. La comprobación se
    /// repite después del RPC nativo, inmediatamente antes de publicar historial.
    fn ensure_history_import_target(
        state: &ManagerState,
        session_id: &str,
        provider_id: &str,
    ) -> Result<(), AgentError> {
        Self::ensure_writable(state)?;
        let record = state.sessions.get(session_id).ok_or_else(|| AgentError {
            code: "not_found".into(),
            message: "target Fleet session does not exist".into(),
        })?;
        if record.summary.provider_id != provider_id {
            return Err(AgentError {
                code: "ProviderMismatch".into(),
                message: "history provider and target session provider differ".into(),
            });
        }
        if record.summary.state != SessionState::Running
            || record.active_turn.is_some()
            || !record.summary.turn_state.accepts_prompt()
        {
            return Err(AgentError {
                code: "SessionBusy".into(),
                message: "cannot import history into an inactive, busy or uncertain session".into(),
            });
        }
        Ok(())
    }

    /// Una reserva existente pertenece a otra ejecución, incluso cuando
    /// el primer GET previo a la transacción no alcanzó a observarla.
    fn replay_history_import(operation: OperationRecord) -> Result<usize, AgentError> {
        match operation.status {
            OperationStatus::Completed => operation
                .message
                .as_deref()
                .and_then(|message| message.parse::<usize>().ok())
                .ok_or_else(|| AgentError {
                    code: "NeedsReconciliation".into(),
                    message: "completed history import has no valid persisted result".into(),
                }),
            OperationStatus::Reserved | OperationStatus::Accepted => Err(AgentError {
                code: "OperationPending".into(),
                message: "history import is already reserved; no duplicate execution".into(),
            }),
            OperationStatus::Uncertain => Err(AgentError {
                code: "NeedsReconciliation".into(),
                message: "previous history import outcome is uncertain".into(),
            }),
            OperationStatus::Failed => Err(AgentError {
                code: "OperationFailed".into(),
                message: "previous import failed; use a new operation key after investigation"
                    .into(),
            }),
        }
    }

    pub fn new(
        registry: ProviderRegistry,
        store: Arc<dyn SessionStore>,
        launcher: Arc<dyn RunnerLauncher>,
        host_id: HostId,
    ) -> Result<Self, AgentError> {
        let manager = Self {
            registry,
            store,
            launcher,
            host_id,
            bus: EventBus::default(),
            state: Mutex::new(ManagerState {
                workspaces: Vec::new(),
                sessions: HashMap::new(),
                approvals: HashMap::new(),
                next_ids: 0,
                storage_fault: None,
            }),
        };
        manager.load_from_store()?;
        Ok(manager)
    }

    fn load_from_store(&self) -> Result<(), AgentError> {
        // The previous daemon cannot prove a provider effect just by having
        // committed its intention. Never silently resume in-flight work.
        self.store.recover_after_restart().map_err(to_agent_error)?;
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
            let desired_controls = self
                .store
                .list_session_controls(&stored.session_id)
                .map_err(to_agent_error)?;
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
                    runner: None,
                    ingress: None,
                    active_turn: None,
                    current_operation: None,
                    desired_controls,
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
        Self::ensure_writable(&state)?;
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
            Self::ensure_writable(&state)?;
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
        let session_id = self.next_id("ses");
        let handle = self.launcher.launch(RunnerLaunchContext {
            session_id: session_id.clone(),
            provider_id: provider_id.to_string(),
            workspace: workspace.path.clone(),
            mode: RunnerMode::Auto,
            existing: None,
            ask_shell: false,
            fail_answer: false,
        })?;
        let identity = handle.identity().clone();
        let runner_handle: Arc<dyn RunnerHandle> = Arc::from(handle);

        let summary = {
            let mut state = self.state.lock().expect("manager poisoned");
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
            self.store
                .save_runner(&RunnerRecord {
                    identity: identity.clone(),
                    status: RunnerStatus::Active,
                    endpoint: format!("runner:{}", identity.runner_id),
                    last_ack_seq: 0,
                    created_at_ms: now_ms(),
                    updated_at_ms: now_ms(),
                })
                .map_err(to_agent_error)?;
            let ingress = self.spawn_ingress(session_id.clone(), Arc::clone(&runner_handle));
            state.sessions.insert(
                session_id.clone(),
                SessionRecord {
                    summary: summary.clone(),
                    next_seq,
                    runner: Some(runner_handle),
                    ingress: Some(ingress),
                    active_turn: None,
                    current_operation: None,
                    desired_controls: Vec::new(),
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

    fn spawn_ingress(
        self: &Arc<Self>,
        session_id: SessionId,
        handle: Arc<dyn RunnerHandle>,
    ) -> JoinHandle<()> {
        let manager = Arc::clone(self);
        let runner_id = handle.identity().runner_id.clone();
        thread::spawn(move || {
            let mut last_probe = std::time::Instant::now();
            let mut seen_notices = std::collections::HashSet::new();
            loop {
                if last_probe.elapsed() >= Duration::from_secs(2) {
                    last_probe = std::time::Instant::now();
                    for notice in handle.notices() {
                        if seen_notices.insert(notice.clone()) {
                            let _ = manager.publish_local(
                                &session_id,
                                EventKind::AgentNotice { message: notice },
                            );
                        }
                    }
                    match handle.provider_alive() {
                        Ok(true) => {}
                        Ok(false) => {
                            manager.mark_runner_uncertain(
                                &session_id,
                                &runner_id,
                                "native provider is no longer alive",
                            );
                            break;
                        }
                        Err(error) => {
                            manager.mark_runner_uncertain(
                                &session_id,
                                &runner_id,
                                &error.to_string(),
                            );
                            break;
                        }
                    }
                }
                match handle.recv(Duration::from_millis(50)) {
                    Ok(Some(RunnerEvent {
                        runner_id: event_runner_id,
                        runner_seq,
                        event,
                    })) => {
                        if event_runner_id != runner_id {
                            continue;
                        }
                        if let Err(error) =
                            manager.ingest_runner_event(&session_id, &runner_id, runner_seq, event)
                        {
                            eprintln!("runner event could not be ingested: {error}");
                            if error.code == "ack_failed" {
                                // The daemon's ACK cursor is now out of sync with the
                                // runner. Continuing would persist events that can
                                // never be confirmed, so fail closed and stop reading.
                                manager.mark_runner_uncertain(
                                    &session_id,
                                    &runner_id,
                                    &format!("runner ACK failed: {error}"),
                                );
                                break;
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        eprintln!("runner ingress stopped for {session_id}: {error}");
                        manager.mark_runner_uncertain(&session_id, &runner_id, &error.to_string());
                        break;
                    }
                }
            }
        })
    }

    fn mark_runner_uncertain(&self, session_id: &str, runner_id: &str, reason: &str) {
        let mut state = self.state.lock().expect("manager poisoned");
        let Some(record) = state.sessions.get_mut(session_id) else {
            return;
        };
        if !matches!(
            record.summary.state,
            SessionState::Running | SessionState::Recovering
        ) || record
            .runner
            .as_ref()
            .is_none_or(|handle| handle.identity().runner_id != runner_id)
        {
            // A deliberate Stop must not be reclassified as a crash.
            return;
        }
        let active = record.active_turn.clone();
        record.summary.state = SessionState::NeedsReconciliation;
        if active.is_some() {
            record.summary.turn_state = TurnState::Uncertain;
        }
        if let Some(turn_id) = active {
            let _ = self.publish_locked(
                &mut state,
                session_id,
                EventKind::TurnUncertain {
                    turn_id,
                    reason: "runner connection or provider health lost".into(),
                },
            );
        }
        let _ = self.publish_locked(
            &mut state,
            session_id,
            EventKind::SessionStateChanged {
                state: SessionState::NeedsReconciliation,
            },
        );
        if let Err(error) = self
            .store
            .update_runner_status(runner_id, RunnerStatus::Uncertain)
        {
            // Fail closed: if the DB still says `active`, a restart would
            // reattach the runner and contradict the in-memory uncertainty.
            eprintln!("cannot persist runner uncertainty: {error:?}; original: {reason}");
            self.record_storage_fault(&error.message);
        }
    }

    /// Ingesta deduplicada por `(runner_id, runner_seq)`: el evento se publica
    /// una sola vez y el ACK se envía sólo después de persistirlo.
    fn ingest_runner_event(
        &self,
        session_id: &str,
        runner_id: &str,
        runner_seq: u64,
        event: AgentEvent,
    ) -> Result<(), AgentError> {
        let handle = {
            let state = self.state.lock().expect("manager poisoned");
            state
                .sessions
                .get(session_id)
                .and_then(|record| record.runner.clone())
        };
        let Some(handle) = handle else {
            return Ok(());
        };
        if self
            .store
            .runner_event_exists(runner_id, runner_seq)
            .map_err(to_agent_error)?
        {
            handle.ack(runner_seq).map_err(|error| AgentError {
                code: "ack_failed".into(),
                message: error.to_string(),
            })?;
            return Ok(());
        }
        let event_id =
            self.publish_agent_from_runner(session_id, event, Some((runner_id, runner_seq)))?;
        if event_id.is_none() {
            // No public event (stale step / redundant native TurnStarted), but
            // its runner sequence must still be durably recorded before ACK.
            self.store
                .record_runner_event(runner_id, runner_seq, "")
                .map_err(to_agent_error)?;
        }
        handle.ack(runner_seq).map_err(|error| AgentError {
            code: "ack_failed".into(),
            message: error.to_string(),
        })?;
        Ok(())
    }

    /// Reintenta adjuntar runners persistentes tras reiniciar el daemon.
    /// Sin evidencia de identidad no se crea ningún runner sustituto.
    pub fn recover_runners(self: &Arc<Self>) -> Result<Vec<String>, AgentError> {
        let mut recovered = Vec::new();
        let records = self.store.list_runners().map_err(to_agent_error)?;
        for record in records {
            if matches!(
                record.status,
                RunnerStatus::Stopped
                    | RunnerStatus::StopUnconfirmed
                    | RunnerStatus::Lost
                    | RunnerStatus::Stopping
                    | RunnerStatus::Uncertain
            ) {
                continue;
            }
            let Some(session_record) = self
                .store
                .get_session(&record.identity.session_id)
                .map_err(to_agent_error)?
            else {
                continue;
            };
            // Logical session state governs reattachment. A deliberately
            // stopped or failed session must never be revived by a live runner.
            if matches!(
                session_record.state,
                SessionState::Stopped
                    | SessionState::Stopping
                    | SessionState::StopUnconfirmed
                    | SessionState::Failed
            ) {
                let _ = self
                    .store
                    .update_runner_status(&record.identity.runner_id, RunnerStatus::Stopped);
                continue;
            }
            // Never start a second ingress for the same runner. Recovery may
            // be retried after a partial failure; two ingress threads would
            // consume the same event stream and fight over state transitions.
            {
                let state = self.state.lock().expect("manager poisoned");
                if state
                    .sessions
                    .get(&record.identity.session_id)
                    .is_none_or(|session| session.runner.is_some() || session.ingress.is_some())
                {
                    continue;
                }
            }
            let context = RunnerLaunchContext {
                session_id: record.identity.session_id.clone(),
                provider_id: record.identity.provider_id.clone(),
                workspace: self
                    .state
                    .lock()
                    .expect("manager poisoned")
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.id == session_record.workspace_id)
                    .map(|workspace| workspace.path.clone())
                    .unwrap_or_default(),
                mode: RunnerMode::Attached,
                existing: Some(record.clone()),
                ask_shell: false,
                fail_answer: false,
            };
            let handle = match self.launcher.launch(context) {
                Ok(handle) => handle,
                Err(_) => {
                    let _ = self
                        .store
                        .update_runner_status(&record.identity.runner_id, RunnerStatus::Uncertain);
                    let _ = self
                        .store
                        .mark_session_approvals_uncertain(&record.identity.session_id);
                    continue;
                }
            };
            let attached = handle.identity();
            if attached.runner_id != record.identity.runner_id
                || attached.boot_id != record.identity.boot_id
                || attached.generation != record.identity.generation
                || !attached.host_id.eq(&self.host_id)
            {
                let _ = self
                    .store
                    .update_runner_status(&record.identity.runner_id, RunnerStatus::Uncertain);
                continue;
            }
            // A valid socket/token alone does not prove the provider survived.
            if !handle.provider_alive().unwrap_or(false) {
                self.store
                    .update_runner_status(&record.identity.runner_id, RunnerStatus::Uncertain)
                    .map_err(to_agent_error)?;
                self.store
                    .mark_session_approvals_uncertain(&record.identity.session_id)
                    .map_err(to_agent_error)?;
                continue;
            }
            let runner_id = record.identity.runner_id.clone();
            let session_id = record.identity.session_id.clone();
            let shared: Arc<dyn RunnerHandle> = Arc::from(handle);
            // Query before handing the reader to the ingress thread.
            let active = match self.store.active_turn_for_session(&session_id) {
                Ok(active) => active,
                Err(error) => {
                    // A single unreadable session must not block recovery of
                    // every other runner handled in this startup pass.
                    eprintln!("cannot read active turn for {session_id}: {error:?}");
                    let _ = self
                        .store
                        .update_runner_status(&runner_id, RunnerStatus::Uncertain);
                    continue;
                }
            };
            // Publish the recovered turn ownership BEFORE starting ingress:
            // replayed terminal events must see the correct active_turn.
            {
                let mut state = self.state.lock().expect("manager poisoned");
                if let Some(session) = state.sessions.get_mut(&session_id) {
                    session.runner = Some(Arc::clone(&shared));
                    if let Some(turn) = &active {
                        session.summary.state = SessionState::Recovering;
                        session.active_turn = Some(turn.turn_id.clone());
                        session.current_operation = Some(turn.operation_id.clone());
                        session.summary.turn_state = TurnState::Uncertain;
                    } else {
                        // No in-flight work: authenticated live runner is ready
                        // to receive the next turn immediately after reattach.
                        session.summary.state = SessionState::Running;
                    }
                }
                if active.is_none() {
                    self.publish_locked(
                        &mut state,
                        &session_id,
                        EventKind::SessionStateChanged {
                            state: SessionState::Running,
                        },
                    )?;
                }
            }
            let ingress = self.spawn_ingress(session_id.clone(), Arc::clone(&shared));
            {
                let mut state = self.state.lock().expect("manager poisoned");
                if let Some(session) = state.sessions.get_mut(&session_id) {
                    session.ingress = Some(ingress);
                }
            }
            self.store
                .update_runner_status(
                    &runner_id,
                    if active.is_some() {
                        RunnerStatus::Recovering
                    } else {
                        RunnerStatus::Active
                    },
                )
                .map_err(to_agent_error)?;
            recovered.push(session_id);
        }
        Ok(recovered)
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
        self.publish_locked_with_runner(state, session_id, kind, approval, None)
    }

    fn publish_locked_with_runner(
        &self,
        state: &mut ManagerState,
        session_id: &str,
        kind: EventKind,
        approval: Option<&ApprovalRequest>,
        runner_source: Option<(&str, u64)>,
    ) -> Result<(), AgentError> {
        Self::ensure_writable(state)?;
        let record = state
            .sessions
            .get(session_id)
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
        let committed = match runner_source {
            Some((runner_id, runner_seq)) => {
                self.store
                    .commit_runner_event(runner_id, runner_seq, &event, &projection, approval)
            }
            None => self
                .store
                .commit_event(&event, &projection, approval)
                .map(|_| true),
        };
        let created = match committed {
            Ok(created) => created,
            Err(error) => {
                // Do not publish or advance the sequence on failed persistence.
                state.storage_fault = Some(error.message.clone());
                return Err(to_agent_error(error));
            }
        };
        if created {
            state
                .sessions
                .get_mut(session_id)
                .expect("session checked")
                .next_seq += 1;
            self.bus.publish(&event);
        }
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
                message: redact_provider_error(&error.to_string()),
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

    #[cfg(test)]
    fn publish_agent(
        &self,
        session_id: &str,
        event: AgentEvent,
    ) -> Result<Option<String>, AgentError> {
        self.publish_agent_from_runner(session_id, event, None)
    }

    fn publish_agent_from_runner(
        &self,
        session_id: &str,
        event: AgentEvent,
        runner_source: Option<(&str, u64)>,
    ) -> Result<Option<String>, AgentError> {
        let next_approval_id = self.next_id("appr");
        let mut state = self.state.lock().expect("manager poisoned");
        let (kind, _operation_update, new_approval, approval_update) = {
            let Some(record) = state.sessions.get_mut(session_id) else {
                return Ok(None);
            };
            let active = record.active_turn.clone();
            let mut operation_update = None;
            let mut new_approval = None;
            let mut approval_update = None;
            let scoped_turn = |turn_id: &str| -> bool { active.as_deref() == Some(turn_id) };
            let kind = match event.kind {
                DomainEvent::TurnStarted { .. } => return Ok(None),
                DomainEvent::AssistantDelta { turn_id, text } => {
                    if !scoped_turn(&turn_id) {
                        return Ok(None);
                    }
                    EventKind::AssistantDelta { turn_id, text }
                }
                DomainEvent::AssistantMessage { turn_id, text } => {
                    if !scoped_turn(&turn_id) {
                        return Ok(None);
                    }
                    EventKind::AssistantMessage { turn_id, text }
                }
                DomainEvent::ToolStarted {
                    turn_id,
                    name,
                    input,
                } => {
                    if !scoped_turn(&turn_id) {
                        return Ok(None);
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
                        return Ok(None);
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
                        return Ok(None);
                    }
                    EventKind::ToolCompleted {
                        turn_id,
                        name,
                        success,
                    }
                }
                DomainEvent::TurnCompleted { turn_id, status } => {
                    if !scoped_turn(&turn_id) {
                        return Ok(None);
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
                        return Ok(None);
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
                DomainEvent::TurnUncertain { turn_id, reason } => {
                    if !scoped_turn(&turn_id) {
                        return Ok(None);
                    }
                    operation_update = record
                        .current_operation
                        .clone()
                        .map(|id| (id, OperationStatus::Uncertain, Some(reason.clone())));
                    record.summary.turn_state = TurnState::Uncertain;
                    EventKind::TurnUncertain {
                        turn_id,
                        reason: redact_provider_error(&reason),
                    }
                }
                DomainEvent::ApprovalRequested {
                    native_request_id,
                    kind,
                    message,
                    resources,
                    allowed_decisions,
                    options,
                } => {
                    let Some(turn_id) = active.clone() else {
                        return Ok(None);
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
                        options,
                    }
                }
                DomainEvent::ApprovalResolved {
                    native_request_id,
                    decision,
                } => {
                    let Some(approval) = state
                        .approvals
                        .values()
                        .find(|a| {
                            a.session_id == session_id && a.native_request_id == native_request_id
                        })
                        .cloned()
                    else {
                        return Ok(None);
                    };
                    if approval.status != ApprovalStatus::Dispatching
                        || approval.decision.as_deref() != Some(decision.as_str())
                        || approval.session_id != session_id
                    {
                        return Ok(None);
                    }
                    let status =
                        if matches!(decision.as_str(), "reject" | "decline" | "cancel" | "abort") {
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
                DomainEvent::AgentNotice { message } => EventKind::AgentNotice {
                    message: redact_provider_error(&message),
                },
                DomainEvent::AgentError { message } => EventKind::AgentError {
                    message: redact_provider_error(&message),
                },
            };
            (kind, operation_update, new_approval, approval_update)
        };

        // Keep the manager lock across the native event transition; no other
        // client may change the approval between checking and committing it.
        self.publish_locked_with_runner(
            &mut state,
            session_id,
            kind,
            new_approval.as_ref(),
            runner_source,
        )?;
        if let Some(approval) = new_approval {
            state
                .approvals
                .insert(approval.approval_id.clone(), approval);
        }
        if let Some((approval, status)) = approval_update {
            let mut resolved = approval;
            resolved.status = status;
            resolved.resolved_at_ms = Some(now_ms());
            state
                .approvals
                .insert(resolved.approval_id.clone(), resolved);
        }
        // Reattachment is complete when the previously uncertain turn emits
        // a durable terminal result. Never leave the live session in Recovering.
        let finish_recovery = state.sessions.get(session_id).is_some_and(|record| {
            record.summary.state == SessionState::Recovering
                && record.active_turn.is_none()
                && matches!(
                    record.summary.turn_state,
                    TurnState::Completed | TurnState::Failed | TurnState::Interrupted
                )
        });
        if finish_recovery {
            if let Some(record) = state.sessions.get_mut(session_id) {
                record.summary.state = SessionState::Running;
            }
            self.publish_locked(
                &mut state,
                session_id,
                EventKind::SessionStateChanged {
                    state: SessionState::Running,
                },
            )?;
            if let Some(runner_id) = runner_source.map(|(id, _)| id) {
                self.store
                    .update_runner_status(runner_id, RunnerStatus::Active)
                    .map_err(to_agent_error)?;
            }
        }
        let event_id = state
            .sessions
            .get(session_id)
            .map(|record| format!("{}-e{}", session_id, record.next_seq.saturating_sub(1)));
        Ok(event_id)
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

    /// Recupera el contrato de opciones desde la timeline durable. P3 ya
    /// persiste los eventos antes de publicar y no requiere una nueva tabla.
    fn validate_approval_payload(
        &self,
        approval: &ApprovalRequest,
        decision: &str,
        args: &[fleet_domain::ApprovalArgument],
    ) -> Result<(), AgentError> {
        let mut since = 0u64;
        loop {
            let events = self
                .store
                .read_events(&approval.session_id, since, TIMELINE_PAGE)
                .map_err(to_agent_error)?;
            if events.is_empty() {
                break;
            }
            for event in &events {
                if let EventKind::ApprovalRequested {
                    approval_id,
                    options,
                    ..
                } = &event.kind
                {
                    if approval_id == &approval.approval_id {
                        let option = options.iter().find(|spec| spec.option_id == decision);
                        return check_approval_fields(option, args);
                    }
                }
            }
            let next = events.last().map(|event| event.seq).unwrap_or(since);
            if next <= since {
                return Err(AgentError {
                    code: "TimelineCorrupt".into(),
                    message: "approval event search did not advance".into(),
                });
            }
            if events.len() < TIMELINE_PAGE {
                break;
            }
            since = next;
        }
        check_approval_fields(None, args)
    }

    /// Atomically claim the approval and its operation. The HTTP result is
    /// NOT known here: only a native driver event can confirm final delivery.
    pub fn answer_approval(
        &self,
        approval_id: &str,
        decision: String,
        operation_key: &str,
    ) -> Result<ApprovalRequest, AgentError> {
        self.answer_approval_with_args(approval_id, decision, Vec::new(), operation_key)
    }

    /// P4.1-D: igual que `answer_approval`, pero transporta argumentos
    /// tipados validados por el driver contra el esquema de la opción.
    pub fn answer_approval_with_args(
        &self,
        approval_id: &str,
        decision: String,
        arguments: Vec<fleet_domain::ApprovalArgument>,
        operation_key: &str,
    ) -> Result<ApprovalRequest, AgentError> {
        let mut state = self.state.lock().expect("manager poisoned");
        let approval = state
            .approvals
            .get(approval_id)
            .cloned()
            .ok_or_else(|| AgentError {
                code: "not_found".into(),
                message: format!("unknown approval `{approval_id}`"),
            })?;
        if !approval.allowed_decisions.contains(&decision) {
            return Err(AgentError {
                code: "InvalidDecision".into(),
                message: format!("`{decision}` is not allowed for `{approval_id}`"),
            });
        }
        self.validate_approval_payload(&approval, &decision, &arguments)?;
        let fingerprint =
            approval_answer_fingerprint(&approval.session_id, approval_id, &decision, &arguments);
        // Idempotent replay must work even after a storage fault: it does not
        // dispatch again, so it is not a new provider effect.
        if let Some(existing) = self
            .store
            .get_operation(operation_key)
            .map_err(to_agent_error)?
        {
            if existing.kind != OperationKind::AnswerApproval
                || existing.session_id != approval.session_id
                || existing.fingerprint != fingerprint
            {
                return Err(AgentError {
                    code: "Conflict".into(),
                    message: "operation key was used with different arguments".into(),
                });
            }
            let stored = self
                .store
                .get_approval(approval_id)
                .map_err(to_agent_error)?
                .unwrap_or(approval);
            return Ok(stored);
        }
        Self::ensure_writable(&state)?;
        let operation = OperationRecord {
            operation_id: Self::next_id_locked(&mut state, "op"),
            operation_key: operation_key.to_string(),
            kind: OperationKind::AnswerApproval,
            session_id: approval.session_id.clone(),
            target_id: Some(approval_id.to_string()),
            fingerprint,
            status: OperationStatus::Reserved,
            message: None,
            created_at_ms: now_ms(),
            updated_at_ms: now_ms(),
        };
        let claimed = self
            .store
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
            ApprovalClaim::Uncertain(_) => {
                return Err(AgentError {
                    code: "NeedsReconciliation".into(),
                    message: "approval delivery is uncertain; no automatic retry".into(),
                })
            }
            ApprovalClaim::Conflict => {
                return Err(AgentError {
                    code: "Conflict".into(),
                    message: "approval already claimed or operation key has different parameters"
                        .into(),
                })
            }
            ApprovalClaim::Created(updated) => updated,
        };
        state
            .approvals
            .insert(approval_id.to_string(), updated.clone());
        let runner = state
            .sessions
            .get(&approval.session_id)
            .and_then(|record| record.runner.clone());
        drop(state);

        let result = runner
            .ok_or_else(|| AgentError {
                code: "runner_unavailable".into(),
                message: "no live runner to deliver approval".into(),
            })
            .and_then(|handle| {
                handle
                    .submit(RunnerCommand::Answer {
                        approval_id: approval_id.to_string(),
                        native_request_id: approval.native_request_id.clone(),
                        decision: decision.clone(),
                        operation_key: operation_key.to_string(),
                        arguments,
                    })
                    .map_err(|error| AgentError {
                        code: "approval_delivery_failed".into(),
                        message: error.to_string(),
                    })
            });
        if let Err(error) = result {
            self.mark_approval_uncertain(&approval.session_id, approval_id, &error);
            return Err(AgentError {
                code: "NeedsReconciliation".into(),
                message: error.message,
            });
        }
        Ok(updated) // Dispatching: NOT Accepted/Rejected until native confirmation
    }

    fn record_storage_fault(&self, reason: &str) {
        let mut state = self.state.lock().expect("manager poisoned");
        if state.storage_fault.is_none() {
            state.storage_fault = Some(reason.to_string());
        }
    }

    pub fn mark_approval_uncertain(&self, session_id: &str, approval_id: &str, error: &AgentError) {
        match self
            .store
            .mark_approval_uncertain(approval_id, &error.message)
        {
            Ok(()) => {}
            Err(store_error) if store_error.code == "approval_already_resolved" => {
                // A native confirmation already won; keep the durable final state.
                return;
            }
            Err(store_error) => {
                // Fail closed: without a durable uncertainty record no new
                // provider effect may be started until restart and reconciliation.
                eprintln!("could not persist uncertain approval: {store_error:?}");
                self.record_storage_fault(&store_error.message);
                return;
            }
        }
        if let Some(approval) = self
            .state
            .lock()
            .expect("manager poisoned")
            .approvals
            .get_mut(approval_id)
        {
            approval.status = ApprovalStatus::Uncertain;
        }
        let _ = self.publish_local(
            session_id,
            EventKind::AgentError {
                message: redact_provider_error(&format!(
                    "approval decision delivery uncertain: {error}"
                )),
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
        if let Some(existing) = self
            .store
            .get_operation(operation_key)
            .map_err(to_agent_error)?
        {
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
                    message: existing
                        .message
                        .unwrap_or_else(|| "previous dispatch failed".into()),
                });
            }
            return Ok(PromptReceipt {
                operation_id: existing.operation_id,
                session_id: existing.session_id,
                turn_id: existing.target_id.unwrap_or_default(),
            });
        }
        Self::ensure_writable(&state)?;
        {
            let record = state.sessions.get(session_id).ok_or_else(|| AgentError {
                code: "not_found".into(),
                message: format!("unknown session `{session_id}`"),
            })?;
            if record.runner.is_none() || record.summary.state != SessionState::Running {
                return Err(AgentError {
                    code: "session_stopped".into(),
                    message: format!("session `{session_id}` does not have a live runner"),
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
            fingerprint: fingerprint.clone(),
            status: OperationStatus::Reserved,
            message: None,
            created_at_ms: now_ms(),
            updated_at_ms: now_ms(),
        };
        match self
            .store
            .reserve_operation(&operation)
            .map_err(to_agent_error)?
        {
            OperationReservation::Created(_) => {}
            OperationReservation::Conflict => {
                return Err(AgentError {
                    code: "Conflict".into(),
                    message: "operation key belongs to a different request".into(),
                })
            }
            OperationReservation::Existing(existing) => {
                if existing.kind != OperationKind::SendPrompt
                    || existing.session_id != session_id
                    || existing.fingerprint != fingerprint
                {
                    return Err(AgentError {
                        code: "Conflict".into(),
                        message: "operation key belongs to a different request".into(),
                    });
                }
                if matches!(
                    existing.status,
                    OperationStatus::Reserved | OperationStatus::Uncertain
                ) {
                    return Err(AgentError {
                        code: "NeedsReconciliation".into(),
                        message: "previous prompt outcome is uncertain; no automatic retry".into(),
                    });
                }
                if existing.status == OperationStatus::Failed {
                    return Err(AgentError {
                        code: "PreviousOperationFailed".into(),
                        message: existing
                            .message
                            .unwrap_or_else(|| "previous dispatch failed".into()),
                    });
                }
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
            self.store
                .update_operation(
                    &operation_id,
                    OperationStatus::Failed,
                    Some("failed to persist turn before dispatch".into()),
                )
                .map_err(to_agent_error)?;
            return Err(to_agent_error(error));
        }
        let (runner, controls) = {
            let record = state.sessions.get_mut(session_id).expect("session checked");
            record.active_turn = Some(turn_id.clone());
            record.current_operation = Some(operation_id.clone());
            record.summary.turn_state = TurnState::Running;
            let controls = record.desired_controls.clone();
            (record.runner.clone().expect("runner checked"), controls)
        };
        // The prompt cannot reach the provider before both startup events are
        // committed. A failure makes the operation uncertain, never replayable.
        for kind in [
            EventKind::TurnStarted {
                turn_id: turn_id.clone(),
            },
            EventKind::UserMessage {
                turn_id: turn_id.clone(),
                text: text.clone(),
            },
        ] {
            if let Err(error) = self.publish_locked(&mut state, session_id, kind) {
                self.store
                    .update_operation(
                        &operation_id,
                        OperationStatus::Uncertain,
                        Some("prompt events were not fully persisted".into()),
                    )
                    .map_err(to_agent_error)?;
                if let Some(record) = state.sessions.get_mut(session_id) {
                    record.summary.turn_state = TurnState::Uncertain;
                    record.active_turn = None;
                    record.current_operation = None;
                }
                return Err(error);
            }
        }
        self.store
            .update_operation(&operation_id, OperationStatus::Accepted, None)
            .map_err(to_agent_error)?;
        drop(state);
        if let Err(error) = runner.submit(RunnerCommand::Prompt {
            text,
            turn_id: turn_id.clone(),
            operation_key: operation_key.to_string(),
            controls,
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

    /// P4.1-A.2: solicita la cancelación del turno activo. No fija un estado
    /// terminal por sí misma: solo el evento del driver puede completar el
    /// turno como `Interrupted`. Una carrera con `stop_session` se resuelve
    /// bajo el mismo lock y el evento tardío se descarta en la ingesta.
    pub fn interrupt_turn(&self, session_id: &str, turn_id: &str) -> Result<(), AgentError> {
        let runner = {
            let mut state = self.state.lock().expect("manager poisoned");
            let record = state
                .sessions
                .get_mut(session_id)
                .ok_or_else(|| AgentError {
                    code: "not_found".into(),
                    message: format!("unknown session `{session_id}`"),
                })?;
            if record.summary.state != SessionState::Running || record.runner.is_none() {
                return Err(AgentError {
                    code: "session_stopped".into(),
                    message: format!("session `{session_id}` has no live runner"),
                });
            }
            match record.active_turn.as_deref() {
                Some(active) if active == turn_id => {}
                Some(_) => {
                    return Err(AgentError {
                        code: "TurnMismatch".into(),
                        message: "the requested turn is not the active turn".into(),
                    })
                }
                None => {
                    return Err(AgentError {
                        code: "NoActiveTurn".into(),
                        message: "session has no active turn to interrupt".into(),
                    })
                }
            }
            if !matches!(
                record.summary.turn_state,
                TurnState::Running | TurnState::AwaitingApproval
            ) {
                return Err(AgentError {
                    code: "TurnNotInterruptible".into(),
                    message: "turn is not in an interruptible state".into(),
                });
            }
            let runner = record.runner.clone().expect("runner checked");
            let _ = self.publish_locked(
                &mut state,
                session_id,
                EventKind::AgentNotice {
                    message: format!("interrupt requested for turn {turn_id}"),
                },
            );
            runner
        };
        runner
            .submit(RunnerCommand::Interrupt {
                turn_id: turn_id.to_string(),
            })
            .map_err(|error| AgentError {
                code: "InterruptDispatchFailed".into(),
                message: redact_provider_error(&format!("interrupt was not dispatched: {error}")),
            })
    }

    /// P4.1-E: controles disponibles para la sesión, con valores descubiertos
    /// del proveedor. No se aplican cambios de forma silenciosa.
    pub fn list_controls(&self, session_id: &str) -> Result<Vec<ControlState>, AgentError> {
        let (provider, desired) = {
            let state = self.state.lock().expect("manager poisoned");
            let record = state.sessions.get(session_id).ok_or_else(|| AgentError {
                code: "not_found".into(),
                message: format!("unknown session `{session_id}`"),
            })?;
            (
                record.summary.provider_id.clone(),
                record.desired_controls.clone(),
            )
        };
        let descriptor = self
            .registry
            .descriptors()
            .into_iter()
            .find(|descriptor| descriptor.id == provider)
            .ok_or_else(|| AgentError::unsupported("native controls"))?;
        let mut controls = descriptor.controls;
        for requested in desired {
            if let Some(control) = controls
                .iter_mut()
                .find(|control| control.name == requested.name)
            {
                control.current = Some(requested.value);
                control.status = "deferred".into();
            }
        }
        Ok(controls)
    }

    /// Selecciona modelo/esfuerzo para el SIGUIENTE turno. Codex expone
    /// configuración por turno; cambiar un turno activo requiere una API
    /// experimental que Fleet no habilita, así que el estado es `deferred`.
    pub fn set_control(
        &self,
        session_id: &str,
        name: &str,
        value: &str,
        scope: &str,
    ) -> Result<ControlState, AgentError> {
        if !matches!(scope, "turn" | "session") {
            return Err(AgentError {
                code: "InvalidArgument".into(),
                message: "scope must be `turn` or `session`".into(),
            });
        }
        let controls = self.list_controls(session_id)?;
        let control = controls
            .into_iter()
            .find(|control| control.name == name)
            .ok_or_else(|| AgentError::unsupported(name))?;
        if !control.mutable {
            return Err(AgentError::unsupported("read-only native control"));
        }
        if !control.available.iter().any(|item| item == value) {
            return Err(AgentError {
                code: "InvalidArgument".into(),
                message: format!("value `{value}` is not available for `{name}`"),
            });
        }
        let mut state = self.state.lock().expect("manager poisoned");
        let record = state
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| AgentError {
                code: "not_found".into(),
                message: format!("unknown session `{session_id}`"),
            })?;
        if record.runner.is_none() || record.summary.state != SessionState::Running {
            return Err(AgentError {
                code: "session_stopped".into(),
                message: "session has no live runner".into(),
            });
        }
        // Escribir antes de actualizar memoria: la preferencia y los turnos
        // futuros deben sobrevivir a la muerte de fleetd (P3).
        self.store
            .save_session_control(session_id, name, value)
            .map_err(to_agent_error)?;
        record
            .desired_controls
            .retain(|requested| requested.name != name);
        record
            .desired_controls
            .push(fleet_domain::RequestedControl {
                name: name.to_string(),
                value: value.to_string(),
            });
        Ok(ControlState {
            name: name.to_string(),
            current: Some(value.to_string()),
            available: control.available,
            mutable: true,
            status: "deferred".into(),
        })
    }

    /// P4.1-F: enumeración no intrusiva de sesiones nativas. No crea runner,
    /// no adopta procesos y no expone IDs nativos. La adopción con escritura
    /// sigue bloqueada porque Codex no ofrece garantía de exclusividad.
    pub fn list_native_sessions(
        &self,
        provider_id: &str,
    ) -> Result<Vec<fleet_agent::NativeSessionInfo>, AgentError> {
        self.registry.list_native_sessions(provider_id)
    }

    /// Importa historial con procedencia explícita. Idempotente por
    /// `operation_key`: un reintento con la misma clave no duplica eventos.
    pub fn import_native_history(
        &self,
        session_id: &str,
        provider_id: &str,
        native_ref: &str,
        operation_key: &str,
    ) -> Result<usize, AgentError> {
        {
            let state = self.state.lock().expect("manager poisoned");
            Self::ensure_history_import_target(&state, session_id, provider_id)?;
        }
        let fingerprint = format!("{session_id}\0{provider_id}\0{native_ref}");
        let operation = OperationRecord {
            operation_id: self.next_id("op"),
            operation_key: operation_key.to_string(),
            kind: OperationKind::ImportHistory,
            session_id: session_id.to_string(),
            target_id: Some(native_ref.to_string()),
            fingerprint,
            status: OperationStatus::Reserved,
            message: None,
            created_at_ms: now_ms(),
            updated_at_ms: now_ms(),
        };
        match self
            .store
            .reserve_operation(&operation)
            .map_err(to_agent_error)?
        {
            OperationReservation::Created(_) => {}
            OperationReservation::Existing(existing) => {
                // Solo el creador de la reserva puede consultar el proveedor y
                // publicar eventos. La transacción SQLite es el árbitro real.
                return Self::replay_history_import(existing);
            }
            OperationReservation::Conflict => {
                return Err(AgentError {
                    code: "Conflict".into(),
                    message: "operation key belongs to a different import".into(),
                });
            }
        }
        let messages = self
            .registry
            .import_native_history(provider_id, native_ref)
            .inspect_err(|error| {
                let _ = self.store.update_operation(
                    &operation.operation_id,
                    OperationStatus::Failed,
                    Some(redact_provider_error(&error.message)),
                );
            })?;
        if messages
            .iter()
            .any(|message| message.source_ref.as_deref().is_none_or(str::is_empty))
        {
            let _ = self.store.update_operation(
                &operation.operation_id,
                OperationStatus::Failed,
                Some("source history item lacks stable identity".into()),
            );
            return Err(AgentError {
                code: "HistoryMissingIdentity".into(),
                message: "cannot safely import historical records without stable identities".into(),
            });
        }
        let mut count = 0usize;
        {
            // Serializa la comparación y las escrituras de importaciones
            // concurrentes dentro del mismo SessionManager.
            let mut state = self.state.lock().expect("manager poisoned");
            // Durante el RPC nativo (fuera del lock) pudo iniciarse un turno,
            // detenerse la sesión o cambiar su estado. No publicar nada allí.
            if let Err(error) = Self::ensure_history_import_target(&state, session_id, provider_id)
            {
                drop(state);
                self.store
                    .update_operation(
                        &operation.operation_id,
                        OperationStatus::Failed,
                        Some(redact_provider_error(&error.message)),
                    )
                    .map_err(to_agent_error)?;
                return Err(error);
            }
            let mut existing_sources = std::collections::HashSet::new();
            let mut since = 0u64;
            loop {
                let page = self
                    .store
                    .read_events(session_id, since, TIMELINE_PAGE)
                    .map_err(to_agent_error)?;
                if page.is_empty() {
                    break;
                }
                for event in &page {
                    if let EventKind::HistoryMessage {
                        source_ref: Some(source),
                        ..
                    } = &event.kind
                    {
                        existing_sources.insert(source.clone());
                    }
                }
                let next = page.last().map(|event| event.seq).unwrap_or(since);
                if next <= since {
                    return Err(AgentError {
                        code: "TimelineCorrupt".into(),
                        message: "history deduplication cursor did not advance".into(),
                    });
                }
                since = next;
                if page.len() < TIMELINE_PAGE {
                    break;
                }
            }
            for message in messages {
                let source_ref = message.source_ref.expect("source checked above");
                if !existing_sources.insert(source_ref.clone()) {
                    continue;
                }
                if let Err(error) = self.publish_locked(
                    &mut state,
                    session_id,
                    EventKind::HistoryMessage {
                        role: message.role,
                        text: message.text,
                        provenance: "imported_from_provider".into(),
                        source_ref: Some(source_ref),
                        tool: message.tool,
                    },
                ) {
                    drop(state);
                    let _ = self.store.update_operation(
                        &operation.operation_id,
                        OperationStatus::Uncertain,
                        Some("partial history import requires reconciliation".into()),
                    );
                    return Err(error);
                }
                count += 1;
            }
        }
        self.store
            .update_operation(
                &operation.operation_id,
                OperationStatus::Completed,
                Some(count.to_string()),
            )
            .map_err(to_agent_error)?;
        Ok(count)
    }

    /// Suscribe antes de copiar el snapshot; publicaciones concurrentes quedan
    /// bloqueadas por el lock de sesión, así que no hay hueco ni duplicado.
    pub fn subscribe(
        &self,
        session_id: &str,
        since_seq: u64,
        limit: usize,
    ) -> Result<(Subscription, Vec<Event>, u64), AgentError> {
        let limit = limit.max(1);
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
        let runner_id = record
            .runner
            .as_ref()
            .map(|handle| handle.identity().runner_id.clone());
        let _ = self.publish_locked(
            &mut state,
            session_id,
            EventKind::SessionStateChanged { state: new_state },
        );
        if let Err(error) = result {
            // El error del driver puede contener URLs y IDs nativos del
            // proveedor. Solo se publica una versión redactada (P4.1-A.1).
            let message =
                redact_provider_error(&format!("driver close could not be confirmed: {error}"));
            let _ = self.publish_locked(&mut state, session_id, EventKind::AgentError { message });
        }
        drop(state);
        self.persist_session_state(session_id);
        // Persist the runner's terminal status too: otherwise the next daemon
        // start would treat a deliberately stopped runner as reattachable.
        if let Some(runner_id) = runner_id {
            let status = if result.is_ok() {
                RunnerStatus::Stopped
            } else {
                RunnerStatus::StopUnconfirmed
            };
            if let Err(error) = self.store.update_runner_status(&runner_id, status) {
                eprintln!("cannot persist runner stop status {status:?}: {error:?}");
            }
        }
    }

    fn mark_stop_unconfirmed(&self, session_id: &str, reason: &str) {
        let mut state = self.state.lock().expect("manager poisoned");
        let Some(record) = state.sessions.get_mut(session_id) else {
            return;
        };
        if record.summary.state != SessionState::Stopping {
            return;
        }
        let runner_id = record
            .runner
            .as_ref()
            .map(|handle| handle.identity().runner_id.clone());
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
                message: redact_provider_error(reason),
            },
        );
        drop(state);
        self.persist_session_state(session_id);
        if let Some(runner_id) = runner_id {
            if let Err(error) = self
                .store
                .update_runner_status(&runner_id, RunnerStatus::StopUnconfirmed)
            {
                eprintln!("cannot persist unconfirmed runner stop: {error:?}");
            }
        }
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
            if record.summary.state == SessionState::Stopped {
                return Err(AgentError {
                    code: "session_stopped".into(),
                    message: format!("session `{session_id}` already stopped"),
                });
            }
            let Some(handle) = record.runner.clone() else {
                // No runner tras un reinicio/reattach fallido: el cierre
                // explícito debe tener efecto verificable en el estado durable
                // aunque no haya un proceso al que enviar Shutdown.
                let runner_id = self
                    .store
                    .get_runner(session_id)
                    .ok()
                    .flatten()
                    .map(|record| record.identity.runner_id);
                record.summary.state = SessionState::Stopped;
                record.active_turn = None;
                record.current_operation = None;
                let _ = self.publish_locked(
                    &mut state,
                    session_id,
                    EventKind::SessionStateChanged {
                        state: SessionState::Stopped,
                    },
                );
                drop(state);
                self.persist_session_state(session_id);
                if let Some(runner_id) = runner_id {
                    let _ = self
                        .store
                        .update_runner_status(&runner_id, RunnerStatus::Stopped);
                }
                return Ok(());
            };
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
        let outcome = handle.close();
        if outcome.is_ok() {
            self.finish_stop(session_id, &outcome);
        } else if let Err(error) = &outcome {
            self.mark_stop_unconfirmed(session_id, &error.message);
        }
        // El hilo de ingress termina cuando el runner cierra; no join bloqueante.
        drop(handle);
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
    use crate::runner::InProcessRunnerLauncher;
    use fleet_agent::fake::{FakeConfig, FakeDriver, FakeFactory};
    use fleet_agent::{AgentDriver, DriverFactory};
    use fleet_domain::{ProviderCapabilities, ProviderVersion, RunnerIdentity, VersionRange};

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
        std::env::temp_dir()
            .join(format!(
                "gf-p2-core-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ))
            .join("fleet.sqlite")
    }

    fn manager_at(config: FakeConfig, path: std::path::PathBuf) -> Arc<SessionManager> {
        let mut registry = ProviderRegistry::new();
        registry.register(Box::new(ConfiguredFactory(config.clone())));
        let mut launcher_registry = ProviderRegistry::new();
        launcher_registry.register(Box::new(ConfiguredFactory(config)));
        let launcher = Arc::new(InProcessRunnerLauncher::new(
            launcher_registry,
            "host-test".into(),
        ));
        let store = Arc::new(fleet_store_sqlite::SqliteStore::open(path).unwrap());
        Arc::new(SessionManager::new(registry, store, launcher, "host-test".into()).unwrap())
    }

    fn manager(config: FakeConfig) -> Arc<SessionManager> {
        manager_at(config, unique_db_path())
    }

    struct BlockingHistoryFactory {
        config: FakeConfig,
        ready: SyncSender<()>,
        resume: Arc<Mutex<Receiver<()>>>,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl DriverFactory for BlockingHistoryFactory {
        fn id(&self) -> String {
            "fake".into()
        }

        fn compatibility(&self) -> VersionRange {
            FakeFactory::new().compatibility()
        }

        fn create(&self) -> Box<dyn AgentDriver> {
            Box::new(FakeDriver::with_config(self.config.clone()))
        }

        fn metadata(&self) -> ProviderDescriptor {
            FakeFactory::new().metadata()
        }

        fn import_native_history(
            &self,
            _native_ref: &str,
        ) -> Result<Vec<fleet_agent::NativeHistoryMessage>, AgentError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.ready.send(()).map_err(|_| AgentError {
                code: "TestSync".into(),
                message: "history test receiver disconnected".into(),
            })?;
            self.resume
                .lock()
                .expect("history test gate poisoned")
                .recv_timeout(Duration::from_secs(5))
                .map_err(|_| AgentError {
                    code: "TestSync".into(),
                    message: "history import was not released by test".into(),
                })?;
            Ok(vec![fleet_agent::NativeHistoryMessage {
                role: "assistant".into(),
                text: "historical answer".into(),
                source_ref: Some("fixture-history-item-1".into()),
                tool: None,
            }])
        }
    }

    fn manager_with_blocking_history(
        config: FakeConfig,
    ) -> (
        Arc<SessionManager>,
        Receiver<()>,
        SyncSender<()>,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let (ready_tx, ready_rx) = mpsc::sync_channel(2);
        let (resume_tx, resume_rx) = mpsc::sync_channel(2);
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut registry = ProviderRegistry::new();
        registry.register(Box::new(BlockingHistoryFactory {
            config: config.clone(),
            ready: ready_tx,
            resume: Arc::new(Mutex::new(resume_rx)),
            calls: Arc::clone(&calls),
        }));
        let mut launcher_registry = ProviderRegistry::new();
        launcher_registry.register(Box::new(ConfiguredFactory(config)));
        let launcher = Arc::new(InProcessRunnerLauncher::new(
            launcher_registry,
            "host-test".into(),
        ));
        let store = Arc::new(fleet_store_sqlite::SqliteStore::open(unique_db_path()).unwrap());
        let manager =
            Arc::new(SessionManager::new(registry, store, launcher, "host-test".into()).unwrap());
        (manager, ready_rx, resume_tx, calls)
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
                cancel_turn: true,
                approvals: false,
                can_own_process: false,
                can_attach_service: false,
            }
        );
    }

    #[test]
    fn durable_commit_failure_blocks_new_provider_effects() {
        use fleet_domain::store::SessionStore;
        let path = unique_db_path();
        let manager = manager_at(FakeConfig::default(), path.clone());
        let ws = workspace(&manager);
        let ses = session(&manager, &ws.id);
        let store = fleet_store_sqlite::SqliteStore::open(path).unwrap();
        // An unexpected durable event occupies the next Fleet cursor.
        // The subsequent commit must fail without advancing seq or publishing.
        store
            .append_event(&Event {
                event_id: "external-conflict".into(),
                session_id: ses.id.clone(),
                seq: 3,
                created_at_ms: now_ms(),
                kind: EventKind::AgentNotice {
                    message: "conflict".into(),
                },
            })
            .unwrap();
        let failure = manager
            .send_prompt(&ses.id, "first".into(), "faulty-key")
            .expect_err("durable sequence conflict must reject");
        assert_eq!(failure.code, "sequence_conflict");
        let second = manager
            .send_prompt(&ses.id, "second".into(), "another-key")
            .expect_err("storage fault must prohibit new provider effects");
        assert_eq!(second.code, "StorageUnavailable");
    }

    #[test]
    fn rejected_prompt_does_not_reserve_operation_key() {
        let manager = manager(FakeConfig {
            block_send: true,
            ..FakeConfig::default()
        });
        let ws = workspace(&manager);
        let ses = session(&manager, &ws.id);
        manager
            .send_prompt(&ses.id, "first".into(), "first-key")
            .unwrap();
        let failure = manager
            .send_prompt(&ses.id, "second".into(), "rejected-key")
            .unwrap_err();
        assert_eq!(failure.code, "TurnAlreadyRunning");
        assert!(manager.get_operation("rejected-key").unwrap().is_none());
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
        // Recovery without a live runner remains uncertain at session level,
        // but the already terminal turn must remain Completed in SQLite.
        assert_eq!(restored.turn_state, TurnState::Completed);
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

    /// Store wrapper that fails only the uncertainty write, to prove fail-closed.
    struct FailingUncertaintyStore {
        inner: Arc<dyn SessionStore>,
    }

    impl SessionStore for FailingUncertaintyStore {
        fn init(&self) -> fleet_domain::store::StoreResult<()> {
            self.inner.init()
        }
        fn health(&self) -> fleet_domain::store::StoreResult<fleet_domain::store::StoreHealth> {
            self.inner.health()
        }
        fn recover_after_restart(&self) -> fleet_domain::store::StoreResult<()> {
            self.inner.recover_after_restart()
        }
        fn save_workspace(
            &self,
            workspace: &fleet_domain::store::WorkspaceRecord,
        ) -> fleet_domain::store::StoreResult<()> {
            self.inner.save_workspace(workspace)
        }
        fn list_workspaces(
            &self,
        ) -> fleet_domain::store::StoreResult<Vec<fleet_domain::store::WorkspaceRecord>> {
            self.inner.list_workspaces()
        }
        fn save_session(
            &self,
            session: &fleet_domain::store::SessionRecord,
        ) -> fleet_domain::store::StoreResult<()> {
            self.inner.save_session(session)
        }
        fn get_session(
            &self,
            session_id: &str,
        ) -> fleet_domain::store::StoreResult<Option<fleet_domain::store::SessionRecord>> {
            self.inner.get_session(session_id)
        }
        fn list_sessions(
            &self,
        ) -> fleet_domain::store::StoreResult<Vec<fleet_domain::store::SessionRecord>> {
            self.inner.list_sessions()
        }
        fn max_seq(&self, session_id: &str) -> fleet_domain::store::StoreResult<u64> {
            self.inner.max_seq(session_id)
        }
        fn append_event(&self, event: &Event) -> fleet_domain::store::StoreResult<()> {
            self.inner.append_event(event)
        }
        fn commit_event(
            &self,
            event: &Event,
            session: &fleet_domain::store::SessionRecord,
            approval: Option<&ApprovalRequest>,
        ) -> fleet_domain::store::StoreResult<()> {
            self.inner.commit_event(event, session, approval)
        }
        fn commit_runner_event(
            &self,
            runner_id: &str,
            runner_seq: u64,
            event: &Event,
            session: &fleet_domain::store::SessionRecord,
            approval: Option<&ApprovalRequest>,
        ) -> fleet_domain::store::StoreResult<bool> {
            self.inner
                .commit_runner_event(runner_id, runner_seq, event, session, approval)
        }
        fn read_events(
            &self,
            session_id: &str,
            since_seq: u64,
            limit: usize,
        ) -> fleet_domain::store::StoreResult<Vec<Event>> {
            self.inner.read_events(session_id, since_seq, limit)
        }
        fn reserve_operation(
            &self,
            operation: &OperationRecord,
        ) -> fleet_domain::store::StoreResult<OperationReservation> {
            self.inner.reserve_operation(operation)
        }
        fn get_operation(
            &self,
            operation_key: &str,
        ) -> fleet_domain::store::StoreResult<Option<OperationRecord>> {
            self.inner.get_operation(operation_key)
        }
        fn update_operation(
            &self,
            operation_id: &str,
            status: OperationStatus,
            message: Option<String>,
        ) -> fleet_domain::store::StoreResult<()> {
            self.inner.update_operation(operation_id, status, message)
        }
        fn save_turn(&self, turn: &TurnRecord) -> fleet_domain::store::StoreResult<()> {
            self.inner.save_turn(turn)
        }
        fn get_turn(&self, turn_id: &str) -> fleet_domain::store::StoreResult<Option<TurnRecord>> {
            self.inner.get_turn(turn_id)
        }
        fn save_approval(
            &self,
            approval: &ApprovalRequest,
        ) -> fleet_domain::store::StoreResult<()> {
            self.inner.save_approval(approval)
        }
        fn claim_approval(
            &self,
            approval_id: &str,
            decision: &str,
            operation: &OperationRecord,
        ) -> fleet_domain::store::StoreResult<ApprovalClaim> {
            self.inner.claim_approval(approval_id, decision, operation)
        }
        fn mark_approval_uncertain(
            &self,
            _approval_id: &str,
            _reason: &str,
        ) -> fleet_domain::store::StoreResult<()> {
            Err(StoreError::new(
                "store_write_failed",
                "injected uncertainty write failure",
            ))
        }
        fn active_turn_for_session(
            &self,
            session_id: &str,
        ) -> fleet_domain::store::StoreResult<Option<TurnRecord>> {
            self.inner.active_turn_for_session(session_id)
        }
        fn mark_session_approvals_uncertain(
            &self,
            session_id: &str,
        ) -> fleet_domain::store::StoreResult<()> {
            self.inner.mark_session_approvals_uncertain(session_id)
        }
        fn save_runner(&self, runner: &RunnerRecord) -> fleet_domain::store::StoreResult<()> {
            self.inner.save_runner(runner)
        }
        fn get_runner(
            &self,
            session_id: &str,
        ) -> fleet_domain::store::StoreResult<Option<RunnerRecord>> {
            self.inner.get_runner(session_id)
        }
        fn list_runners(&self) -> fleet_domain::store::StoreResult<Vec<RunnerRecord>> {
            self.inner.list_runners()
        }
        fn update_runner_status(
            &self,
            runner_id: &str,
            status: RunnerStatus,
        ) -> fleet_domain::store::StoreResult<()> {
            self.inner.update_runner_status(runner_id, status)
        }
        fn update_runner_ack(
            &self,
            runner_id: &str,
            runner_seq: u64,
        ) -> fleet_domain::store::StoreResult<()> {
            self.inner.update_runner_ack(runner_id, runner_seq)
        }
        fn runner_event_exists(
            &self,
            runner_id: &str,
            runner_seq: u64,
        ) -> fleet_domain::store::StoreResult<bool> {
            self.inner.runner_event_exists(runner_id, runner_seq)
        }
        fn record_runner_event(
            &self,
            runner_id: &str,
            runner_seq: u64,
            fleet_event_id: &str,
        ) -> fleet_domain::store::StoreResult<bool> {
            self.inner
                .record_runner_event(runner_id, runner_seq, fleet_event_id)
        }
        fn pending_runner_events(
            &self,
            runner_id: &str,
            since_seq: u64,
            limit: usize,
        ) -> fleet_domain::store::StoreResult<Vec<(u64, String)>> {
            self.inner
                .pending_runner_events(runner_id, since_seq, limit)
        }
        fn get_approval(
            &self,
            approval_id: &str,
        ) -> fleet_domain::store::StoreResult<Option<ApprovalRequest>> {
            self.inner.get_approval(approval_id)
        }
        fn list_approvals(
            &self,
            session_id: Option<&str>,
        ) -> fleet_domain::store::StoreResult<Vec<ApprovalRequest>> {
            self.inner.list_approvals(session_id)
        }
        fn resolve_approval(
            &self,
            approval_id: &str,
            decision: &str,
            status: ApprovalStatus,
        ) -> fleet_domain::store::StoreResult<bool> {
            self.inner.resolve_approval(approval_id, decision, status)
        }
        fn update_approval_status(
            &self,
            approval_id: &str,
            status: ApprovalStatus,
        ) -> fleet_domain::store::StoreResult<()> {
            self.inner.update_approval_status(approval_id, status)
        }
    }

    #[test]
    fn native_confirmation_is_scoped_to_its_session() {
        let path = unique_db_path();
        let manager = manager_at(FakeConfig::default(), path.clone());
        let store = fleet_store_sqlite::SqliteStore::open(path).unwrap();
        let ws = workspace(&manager);
        let a = session(&manager, &ws.id);
        let b = session(&manager, &ws.id);
        let approval_a = ApprovalRequest {
            approval_id: "appr_a".into(),
            session_id: a.id.clone(),
            turn_id: "turn_a".into(),
            kind: "shell".into(),
            message: "a".into(),
            resources: vec![],
            allowed_decisions: vec!["once".into()],
            status: ApprovalStatus::Dispatching,
            native_request_id: "same-native-id".into(),
            decision: Some("once".into()),
            created_at_ms: 1,
            resolved_at_ms: None,
        };
        let approval_b = ApprovalRequest {
            approval_id: "appr_b".into(),
            session_id: b.id.clone(),
            turn_id: "turn_b".into(),
            message: "b".into(),
            ..approval_a.clone()
        };
        for (session_id, turn_id) in [(a.id.as_str(), "turn_a"), (b.id.as_str(), "turn_b")] {
            store
                .save_turn(&TurnRecord {
                    turn_id: turn_id.to_string(),
                    session_id: session_id.to_string(),
                    operation_id: "op_scoped".into(),
                    state: TurnState::Running,
                    created_at_ms: 1,
                    updated_at_ms: 1,
                })
                .unwrap();
        }
        store.save_approval(&approval_a).unwrap();
        store.save_approval(&approval_b).unwrap();
        {
            let mut state = manager.state.lock().expect("manager poisoned");
            state.approvals.insert("appr_a".into(), approval_a.clone());
            state.approvals.insert("appr_b".into(), approval_b.clone());
        }
        manager
            .publish_agent(
                &b.id,
                AgentEvent {
                    kind: DomainEvent::ApprovalResolved {
                        native_request_id: "same-native-id".into(),
                        decision: "once".into(),
                    },
                },
            )
            .unwrap();
        let resolved = manager.get_approval("appr_b").unwrap();
        assert_eq!(resolved.status, ApprovalStatus::Accepted);
        assert_eq!(
            manager.get_approval("appr_a").unwrap().status,
            ApprovalStatus::Dispatching
        );
    }

    #[test]
    fn uncertain_write_failure_latches_storage_fault() {
        let path = unique_db_path();
        let inner: Arc<dyn SessionStore> =
            Arc::new(fleet_store_sqlite::SqliteStore::open(path).unwrap());
        let failing = Arc::new(FailingUncertaintyStore { inner });
        let mut registry = ProviderRegistry::new();
        registry.register(Box::new(ConfiguredFactory(FakeConfig::default())));
        let mut launcher_registry = ProviderRegistry::new();
        launcher_registry.register(Box::new(ConfiguredFactory(FakeConfig::default())));
        let launcher = Arc::new(InProcessRunnerLauncher::new(
            launcher_registry,
            "host-test".into(),
        ));
        let manager =
            Arc::new(SessionManager::new(registry, failing, launcher, "host-test".into()).unwrap());
        let ws = workspace(&manager);
        let ses = session(&manager, &ws.id);
        manager.mark_approval_uncertain(
            &ses.id,
            "missing",
            &AgentError {
                code: "test".into(),
                message: "send failed".into(),
            },
        );
        let error = manager
            .send_prompt(&ses.id, "new".into(), "after-fault")
            .unwrap_err();
        assert_eq!(error.code, "StorageUnavailable");
    }

    #[test]
    fn answer_replay_survives_storage_fault() {
        let path = unique_db_path();
        let store: Arc<dyn SessionStore> =
            Arc::new(fleet_store_sqlite::SqliteStore::open(path).unwrap());
        let config = FakeConfig {
            emit_approval: true,
            ..FakeConfig::default()
        };
        let mut registry = ProviderRegistry::new();
        registry.register(Box::new(ConfiguredFactory(config.clone())));
        let mut launcher_registry = ProviderRegistry::new();
        launcher_registry.register(Box::new(ConfiguredFactory(config)));
        let launcher = Arc::new(InProcessRunnerLauncher::new(
            launcher_registry,
            "host-test".into(),
        ));
        let manager =
            Arc::new(SessionManager::new(registry, store, launcher, "host-test".into()).unwrap());
        let ws = workspace(&manager);
        let ses = session(&manager, &ws.id);
        let (subscription, _, _) = manager.subscribe(&ses.id, 0, SUBSCRIBER_LIMIT).unwrap();
        manager.send_prompt(&ses.id, "run".into(), "op-1").unwrap();
        let approval_id = loop {
            match subscription.recv_event(Duration::from_millis(100)) {
                Ok(Some(Event {
                    kind: EventKind::ApprovalRequested { approval_id, .. },
                    ..
                })) => break approval_id,
                Ok(Some(_)) | Ok(None) => {}
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => panic!("closed"),
            }
        };
        let first = manager
            .answer_approval(&approval_id, "once".into(), "answer-key")
            .unwrap();
        assert_eq!(first.status, ApprovalStatus::Dispatching);
        // Simulate a later storage fault from an unrelated event.
        manager
            .state
            .lock()
            .expect("manager poisoned")
            .storage_fault = Some("injected".into());
        let replay = manager
            .answer_approval(&approval_id, "once".into(), "answer-key")
            .unwrap();
        assert_eq!(replay.approval_id, first.approval_id);
    }

    #[test]
    fn confirmed_stop_persists_runner_status_and_is_not_revived() {
        let path = unique_db_path();
        let manager = manager_at(FakeConfig::default(), path.clone());
        let ws = workspace(&manager);
        let ses = session(&manager, &ws.id);
        manager.stop_session(&ses.id).unwrap();
        assert_eq!(
            manager.store.get_runner(&ses.id).unwrap().unwrap().status,
            RunnerStatus::Stopped
        );
        drop(manager);

        let reopened = manager_at(FakeConfig::default(), path);
        let recovered = reopened.recover_runners().unwrap();
        assert!(
            recovered.is_empty(),
            "a stopped session must not be reattached"
        );
        let restored = reopened
            .list_sessions()
            .into_iter()
            .find(|item| item.id == ses.id)
            .unwrap();
        assert_eq!(restored.state, SessionState::Stopped);
        assert_eq!(
            reopened.store.get_runner(&ses.id).unwrap().unwrap().status,
            RunnerStatus::Stopped
        );
    }

    #[test]
    fn duplicate_recovery_does_not_spawn_second_ingress() {
        let manager = manager(FakeConfig::default());
        let ws = workspace(&manager);
        let ses = session(&manager, &ws.id);
        // Simulate a daemon restart with a still-attached runner: the guard
        // must skip rather than starting a second ingress over the same stream.
        {
            let mut state = manager.state.lock().expect("manager poisoned");
            if let Some(record) = state.sessions.get_mut(&ses.id) {
                record.summary.state = SessionState::NeedsReconciliation;
            }
        }
        let recovered = manager.recover_runners().unwrap();
        assert!(
            recovered.is_empty(),
            "recovery must skip a session that already has a runner and ingress"
        );
        manager.stop_session(&ses.id).unwrap();
    }

    #[test]
    fn stop_after_failed_reattach_finalizes_durable_state() {
        let path = unique_db_path();
        let manager = manager_at(FakeConfig::default(), path.clone());
        let ws = workspace(&manager);
        let ses = session(&manager, &ws.id);
        drop(manager);

        let reopened = manager_at(FakeConfig::default(), path);
        // In-process launcher cannot reattach, so recovery leaves the session
        // uncertain with no runner. An explicit Stop must still be effective.
        let _ = reopened.recover_runners().unwrap();
        let before = reopened
            .list_sessions()
            .into_iter()
            .find(|item| item.id == ses.id)
            .unwrap();
        assert_eq!(before.state, SessionState::NeedsReconciliation);
        reopened.stop_session(&ses.id).unwrap();
        let after = reopened
            .list_sessions()
            .into_iter()
            .find(|item| item.id == ses.id)
            .unwrap();
        assert_eq!(after.state, SessionState::Stopped);
        assert_eq!(
            reopened.store.get_runner(&ses.id).unwrap().unwrap().status,
            RunnerStatus::Stopped
        );
    }

    struct FailAckHandle(Box<dyn RunnerHandle>);

    impl RunnerHandle for FailAckHandle {
        fn identity(&self) -> &RunnerIdentity {
            self.0.identity()
        }
        fn submit(&self, command: RunnerCommand) -> Result<(), AgentError> {
            self.0.submit(command)
        }
        fn recv(&self, timeout: Duration) -> Result<Option<RunnerEvent>, AgentError> {
            self.0.recv(timeout)
        }
        fn ack(&self, _runner_seq: u64) -> Result<(), AgentError> {
            Err(AgentError {
                code: "ack_failed".into(),
                message: "injected runner ACK failure".into(),
            })
        }
        fn close(&self) -> Result<(), AgentError> {
            self.0.close()
        }
    }

    struct FailAckLauncher(InProcessRunnerLauncher);

    impl RunnerLauncher for FailAckLauncher {
        fn launch(&self, ctx: RunnerLaunchContext) -> Result<Box<dyn RunnerHandle>, AgentError> {
            Ok(Box::new(FailAckHandle(self.0.launch(ctx)?)))
        }
    }

    #[test]
    fn ack_failure_is_reported_instead_of_silently_replaying() {
        let path = unique_db_path();
        let store: Arc<dyn SessionStore> =
            Arc::new(fleet_store_sqlite::SqliteStore::open(path).unwrap());
        let mut registry = ProviderRegistry::new();
        registry.register(Box::new(ConfiguredFactory(FakeConfig::default())));
        let mut launcher_registry = ProviderRegistry::new();
        launcher_registry.register(Box::new(ConfiguredFactory(FakeConfig::default())));
        let launcher = Arc::new(FailAckLauncher(InProcessRunnerLauncher::new(
            launcher_registry,
            "host-test".into(),
        )));
        let manager =
            Arc::new(SessionManager::new(registry, store, launcher, "host-test".into()).unwrap());
        let ws = workspace(&manager);
        let ses = session(&manager, &ws.id);
        let receipt = manager
            .send_prompt(&ses.id, "ack test".into(), "ack-key")
            .unwrap();
        let runner_id = manager
            .store
            .get_runner(&ses.id)
            .unwrap()
            .unwrap()
            .identity
            .runner_id;
        let error = manager
            .ingest_runner_event(
                &ses.id,
                &runner_id,
                1,
                AgentEvent {
                    kind: DomainEvent::AssistantDelta {
                        turn_id: receipt.turn_id,
                        text: "x".into(),
                    },
                },
            )
            .unwrap_err();
        assert_eq!(error.code, "ack_failed");
        manager.stop_session(&ses.id).unwrap();
    }

    #[test]
    fn stop_path_public_events_redact_native_ids_and_urls() {
        let manager = manager(FakeConfig::default());
        let ws = workspace(&manager);
        let ses = session(&manager, &ws.id);
        {
            let mut state = manager.state.lock().expect("manager poisoned");
            state.sessions.get_mut(&ses.id).unwrap().summary.state = SessionState::Stopping;
        }

        manager.mark_stop_unconfirmed(
            &ses.id,
            "close failed at http://127.0.0.1:1/api/session/ses_secret/per_secret",
        );
        let notice = manager
            .read_timeline(&ses.id, 0, 128)
            .unwrap()
            .into_iter()
            .find_map(|event| match event.kind {
                EventKind::AgentNotice { message } => Some(message),
                _ => None,
            })
            .expect("AgentNotice must be published for an unconfirmed stop");
        assert!(!notice.contains("ses_secret"));
        assert!(!notice.contains("per_secret"));
        assert!(!notice.contains("http://"));

        {
            let mut state = manager.state.lock().expect("manager poisoned");
            state.sessions.get_mut(&ses.id).unwrap().summary.state = SessionState::Stopping;
        }
        manager.finish_stop(
            &ses.id,
            &Err(AgentError {
                code: "close_failed".into(),
                message: "close failed https://host.example/api/exec_exc/msg_secret".into(),
            }),
        );
        let public_error = manager
            .read_timeline(&ses.id, 0, 128)
            .unwrap()
            .into_iter()
            .filter_map(|event| match event.kind {
                EventKind::AgentError { message } => Some(message),
                _ => None,
            })
            .find(|message| message.contains("close failed"))
            .expect("AgentError must be published for a failed close");
        assert!(public_error.contains("[url redacted]"));
        assert!(!public_error.contains("exec_exc"));
        assert!(!public_error.contains("msg_secret"));
        manager.stop_session(&ses.id).unwrap();
    }

    #[test]
    fn interrupt_reaches_driver_and_completes_turn() {
        let manager = manager(FakeConfig {
            defer_terminal_until_interrupt: true,
            ..FakeConfig::default()
        });
        let ws = workspace(&manager);
        let ses = session(&manager, &ws.id);
        let (subscription, _, _) = manager.subscribe(&ses.id, 0, 64).unwrap();
        let receipt = manager
            .send_prompt(&ses.id, "long running task".into(), "op-interrupt-ok")
            .unwrap();

        manager
            .interrupt_turn(&ses.id, &receipt.turn_id)
            .expect("interrupt must be accepted for the active turn");
        let events = collect_terminal(&subscription);
        assert!(events.iter().any(|event| matches!(
            event.kind,
            EventKind::TurnCompleted {
                status: TurnStatus::Interrupted,
                ..
            }
        )));
        // Una segunda interrupción sobre un turno ya terminal se rechaza;
        // no produce una segunda terminalidad ni reenvía nada.
        assert_eq!(
            manager
                .interrupt_turn(&ses.id, &receipt.turn_id)
                .unwrap_err()
                .code,
            "NoActiveTurn"
        );
        manager.stop_session(&ses.id).unwrap();
    }

    #[test]
    fn interrupt_rejects_wrong_turn_and_failed_delivery_is_not_terminal() {
        let manager = manager(FakeConfig {
            defer_terminal_until_interrupt: true,
            fail_interrupt: true,
            ..FakeConfig::default()
        });
        let ws = workspace(&manager);
        let ses = session(&manager, &ws.id);
        let receipt = manager
            .send_prompt(&ses.id, "task".into(), "op-interrupt-fail")
            .unwrap();

        assert_eq!(
            manager
                .interrupt_turn(&ses.id, "turn-not-active")
                .unwrap_err()
                .code,
            "TurnMismatch"
        );
        manager
            .interrupt_turn(&ses.id, &receipt.turn_id)
            .expect("dispatch itself succeeds; delivery failure arrives as event");
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut saw_delivery_error = false;
        while std::time::Instant::now() < deadline {
            let events = manager.read_timeline(&ses.id, 0, 128).unwrap();
            saw_delivery_error = events.iter().any(|event| matches!(
                &event.kind,
                EventKind::AgentError { message } if message.contains("interrupt_delivery_failed")
            ));
            if saw_delivery_error {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            saw_delivery_error,
            "failed interrupt delivery must be a public error event"
        );
        let events = manager.read_timeline(&ses.id, 0, 128).unwrap();
        assert!(!events
            .iter()
            .any(|event| matches!(event.kind, EventKind::TurnCompleted { .. })));
        manager.stop_session(&ses.id).unwrap();
    }

    #[test]
    fn interrupt_does_not_resurrect_turn_after_stop() {
        let manager = manager(FakeConfig {
            defer_terminal_until_interrupt: true,
            ..FakeConfig::default()
        });
        let ws = workspace(&manager);
        let ses = session(&manager, &ws.id);
        let receipt = manager
            .send_prompt(&ses.id, "task".into(), "op-interrupt-stop-race")
            .unwrap();
        manager.stop_session(&ses.id).unwrap();
        // El turno quedó Uncertain al cerrar; una interrupción posterior no
        // debe resucitarlo ni generar una terminalidad tardía.
        manager
            .interrupt_turn(&ses.id, &receipt.turn_id)
            .expect_err("stopped session must reject new interrupts");
        let events = manager.read_timeline(&ses.id, 0, 128).unwrap();
        assert!(!events.iter().any(|event| matches!(
            event.kind,
            EventKind::TurnCompleted {
                status: TurnStatus::Interrupted,
                ..
            }
        )));
    }
    #[test]
    fn typed_approval_fingerprint_is_sensitive_to_arguments() {
        let args = vec![
            fleet_domain::ApprovalArgument {
                name: "permissions".into(),
                values: vec!["read".into()],
            },
            fleet_domain::ApprovalArgument {
                name: "scope".into(),
                values: vec!["turn".into()],
            },
        ];
        let mut reordered = args.clone();
        reordered.reverse();
        let original = approval_answer_fingerprint("s", "a", "grant", &args);
        assert_eq!(
            original,
            approval_answer_fingerprint("s", "a", "grant", &reordered)
        );
        let mut different = args.clone();
        different[0].values = vec!["write".into()];
        assert_ne!(
            original,
            approval_answer_fingerprint("s", "a", "grant", &different)
        );
        assert_eq!(
            approval_answer_fingerprint("s", "a", "once", &[]),
            "s\0a\0once"
        );
    }
    #[test]
    fn approval_arguments_are_checked_before_reservation() {
        let spec = fleet_domain::ApprovalOptionSpec {
            option_id: "grant".into(),
            label: "Grant".into(),
            args: vec![
                fleet_domain::ApprovalArgSpec {
                    name: "permissions".into(),
                    kind: "text_list".into(),
                    required: true,
                },
                fleet_domain::ApprovalArgSpec {
                    name: "scope".into(),
                    kind: "text".into(),
                    required: false,
                },
            ],
        };
        assert!(check_approval_fields(Some(&spec), &[]).is_err());
        assert!(check_approval_fields(
            Some(&spec),
            &[fleet_domain::ApprovalArgument {
                name: "unknown".into(),
                values: vec!["value".into()],
            }]
        )
        .is_err());
        assert!(check_approval_fields(
            Some(&spec),
            &[fleet_domain::ApprovalArgument {
                name: "permissions".into(),
                values: vec!["read".into()],
            }]
        )
        .is_ok());
    }
    #[test]
    fn history_import_rechecks_session_after_slow_native_fetch() {
        let config = FakeConfig {
            defer_terminal_until_interrupt: true,
            ..FakeConfig::default()
        };
        let (manager, started, release, calls) = manager_with_blocking_history(config);
        let ws = workspace(&manager);
        let ses = session(&manager, &ws.id);
        let importing = Arc::clone(&manager);
        let session_id = ses.id.clone();
        let task = thread::spawn(move || {
            importing.import_native_history(&session_id, "fake", "native-1", "import-race")
        });
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        manager
            .send_prompt(&ses.id, "active turn".into(), "prompt-while-importing")
            .unwrap();
        release.send(()).unwrap();
        let error = task.join().unwrap().unwrap_err();
        assert_eq!(error.code, "SessionBusy");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            manager
                .get_operation("import-race")
                .unwrap()
                .unwrap()
                .status,
            OperationStatus::Failed
        );
        assert!(manager
            .read_timeline(&ses.id, 0, TIMELINE_PAGE)
            .unwrap()
            .iter()
            .all(|event| !matches!(event.kind, EventKind::HistoryMessage { .. })));
        manager.stop_session(&ses.id).unwrap();
    }

    #[test]
    fn same_import_key_has_one_owner_and_replays_completed_result() {
        let (manager, started, release, calls) =
            manager_with_blocking_history(FakeConfig::default());
        let ws = workspace(&manager);
        let ses = session(&manager, &ws.id);
        let owner = Arc::clone(&manager);
        let session_id = ses.id.clone();
        let owner_thread = thread::spawn(move || {
            owner.import_native_history(&session_id, "fake", "native-1", "same-key")
        });
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        let contender = Arc::clone(&manager);
        let contender_session = ses.id.clone();
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let contender_thread = thread::spawn(move || {
            let outcome =
                contender.import_native_history(&contender_session, "fake", "native-1", "same-key");
            result_tx.send(outcome).unwrap();
        });
        let contender_result = result_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("contender must not execute the slow provider import")
            .unwrap_err();
        assert_eq!(contender_result.code, "OperationPending");
        release.send(()).unwrap();
        assert_eq!(owner_thread.join().unwrap().unwrap(), 1);
        contender_thread.join().unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            manager
                .import_native_history(&ses.id, "fake", "native-1", "same-key")
                .unwrap(),
            1
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let history_count = manager
            .read_timeline(&ses.id, 0, TIMELINE_PAGE)
            .unwrap()
            .iter()
            .filter(|event| matches!(event.kind, EventKind::HistoryMessage { .. }))
            .count();
        assert_eq!(history_count, 1);
        manager.stop_session(&ses.id).unwrap();
    }
}

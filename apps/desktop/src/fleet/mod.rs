//! Fleet SDK adapter for the GPUI application.
//!
//! This module is the only Desktop boundary to `fleet-client`. It deliberately
//! does not import transports, daemon internals, provider drivers, or storage.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::{Duration, Instant};

use fleet_client::{
    AgentSummary, ClientError, FleetClient, Host, SessionSummary, WorkspaceSummary,
};

/// Upper bound for the UI waiting on one SDK operation (connect plus three lists).
///
/// A timeout ends the wait, NOT the underlying OS thread. The worker permit
/// remains owned by that thread until the transport operation actually exits,
/// so repeated timeouts cannot create unbounded connections or threads.
pub const OPERATION_DEADLINE: Duration = Duration::from_secs(20);
const CANCELLATION_POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MachineId {
    Local,
    Ssh(String),
}

impl MachineId {
    pub fn label(&self) -> &str {
        match self {
            Self::Local => "Local",
            Self::Ssh(alias) => alias,
        }
    }

    fn sdk_host(&self) -> Host {
        match self {
            Self::Local => Host::Local,
            Self::Ssh(alias) => Host::Ssh(alias.clone()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineKind {
    Local,
    Ssh,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionStatus {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting,
    Error,
    Unavailable,
}

pub fn status_for_error(machine: &MachineId, error: &SafeFleetError) -> ConnectionStatus {
    if *machine == MachineId::Local && error.kind == FleetErrorKind::DaemonUnavailable {
        ConnectionStatus::Unavailable
    } else {
        ConnectionStatus::Error
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FleetErrorKind {
    Transport,
    SshAuthentication,
    UntrustedHostKey,
    BridgeMissing,
    DaemonUnavailable,
    IncompatibleProtocol,
    Timeout,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafeFleetError {
    pub kind: FleetErrorKind,
    pub message: &'static str,
}

pub fn safe_error(error: &ClientError) -> SafeFleetError {
    let text = error.to_string().to_ascii_lowercase();
    let (kind, message) = if text.contains("hostkeyrejected")
        || text.contains("host key verification failed")
        || text.contains("strict checking")
        || text.contains("remote host identification has changed")
    {
        (
            FleetErrorKind::UntrustedHostKey,
            "La identidad SSH del host no está verificada. Revisa known_hosts antes de reintentar.",
        )
    } else if text.contains("authfailed") || text.contains("permission denied") {
        (
            FleetErrorKind::SshAuthentication,
            "OpenSSH rechazó la autenticación. Revisa tu configuración SSH.",
        )
    } else if text.contains("bridgemissing") || text.contains("fleet-ssh-bridge: not found") {
        (
            FleetErrorKind::BridgeMissing,
            "No se encontró fleet-ssh-bridge en el host remoto.",
        )
    } else if text.contains("sshunreachable") {
        (
            FleetErrorKind::Transport,
            "No se pudo alcanzar el host SSH. Comprueba la conectividad e inténtalo de nuevo.",
        )
    } else if text.contains("daemondown")
        || text.contains("daemon unavailable")
        || text.contains("connection refused")
        || text.contains("fleet connection closed")
    {
        (
            FleetErrorKind::DaemonUnavailable,
            "fleetd no está disponible en esta máquina.",
        )
    } else if text.contains("incompatible fleet wire") || text.contains("incompatibleprotocol") {
        (
            FleetErrorKind::IncompatibleProtocol,
            "La versión del protocolo Fleet no es compatible.",
        )
    } else if matches!(error, ClientError::Transport(_)) {
        (
            FleetErrorKind::Transport,
            "No se pudo establecer o mantener la conexión con Fleet.",
        )
    } else {
        (
            FleetErrorKind::Other,
            "Fleet devolvió un error al consultar la máquina.",
        )
    };
    SafeFleetError { kind, message }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineCatalogEntry {
    pub id: MachineId,
    pub kind: MachineKind,
}

pub fn discover_catalog() -> Result<Vec<MachineCatalogEntry>, SafeFleetError> {
    let mut entries = vec![MachineCatalogEntry {
        id: MachineId::Local,
        kind: MachineKind::Local,
    }];
    let hosts = fleet_client::configured_hosts().map_err(|error| safe_error(&error))?;
    entries.extend(hosts.into_iter().map(|host| MachineCatalogEntry {
        id: MachineId::Ssh(host.alias),
        kind: MachineKind::Ssh,
    }));
    Ok(entries)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineSnapshot {
    pub agents: Vec<AgentSummary>,
    pub workspaces: Vec<WorkspaceSummary>,
    pub sessions: Vec<SessionSummary>,
}

/// Opaque live SDK connection retained for the selected machine.
pub struct Connection {
    client: FleetClient,
}

impl Connection {
    pub fn disconnect(&mut self) {
        self.client.disconnect();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineRow {
    pub entry: MachineCatalogEntry,
    pub status: ConnectionStatus,
    pub expanded: bool,
    pub snapshot: Option<MachineSnapshot>,
    pub error: Option<SafeFleetError>,
}

impl MachineRow {
    pub fn new(entry: MachineCatalogEntry) -> Self {
        Self {
            entry,
            status: ConnectionStatus::Disconnected,
            expanded: true,
            snapshot: None,
            error: None,
        }
    }

    /// Drop any previously loaded data: a closed or lost connection must never
    /// keep rendering an earlier snapshot as if it were current.
    pub fn mark_disconnected(&mut self) {
        self.status = ConnectionStatus::Disconnected;
        self.snapshot = None;
        self.error = None;
    }

    pub fn mark_failed(&mut self, status: ConnectionStatus, error: SafeFleetError) {
        self.status = status;
        self.snapshot = None;
        self.error = Some(error);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FleetUiState {
    pub machines: Vec<MachineRow>,
    pub selected: Option<MachineId>,
    pub request_generation: u64,
    pub active_request_machine: Option<MachineId>,
    pub catalog_generation: u64,
    pub catalog_error: Option<SafeFleetError>,
}

pub fn response_is_current(
    current_generation: u64,
    selected: Option<&MachineId>,
    request_generation: u64,
    request_machine: &MachineId,
) -> bool {
    current_generation == request_generation && selected == Some(request_machine)
}

impl FleetUiState {
    pub fn row(&self, id: &MachineId) -> Option<&MachineRow> {
        self.machines.iter().find(|row| &row.entry.id == id)
    }

    pub fn row_mut(&mut self, id: &MachineId) -> Option<&mut MachineRow> {
        self.machines.iter_mut().find(|row| &row.entry.id == id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionIdentity {
    pub machine: MachineId,
    pub session_id: String,
}

/// A cancellation token is checked between every blocking SDK operation.
/// In-flight I/O is bounded by the SDK/OpenSSH connect timeout and its result
/// is discarded by the caller's request generation after cancellation.
#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

fn cancelled_error() -> SafeFleetError {
    SafeFleetError {
        kind: FleetErrorKind::Other,
        message: "La consulta fue cancelada.",
    }
}

fn timeout_error() -> SafeFleetError {
    SafeFleetError {
        kind: FleetErrorKind::Timeout,
        message: "La operación Fleet no respondió a tiempo. Puedes reintentar.",
    }
}

/// Run a blocking SDK operation on a dedicated thread, returning on cancellation
/// or deadline without blocking GPUI. Ownership of the permit moves to the REAL
/// transport worker. It is not released when the caller stops waiting.
pub fn run_bounded<T, F>(
    cancellation: &Cancellation,
    permit: WorkerPermit,
    operation: F,
) -> Result<T, SafeFleetError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, SafeFleetError> + Send + 'static,
{
    run_bounded_with_deadline(cancellation, OPERATION_DEADLINE, permit, operation)
}

pub fn run_bounded_with_deadline<T, F>(
    cancellation: &Cancellation,
    deadline: Duration,
    permit: WorkerPermit,
    operation: F,
) -> Result<T, SafeFleetError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, SafeFleetError> + Send + 'static,
{
    if cancellation.is_cancelled() {
        return Err(cancelled_error());
    }
    if deadline.is_zero() {
        return Err(timeout_error());
    }
    let (sender, receiver) = mpsc::channel();
    let worker = std::thread::Builder::new()
        .name("fleet-desktop-sdk".into())
        .spawn(move || {
            // This guard is held for the lifetime of the blocking operation.
            // Never return the slot to the UI when recv_timeout expires.
            let _permit = permit;
            let _ = sender.send(operation());
        })
        .map_err(|_| SafeFleetError {
            kind: FleetErrorKind::Other,
            message: "No se pudo iniciar la consulta Fleet.",
        })?;
    let started = Instant::now();
    loop {
        if cancellation.is_cancelled() {
            // The detached worker still closes its own transport when it ends.
            return Err(cancelled_error());
        }
        let remaining = deadline.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(timeout_error());
        }
        match receiver.recv_timeout(remaining.min(CANCELLATION_POLL)) {
            Ok(result) => {
                let _ = worker.join();
                return result;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = worker.join();
                return Err(SafeFleetError {
                    kind: FleetErrorKind::Other,
                    message: "La consulta Fleet terminó de forma inesperada.",
                });
            }
        }
    }
}

/// Limits the number of ACTUAL blocking Fleet SDK operations, including
/// operations whose callers already timed out or cancelled.
#[derive(Clone, Default)]
pub struct FleetWorkers(Arc<std::sync::atomic::AtomicUsize>);

impl FleetWorkers {
    pub fn try_acquire(&self) -> Option<WorkerPermit> {
        let mut active = self.0.load(Ordering::Acquire);
        loop {
            if active >= 2 {
                return None;
            }
            match self.0.compare_exchange_weak(
                active,
                active + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(WorkerPermit(self.0.clone())),
                Err(current) => active = current,
            }
        }
    }
}

pub struct WorkerPermit(Arc<std::sync::atomic::AtomicUsize>);

impl Drop for WorkerPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn check_cancelled(
    cancellation: &Cancellation,
    client: &mut FleetClient,
) -> Result<(), SafeFleetError> {
    if cancellation.is_cancelled() {
        client.disconnect();
        return Err(cancelled_error());
    }
    Ok(())
}

fn inspect_blocking(
    machine: &MachineId,
    cancellation: &Cancellation,
    connection: Option<Connection>,
) -> Result<(MachineSnapshot, Connection), SafeFleetError> {
    if cancellation.is_cancelled() {
        return Err(cancelled_error());
    }
    let mut client = match connection {
        Some(connection) if connection.client.host() == &machine.sdk_host() => connection.client,
        Some(mut connection) => {
            connection.disconnect();
            FleetClient::connect(machine.sdk_host()).map_err(|error| safe_error(&error))?
        }
        None => FleetClient::connect(machine.sdk_host()).map_err(|error| safe_error(&error))?,
    };
    check_cancelled(cancellation, &mut client)?;
    let agents = client.list_agents().map_err(|error| safe_error(&error))?;
    check_cancelled(cancellation, &mut client)?;
    let workspaces = client
        .list_workspaces()
        .map_err(|error| safe_error(&error))?;
    check_cancelled(cancellation, &mut client)?;
    let sessions = client.list_sessions().map_err(|error| safe_error(&error))?;
    Ok((
        MachineSnapshot {
            agents,
            workspaces,
            sessions,
        },
        Connection { client },
    ))
}

pub fn inspect(
    machine: &MachineId,
    cancellation: &Cancellation,
    connection: Option<Connection>,
    permit: WorkerPermit,
) -> Result<(MachineSnapshot, Connection), SafeFleetError> {
    let request_machine = machine.clone();
    let request_cancellation = cancellation.clone();
    run_bounded(cancellation, permit, move || {
        inspect_blocking(&request_machine, &request_cancellation, connection)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_and_session_identity_are_scoped_to_machine() {
        let local = SessionIdentity {
            machine: MachineId::Local,
            session_id: "same-id".into(),
        };
        let remote = SessionIdentity {
            machine: MachineId::Ssh("build".into()),
            session_id: "same-id".into(),
        };
        assert_ne!(local, remote);
    }

    #[test]
    fn cancellation_token_is_shared_and_sticky() {
        let cancellation = Cancellation::default();
        let worker_copy = cancellation.clone();
        assert!(!worker_copy.is_cancelled());
        cancellation.cancel();
        assert!(worker_copy.is_cancelled());
    }

    #[test]
    fn disconnecting_or_failing_drops_the_previous_snapshot() {
        let mut row = MachineRow::new(MachineCatalogEntry {
            id: MachineId::Local,
            kind: MachineKind::Local,
        });
        row.status = ConnectionStatus::Connected;
        row.snapshot = Some(MachineSnapshot {
            agents: Vec::new(),
            workspaces: Vec::new(),
            sessions: Vec::new(),
        });

        row.mark_disconnected();
        assert_eq!(row.status, ConnectionStatus::Disconnected);
        assert!(row.snapshot.is_none());

        row.snapshot = Some(MachineSnapshot {
            agents: Vec::new(),
            workspaces: Vec::new(),
            sessions: Vec::new(),
        });
        row.mark_failed(
            ConnectionStatus::Error,
            SafeFleetError {
                kind: FleetErrorKind::Transport,
                message: "No se pudo conectar.",
            },
        );
        assert_eq!(row.status, ConnectionStatus::Error);
        assert!(row.snapshot.is_none());
        assert!(row.error.is_some());
    }

    #[test]
    fn bounded_operations_time_out_and_honour_cancellation() {
        let workers = FleetWorkers::default();
        let cancellation = Cancellation::default();
        let (finish_tx, finish_rx) = mpsc::channel::<()>();
        let started = Instant::now();
        let result = run_bounded_with_deadline(
            &cancellation,
            Duration::from_millis(50),
            workers.try_acquire().expect("slot available"),
            move || {
                finish_rx.recv().expect("release blocked RPC");
                Ok(())
            },
        );
        assert_eq!(result.unwrap_err().kind, FleetErrorKind::Timeout);
        assert!(started.elapsed() < Duration::from_millis(250));
        // Only one slot can now be acquired: the timed-out RPC is still alive.
        let second = workers.try_acquire().expect("second slot available");
        assert!(workers.try_acquire().is_none());
        drop(second);
        finish_tx.send(()).expect("unblock timed-out RPC");

        let cancellation = Cancellation::default();
        let worker_cancel = cancellation.clone();
        let workers = FleetWorkers::default();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            worker_cancel.cancel();
        });
        let started = Instant::now();
        let result = run_bounded_with_deadline(
            &cancellation,
            Duration::from_secs(5),
            workers.try_acquire().expect("slot available"),
            move || {
                release_rx.recv().expect("release cancelled RPC");
                Ok(())
            },
        );
        assert_eq!(result.unwrap_err().message, "La consulta fue cancelada.");
        assert!(started.elapsed() < Duration::from_millis(350));
        let second = workers.try_acquire().expect("second slot available");
        assert!(workers.try_acquire().is_none());
        drop(second);
        release_tx.send(()).expect("unblock cancelled RPC");
        canceller.join().expect("cancellation thread finished");
    }

    #[test]
    fn timed_out_operations_keep_both_slots_until_real_workers_exit() {
        let workers = FleetWorkers::default();
        let cancellation = Cancellation::default();
        let mut release = Vec::new();
        for _ in 0..2 {
            let permit = workers.try_acquire().expect("slot available");
            let (tx, rx) = mpsc::channel::<()>();
            let result = run_bounded_with_deadline(
                &cancellation,
                Duration::from_millis(30),
                permit,
                move || {
                    rx.recv().expect("release blocked RPC");
                    Ok(())
                },
            );
            assert_eq!(result.unwrap_err().kind, FleetErrorKind::Timeout);
            release.push(tx);
        }

        // Repeated user retries must not create additional SDK threads.
        for _ in 0..100 {
            assert!(workers.try_acquire().is_none(), "timed-out RPC still holds slot");
        }

        for tx in release {
            tx.send(()).expect("release blocked RPC");
        }

        let started = Instant::now();
        loop {
            if let Some(first) = workers.try_acquire() {
                if let Some(second) = workers.try_acquire() {
                    drop((first, second));
                    break;
                }
                drop(first);
            }
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "both permits should return when their real threads exit"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn already_cancelled_requests_never_spawn_or_consume_capacity() {
        let workers = FleetWorkers::default();
        let cancellation = Cancellation::default();
        cancellation.cancel();
        let result = run_bounded_with_deadline(
            &cancellation,
            Duration::from_secs(1),
            workers.try_acquire().unwrap(),
            || -> Result<(), SafeFleetError> {
                panic!("a cancelled request must not invoke the SDK")
            },
        );
        assert_eq!(result.unwrap_err().message, "La consulta fue cancelada.");
        let _first = workers.try_acquire().expect("first slot returned");
        let _second = workers.try_acquire().expect("second slot returned");
        assert!(workers.try_acquire().is_none());
    }

    #[test]
    fn worker_admission_is_bounded_and_permit_releases_on_drop() {
        let workers = FleetWorkers::default();
        let first = workers.try_acquire().unwrap();
        let second = workers.try_acquire().unwrap();
        assert!(workers.try_acquire().is_none());
        drop(first);
        let third = workers.try_acquire().unwrap();
        drop((second, third));
        assert!(workers.try_acquire().is_some());
    }

    #[test]
    fn obsolete_machine_responses_are_rejected_by_generation_and_identity() {
        let first = MachineId::Ssh("first".into());
        let second = MachineId::Ssh("second".into());
        assert!(!response_is_current(8, Some(&second), 7, &first));
        assert!(!response_is_current(7, Some(&second), 7, &first));
        assert!(response_is_current(7, Some(&first), 7, &first));
    }

    #[test]
    fn ssh_errors_are_classified_without_echoing_raw_diagnostics() {
        let error = ClientError::Transport(
            "HostKeyRejected: [private-host] Host key verification failed".into(),
        );
        let safe = safe_error(&error);
        assert_eq!(safe.kind, FleetErrorKind::UntrustedHostKey);
        assert!(!safe.message.contains("private-host"));
    }

    #[test]
    fn known_connection_failures_have_distinct_safe_categories() {
        let cases = [
            (
                "AuthFailed: Permission denied (publickey).",
                FleetErrorKind::SshAuthentication,
            ),
            (
                "BridgeMissing: fleet-ssh-bridge: not found",
                FleetErrorKind::BridgeMissing,
            ),
            (
                "DaemonDown: Connection refused",
                FleetErrorKind::DaemonUnavailable,
            ),
            (
                "incompatible Fleet wire: expected 1, received 2",
                FleetErrorKind::IncompatibleProtocol,
            ),
            (
                "SSHUnreachable: connection refused",
                FleetErrorKind::Transport,
            ),
        ];
        for (diagnostic, expected) in cases {
            let classified = safe_error(&ClientError::Transport(diagnostic.into()));
            assert_eq!(classified.kind, expected, "{diagnostic}");
        }
    }

    #[test]
    fn missing_local_daemon_is_unavailable_and_remote_failure_is_error() {
        let error = SafeFleetError {
            kind: FleetErrorKind::DaemonUnavailable,
            message: "fleetd no está disponible en esta máquina.",
        };
        assert_eq!(
            status_for_error(&MachineId::Local, &error),
            ConnectionStatus::Unavailable
        );
        assert_eq!(
            status_for_error(&MachineId::Ssh("host".into()), &error),
            ConnectionStatus::Error
        );
    }
}

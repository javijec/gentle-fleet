//! Fleet SDK adapter for the GPUI application.
//!
//! This module is the only Desktop boundary to `fleet-client`. It deliberately
//! does not import transports, daemon internals, provider drivers, or storage.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use fleet_client::{
    AgentSummary, ClientError, FleetClient, Host, SessionSummary, WorkspaceSummary,
};

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

/// A tiny bounded admission controller for blocking Fleet SDK work.
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

pub fn inspect(
    machine: &MachineId,
    cancellation: &Cancellation,
    connection: Option<Connection>,
) -> Result<(MachineSnapshot, Connection), SafeFleetError> {
    if cancellation.is_cancelled() {
        return Err(SafeFleetError {
            kind: FleetErrorKind::Other,
            message: "La consulta fue cancelada.",
        });
    }
    let mut client = match connection {
        Some(connection) if connection.client.host() == &machine.sdk_host() => connection.client,
        Some(mut connection) => {
            connection.disconnect();
            FleetClient::connect(machine.sdk_host()).map_err(|error| safe_error(&error))?
        }
        None => FleetClient::connect(machine.sdk_host()).map_err(|error| safe_error(&error))?,
    };
    if cancellation.is_cancelled() {
        client.disconnect();
        return Err(SafeFleetError {
            kind: FleetErrorKind::Other,
            message: "La consulta fue cancelada.",
        });
    }
    let agents = client.list_agents().map_err(|error| safe_error(&error))?;
    if cancellation.is_cancelled() {
        client.disconnect();
        return Err(SafeFleetError {
            kind: FleetErrorKind::Other,
            message: "La consulta fue cancelada.",
        });
    }
    let workspaces = client
        .list_workspaces()
        .map_err(|error| safe_error(&error))?;
    if cancellation.is_cancelled() {
        client.disconnect();
        return Err(SafeFleetError {
            kind: FleetErrorKind::Other,
            message: "La consulta fue cancelada.",
        });
    }
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

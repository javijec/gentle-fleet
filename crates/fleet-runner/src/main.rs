//! fleet-runner — proceso independiente que posee el agente y su spool.

mod spool;

#[cfg(unix)]
use std::io::Write;
use std::io::{self, BufReader, BufWriter};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fleet_agent::ProviderRegistry;
use fleet_domain::{AgentEvent, DomainEvent, RunnerIdentity, RunnerStatus, SessionId};
use fleet_protocol::runner::*;
use fleet_protocol::transport::{runtime_dir, LocalListener, LocalStream};
use fleet_providers::builtin_registry_from_env;

use spool::Spool;

struct Args {
    runner_id: String,
    session_id: SessionId,
    provider_id: String,
    workspace: PathBuf,
}

/// Shared operational state: the daemon endpoint reads snapshots while the
/// provider worker owns the actual AgentDriver.
struct RunnerShared {
    spool: Arc<Spool>,
    pending_approvals: Arc<Mutex<Vec<String>>>,
    provider_alive: Arc<AtomicBool>,
}

fn parse_args() -> Args {
    let mut args = std::env::args().skip(1);
    let mut out = Args {
        runner_id: String::new(),
        session_id: String::new(),
        provider_id: String::new(),
        workspace: PathBuf::new(),
    };
    while let Some(flag) = args.next() {
        let value = args.next().unwrap_or_default();
        match flag.as_str() {
            "--runner-id" => out.runner_id = value,
            "--session" => out.session_id = value,
            "--provider" => out.provider_id = value,
            "--workspace" => out.workspace = PathBuf::from(value),
            _ => {}
        }
    }
    if out.runner_id.is_empty()
        || out.session_id.is_empty()
        || out.provider_id.is_empty()
        || out.workspace.as_os_str().is_empty()
    {
        eprintln!("usage: fleet-runner --runner-id <id> --session <id> --provider <id> --workspace <path>");
        std::process::exit(2);
    }
    out
}

fn build_registry() -> ProviderRegistry {
    builtin_registry_from_env()
}

fn token_file(runner_id: &str) -> PathBuf {
    runtime_dir().join(format!("fleet-runner-{runner_id}.token"))
}

/// Test hook: touching this file simulates a dead native provider without
/// killing the runner process. The probe is immediate so fail-closed does not
/// wait for a blocked driver worker (e.g. an HTTP call with a 30s timeout).
fn provider_dead_marker(runner_id: &str) -> PathBuf {
    runtime_dir().join(format!("fleet-runner-{runner_id}.provider-dead"))
}

fn prepare_token(runner_id: &str) -> io::Result<String> {
    let path = token_file(runner_id);
    // A newly spawned runner must have a fresh secret. Old PID-based tokens
    // were predictable; token continuity is only needed while THIS process lives.
    let token = uuid::Uuid::new_v4().to_string();
    #[cfg(windows)]
    fleet_protocol::transport::write_private_file(&path, token.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true).mode(0o600);
        let mut file = options.open(&path)?;
        file.write_all(token.as_bytes())?;
        drop(file);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(token)
}

#[cfg(windows)]
fn durable_spool_path(
    runner_id: &str,
    runtime: &std::path::Path,
    database: &std::path::Path,
) -> io::Result<PathBuf> {
    // Restrict identifiers to a canonical Windows filename alphabet. In
    // particular, case aliases, path traversal and alternate streams must
    // never cause two logical runner IDs to address the same spool.
    if runner_id.is_empty()
        || runner_id.len() > 128
        || !runner_id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_".contains(&byte))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid Windows runner identifier",
        ));
    }
    let name = format!("fleet-runner-{runner_id}.spool.sqlite");
    let legacy = runtime.join(&name);
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut candidate = legacy.as_os_str().to_os_string();
        candidate.push(suffix);
        match std::fs::symlink_metadata(&candidate) {
            Ok(_) => {
                return Err(io::Error::other(
                    "legacy runner spool exists; explicit migration required",
                ))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    let directory = database
        .parent()
        .ok_or_else(|| io::Error::other("durable database requires a parent directory"))?;
    // Reserve the entire spool filename namespace, including SQLite sidecars.
    if database.file_name().is_some_and(|file| {
        let file = file.to_string_lossy().to_ascii_lowercase();
        file.starts_with("fleet-runner-") || file == "fleetd.lease"
    }) {
        return Err(io::Error::other(
            "daemon database conflicts with reserved runner spool namespace",
        ));
    }
    Ok(directory.join(name))
}

/// A new runner process has a new boot_id. It must never replay a spool
/// belonging to a previous boot, even when the logical runner_id matches.
/// Live reattach connects to the existing process and does not enter here.
/// Caller holds the exclusive spool lease.
#[cfg(windows)]
fn reserve_fresh_spool(path: &std::path::Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("spool requires a parent"))?;
    let directory = fleet_protocol::transport::DataDirectory::open(parent)?;
    directory.validate_sqlite_files(path)?;
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut candidate = path.as_os_str().to_os_string();
        candidate.push(suffix);
        match std::fs::symlink_metadata(&candidate) {
            Ok(_) => {
                return Err(io::Error::other(
                    "previous runner spool exists; explicit recovery required",
                ))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    // Reserve a new regular file without truncating existing data. If
    // startup subsequently fails, retain the reservation for explicit
    // reconciliation rather than allowing an ambiguous automatic retry.
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    directory.validate_private_file(path)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(all(test, windows))]
#[test]
fn fresh_spool_never_adopts_previous_boot_or_sidecar() {
    let root = std::env::temp_dir().join(format!("gf-spool-boot-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let data = root.join("data");
    let path = data.join("fleet-runner-one.spool.sqlite");
    let lease =
        fleet_protocol::transport::DataLease::acquire(&data, "fleet-runner-one.spool.lease")
            .unwrap();
    reserve_fresh_spool(&path).unwrap();
    std::fs::write(&path, b"previous boot").unwrap();
    assert!(reserve_fresh_spool(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"previous boot");
    let other = data.join("fleet-runner-two.spool.sqlite");
    let sidecar = data.join("fleet-runner-two.spool.sqlite-wal");
    std::fs::write(&sidecar, b"unreconciled WAL").unwrap();
    assert!(reserve_fresh_spool(&other).is_err());
    assert!(!other.exists());
    assert_eq!(std::fs::read(&sidecar).unwrap(), b"unreconciled WAL");
    drop(lease);
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(all(test, windows))]
#[test]
fn durable_spool_names_and_legacy_rejection() {
    let root = std::env::temp_dir().join(format!("gf-spool-path-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let runtime = root.join("runtime");
    std::fs::create_dir(&runtime).unwrap();
    let database = root.join("data").join("fleet.sqlite");
    let first = durable_spool_path("runner-one", &runtime, &database).unwrap();
    let second = durable_spool_path("runner-two", &runtime, &database).unwrap();
    assert_ne!(first, second);
    assert_eq!(first.parent(), database.parent());
    for invalid in ["", "Runner-one", "../other", "runner:stream"] {
        assert!(durable_spool_path(invalid, &runtime, &database).is_err());
    }
    let collision = database.with_file_name("FLEET-RUNNER-one.spool.sqlite-wal");
    assert!(durable_spool_path("one", &runtime, &collision).is_err());
    let legacy = runtime.join("fleet-runner-runner-one.spool.sqlite-wal");
    std::fs::write(&legacy, b"preserve").unwrap();
    assert!(durable_spool_path("runner-one", &runtime, &database).is_err());
    assert_eq!(std::fs::read(&legacy).unwrap(), b"preserve");
    std::fs::remove_dir_all(root).unwrap();
}

fn main() -> io::Result<()> {
    let args = parse_args();
    #[cfg(windows)]
    let spool_path = durable_spool_path(
        &args.runner_id,
        &runtime_dir(),
        &fleet_protocol::transport::database_path()?,
    )?;
    // Reserve the spool before publishing a pipe/token or opening SQLite.
    #[cfg(windows)]
    let _spool_lease = fleet_protocol::transport::DataLease::acquire(
        spool_path
            .parent()
            .ok_or_else(|| io::Error::other("spool requires a parent directory"))?,
        &format!("fleet-runner-{}.spool.lease", args.runner_id),
    )?;
    #[cfg(windows)]
    reserve_fresh_spool(&spool_path)?;
    let listener = LocalListener::bind_named(&format!("fleet-runner-{}", args.runner_id))?;
    let token = prepare_token(&args.runner_id)?;
    #[cfg(unix)]
    let spool_path = runtime_dir().join(format!("fleet-runner-{}.spool.sqlite", args.runner_id));
    let spool = Arc::new(Spool::open(&spool_path).map_err(io::Error::other)?);
    let identity = RunnerIdentity {
        runner_id: args.runner_id.clone(),
        session_id: args.session_id.clone(),
        provider_id: args.provider_id.clone(),
        host_id: std::env::var("FLEET_HOST_ID").unwrap_or_else(|_| "local-host".into()),
        boot_id: uuid::Uuid::new_v4().to_string(),
        pid: std::process::id(),
        protocol_version: RUNNER_WIRE.into(),
        generation: 1,
    };
    let (commands, command_rx): (SyncSender<DriverCommand>, _) = mpsc::sync_channel(16);
    let (closed_tx, closed_rx) = mpsc::channel();
    let (startup_tx, startup_rx) = mpsc::channel();
    let provider_alive = Arc::new(AtomicBool::new(false));
    let pending_approvals: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let shared = Arc::new(RunnerShared {
        spool: Arc::clone(&spool),
        pending_approvals: Arc::clone(&pending_approvals),
        provider_alive: Arc::clone(&provider_alive),
    });
    let closed_rx = Arc::new(Mutex::new(closed_rx));
    spawn_driver_loop(
        args,
        build_registry(),
        command_rx,
        Arc::clone(&shared),
        startup_tx,
        closed_tx,
    );
    startup_rx
        .recv_timeout(Duration::from_secs(60))
        .map_err(|error| io::Error::other(format!("driver startup unconfirmed: {error}")))?
        .map_err(io::Error::other)?;

    // A launch receipt must never be visible before the provider opened.
    println!(
        "{}",
        serde_json::to_string(&identity).map_err(io::Error::other)?
    );

    loop {
        match listener.accept() {
            Ok(stream) => {
                if let Err(error) = serve_daemon(
                    stream,
                    token.clone(),
                    identity.clone(),
                    Arc::clone(&shared),
                    commands.clone(),
                    Arc::clone(&closed_rx),
                ) {
                    eprintln!("runner connection ended: {error}");
                }
            }
            Err(error) => eprintln!("runner accept failed: {error}"),
        }
    }
}

fn spawn_driver_loop(
    args: Args,
    registry: ProviderRegistry,
    commands: Receiver<DriverCommand>,
    shared: Arc<RunnerShared>,
    startup: Sender<Result<(), String>>,
    closed_tx: Sender<()>,
) {
    let spool = Arc::clone(&shared.spool);
    let pending_approvals = Arc::clone(&shared.pending_approvals);
    let provider_alive = Arc::clone(&shared.provider_alive);
    std::thread::spawn(move || {
        // Panic or unexpected thread exit must not leave a stale true flag.
        struct AliveGuard(Arc<AtomicBool>);
        impl Drop for AliveGuard {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _guard = AliveGuard(Arc::clone(&provider_alive));
        let mut driver = match registry.create(&args.provider_id) {
            Ok(driver) => driver,
            Err(error) => {
                let _ = startup.send(Err(error.to_string()));
                eprintln!("runner cannot create driver: {error}");
                return;
            }
        };
        if let Err(error) = driver.open(fleet_agent::LaunchContext {
            workspace: args.workspace.clone(),
            mode: fleet_domain::RunnerMode::Auto,
        }) {
            let _ = startup.send(Err(error.to_string()));
            eprintln!("runner driver open failed: {error}");
            return;
        }
        provider_alive.store(true, Ordering::Release);
        let _ = startup.send(Ok(()));
        let mut last_probe = std::time::Instant::now();
        loop {
            // inspect_recovery is called on the worker (never on the IPC
            // thread), so IPC Status remains responsive even for a slow probe.
            if last_probe.elapsed() >= Duration::from_secs(2) {
                provider_alive.store(driver.inspect_recovery().session_alive, Ordering::Release);
                last_probe = std::time::Instant::now();
            }
            match commands.recv_timeout(Duration::from_millis(20)) {
                Ok(DriverCommand::Prompt {
                    text,
                    turn_id,
                    controls,
                }) => {
                    if let Err(error) = driver.send(fleet_agent::AgentInput {
                        text,
                        turn_id,
                        controls,
                    }) {
                        append_with_backpressure(
                            &spool,
                            AgentEvent {
                                kind: DomainEvent::AgentError {
                                    message: error.to_string(),
                                },
                            },
                        );
                    }
                }
                Ok(DriverCommand::Answer {
                    native_request_id,
                    decision,
                    arguments,
                    done,
                    ..
                }) => {
                    let outcome = driver
                        .answer_with_args(native_request_id, decision, arguments)
                        .map_err(|error| error.to_string());
                    if let Err(error) = &outcome {
                        append_with_backpressure(
                            &spool,
                            AgentEvent {
                                kind: DomainEvent::AgentError {
                                    message: error.clone(),
                                },
                            },
                        );
                    }
                    let _ = done.send(outcome);
                }
                Ok(DriverCommand::Interrupt { turn_id }) => {
                    if let Err(error) = driver.interrupt(turn_id) {
                        append_with_backpressure(
                            &spool,
                            AgentEvent {
                                kind: DomainEvent::AgentError {
                                    message: error.to_string(),
                                },
                            },
                        );
                    }
                }
                Ok(DriverCommand::Stop) => {
                    match driver.close() {
                        Ok(()) => {
                            provider_alive.store(false, Ordering::Release);
                            let _ = closed_tx.send(());
                        }
                        Err(error) => {
                            eprintln!("runner driver close not confirmed: {error}");
                        }
                    }
                    break;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            match driver.next_event(Duration::from_millis(50)) {
                Ok(Some(event)) => {
                    if let DomainEvent::ApprovalRequested {
                        native_request_id, ..
                    } = &event.kind
                    {
                        pending_approvals
                            .lock()
                            .expect("approvals poisoned")
                            .push(native_request_id.clone());
                    }
                    if let DomainEvent::ApprovalResolved {
                        native_request_id, ..
                    } = &event.kind
                    {
                        pending_approvals
                            .lock()
                            .expect("approvals poisoned")
                            .retain(|id| id != native_request_id);
                    }
                    append_with_backpressure(&spool, event);
                }
                Ok(None) => {}
                Err(error) => append_with_backpressure(
                    &spool,
                    AgentEvent {
                        kind: DomainEvent::AgentError {
                            message: error.to_string(),
                        },
                    },
                ),
            }
        }
    });
}

fn append_with_backpressure(spool: &Spool, event: AgentEvent) {
    loop {
        match spool.append(&event) {
            Ok(_) => return,
            Err(_error) if _error == "spool_full" => {
                // Emitir evidencia de saturación en cuanto haya hueco reservado.
                let message = format!(
                    "runner spool full ({MAX_SPOOL_EVENTS} events); operations suspended until daemon ACK"
                );
                if let Err(error) = spool.record_notice(&message) {
                    eprintln!("runner could not persist backpressure notice: {error}");
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(_error) => {
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

#[derive(Debug, Clone)]
enum DriverCommand {
    Prompt {
        text: String,
        turn_id: String,
        controls: Vec<fleet_domain::RequestedControl>,
    },
    Answer {
        _approval_id: String,
        native_request_id: String,
        decision: String,
        arguments: Vec<fleet_domain::ApprovalArgument>,
        done: std::sync::mpsc::Sender<Result<(), String>>,
    },
    Interrupt {
        turn_id: String,
    },
    Stop,
}

fn serve_daemon(
    stream: LocalStream,
    token: String,
    identity: RunnerIdentity,
    shared: Arc<RunnerShared>,
    commands: SyncSender<DriverCommand>,
    closed_rx: Arc<Mutex<Receiver<()>>>,
) -> Result<(), String> {
    let spool = &shared.spool;
    let pending_approvals = &shared.pending_approvals;
    let provider_alive = &shared.provider_alive;
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let mut writer = BufWriter::new(stream);
    let mut authenticated = false;

    while let Some(frame) = read_runner_frame(&mut reader).map_err(|e| e.to_string())? {
        let RunnerFrame::Request {
            request_id,
            request,
        } = frame
        else {
            continue;
        };
        if !authenticated {
            match &request {
                RunnerRequest::Hello {
                    identity: hello,
                    token: offered,
                } if hello.runner_id == identity.runner_id
                    && hello.boot_id == identity.boot_id
                    && hello.protocol_version == RUNNER_WIRE
                    && *offered == token => {}
                _ => {
                    write_runner_frame(
                        &mut writer,
                        &RunnerFrame::Response {
                            request_id,
                            result: Err(RunnerError::new(
                                "Unauthorized",
                                "runner identity or token mismatch",
                            )),
                        },
                    )
                    .map_err(|e| e.to_string())?;
                    continue;
                }
            }
            authenticated = true;
        }
        // Un único predicado de salud para Hello y Status: un proveedor muerto
        // no puede presentarse como activo durante el reattachment.
        let healthy = provider_alive.load(Ordering::Acquire)
            && !provider_dead_marker(&identity.runner_id).exists();
        let result: Result<RunnerResponse, String> = match request {
            RunnerRequest::Hello { .. } => Ok(RunnerResponse::Hello {
                identity: identity.clone(),
                status: if healthy {
                    RunnerStatus::Active.as_str()
                } else {
                    RunnerStatus::Uncertain.as_str()
                }
                .into(),
                last_ack_seq: spool.last_ack()?,
                pending_count: spool.pending_count()?,
                protocol_version: RUNNER_WIRE.into(),
            }),
            RunnerRequest::Status => {
                let alive = healthy;
                Ok(RunnerResponse::Status {
                    status: if alive {
                        RunnerStatus::Active
                    } else {
                        RunnerStatus::Uncertain
                    }
                    .as_str()
                    .into(),
                    last_ack_seq: spool.last_ack()?,
                    pending_count: spool.pending_count()?,
                    provider_alive: alive,
                    notices: spool.notices()?,
                })
            }
            RunnerRequest::Pending { since_seq, limit } => Ok(RunnerResponse::Pending {
                events: spool.pending(since_seq, limit.min(MAX_SPOOL_EVENTS))?,
            }),
            RunnerRequest::Ack { runner_seq } => {
                spool.ack(runner_seq)?;
                Ok(RunnerResponse::Status {
                    status: RunnerStatus::Active.as_str().into(),
                    last_ack_seq: spool.last_ack()?,
                    pending_count: spool.pending_count()?,
                    provider_alive: provider_alive.load(Ordering::Acquire),
                    notices: spool.notices()?,
                })
            }
            RunnerRequest::Submit {
                operation_id,
                operation_key,
                command,
            } => {
                let duplicate = spool.reserve_operation(&operation_key, &operation_id)?;
                if !duplicate {
                    let (native, answer_done) = match command {
                        RunnerCommand::Prompt {
                            text,
                            turn_id,
                            operation_key: _,
                            controls,
                        } => (
                            DriverCommand::Prompt {
                                text,
                                turn_id,
                                controls,
                            },
                            None,
                        ),
                        RunnerCommand::Answer {
                            approval_id,
                            native_request_id,
                            decision,
                            operation_key: _,
                            arguments,
                        } => {
                            let (done_tx, done_rx) = std::sync::mpsc::channel();
                            (
                                DriverCommand::Answer {
                                    _approval_id: approval_id,
                                    native_request_id,
                                    decision,
                                    arguments,
                                    done: done_tx,
                                },
                                Some(done_rx),
                            )
                        }
                        RunnerCommand::Interrupt { turn_id } => {
                            (DriverCommand::Interrupt { turn_id }, None)
                        }
                        RunnerCommand::Stop => (DriverCommand::Stop, None),
                    };
                    commands.try_send(native).map_err(|error| match error {
                        TrySendError::Full(_) => {
                            "runner_busy: command queue full (backpressure)".to_string()
                        }
                        TrySendError::Disconnected(_) => "runner_gone".to_string(),
                    })?;
                    if let Some(done) = answer_done {
                        match done.recv_timeout(Duration::from_secs(5)) {
                            Ok(Ok(())) => {}
                            Ok(Err(error)) => {
                                write_runner_frame(
                                    &mut writer,
                                    &RunnerFrame::Notice {
                                        code: "approval_delivery_failed".into(),
                                        message: error,
                                    },
                                )
                                .map_err(|e| e.to_string())?;
                                continue;
                            }
                            Err(_) => {
                                write_runner_frame(
                                    &mut writer,
                                    &RunnerFrame::Notice {
                                        code: "approval_delivery_failed".into(),
                                        message: "timeout".into(),
                                    },
                                )
                                .map_err(|e| e.to_string())?;
                                continue;
                            }
                        }
                    }
                }
                Ok(RunnerResponse::Accepted { operation_id })
            }
            RunnerRequest::QueryApprovals => Ok(RunnerResponse::Approvals {
                pending: pending_approvals
                    .lock()
                    .expect("approvals poisoned")
                    .clone(),
            }),
            RunnerRequest::Shutdown => {
                let _ = commands.try_send(DriverCommand::Stop);
                let received = closed_rx
                    .lock()
                    .expect("closed channel poisoned")
                    .recv_timeout(Duration::from_millis(500));
                match received {
                    Ok(()) => {
                        let response = RunnerResponse::Shutdown {
                            status: RUNNER_SHUTDOWN_STOPPED.into(),
                        };
                        write_runner_frame(
                            &mut writer,
                            &RunnerFrame::Response {
                                request_id,
                                result: Ok(response),
                            },
                        )
                        .map_err(|e| e.to_string())?;
                        std::process::exit(0);
                    }
                    Err(_) => {
                        write_runner_frame(
                            &mut writer,
                            &RunnerFrame::Notice {
                                code: "StopUnconfirmed".into(),
                                message: "driver did not close in time".into(),
                            },
                        )
                        .map_err(|e| e.to_string())?;
                        continue;
                    }
                }
            }
        };
        write_runner_frame(
            &mut writer,
            &RunnerFrame::Response {
                request_id,
                result: result.map_err(|error| RunnerError::new("runner_error", error)),
            },
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

trait StatusStr {
    fn as_str(&self) -> &'static str;
}
impl StatusStr for RunnerStatus {
    fn as_str(&self) -> &'static str {
        match self {
            RunnerStatus::Starting => "starting",
            RunnerStatus::Active => "active",
            RunnerStatus::Disconnected => "disconnected",
            RunnerStatus::Recovering => "recovering",
            RunnerStatus::Stopping => "stopping",
            RunnerStatus::StopUnconfirmed => "stop_unconfirmed",
            RunnerStatus::Stopped => RUNNER_SHUTDOWN_STOPPED,
            RunnerStatus::Lost => "lost",
            RunnerStatus::Uncertain => "uncertain",
        }
    }
}

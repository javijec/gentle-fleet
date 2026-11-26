//! Runner real: lanza y reconecta procesos `fleet-runner` mediante IPC local.

use std::cell::Cell;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Mutex, TryLockError};
use std::time::{Duration, Instant};

use fleet_agent::AgentError;
use fleet_core::runner::{
    RunnerCommand as CoreCommand, RunnerHandle, RunnerLaunchContext, RunnerLauncher,
};
use fleet_domain::{RunnerEvent, RunnerIdentity};
use fleet_protocol::runner::*;
use fleet_protocol::transport::{connect_named, runtime_dir, LocalStream};

struct RunnerIo {
    writer: LocalStream,
    // Preserve BufReader across requests; recreating it discards prefetched
    // frames from the socket and can lose ACKs or terminal events.
    reader: BufReader<LocalStream>,
}

struct ProcessHandle {
    identity: RunnerIdentity,
    io: Mutex<RunnerIo>,
    cancellation: LocalStream,
    buffered: Mutex<VecDeque<RunnerEvent>>,
    next_request: AtomicU64,
    child: Mutex<Option<std::process::Child>>,
    #[cfg(windows)]
    peer_process: Option<fleet_protocol::transport::PeerProcess>,
    last_seq: AtomicU64,
    shutdown_requested: AtomicBool,
}

fn io_error(error: std::io::Error) -> AgentError {
    AgentError {
        code: "runner_io".into(),
        message: error.to_string(),
    }
}

const STARTUP_TIMEOUT: Duration = Duration::from_secs(70);
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const EXIT_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_IDENTITY_BYTES: u64 = 16 * 1024;

fn read_identity(stdout: impl Read) -> Result<RunnerIdentity, AgentError> {
    let mut line = Vec::new();
    BufReader::new(stdout)
        .take(MAX_IDENTITY_BYTES + 1)
        .read_until(b'\n', &mut line)
        .map_err(io_error)?;
    if line.len() as u64 > MAX_IDENTITY_BYTES || !line.ends_with(b"\n") {
        return Err(AgentError {
            code: "runner_identity_invalid".into(),
            message: "runner identity missing, truncated or oversized".into(),
        });
    }
    serde_json::from_slice(&line).map_err(|_| AgentError {
        code: "runner_identity_invalid".into(),
        message: "runner identity is not valid JSON".into(),
    })
}

fn wait_identity(
    receiver: mpsc::Receiver<Result<RunnerIdentity, AgentError>>,
    timeout: Duration,
) -> Result<RunnerIdentity, AgentError> {
    match receiver.recv_timeout(timeout) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(AgentError {
            code: "runner_startup_timeout".into(),
            message: "runner did not announce identity before the startup deadline".into(),
        }),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(AgentError {
            code: "runner_identity_invalid".into(),
            message: "runner identity reader ended without a result".into(),
        }),
    }
}

fn validate_new_identity(
    identity: &RunnerIdentity,
    ctx: &RunnerLaunchContext,
    runner_id: &str,
    pid: u32,
    host_id: &str,
) -> Result<(), AgentError> {
    if identity.runner_id != runner_id
        || identity.session_id != ctx.session_id
        || identity.provider_id != ctx.provider_id
        || identity.host_id != host_id
        || identity.pid != pid
        || identity.boot_id.is_empty()
        || identity.generation != 1
        || identity.protocol_version != RUNNER_WIRE
    {
        return Err(AgentError {
            code: "runner_identity_invalid".into(),
            message: "runner startup identity does not match the owned child or launch context"
                .into(),
        });
    }
    Ok(())
}

/// Only used for an owned child before launch is accepted. Never kill by
/// persisted PID. A stopped runner is not proof of provider/descendant closure.
fn terminate_startup_child(child: &mut std::process::Child) -> std::io::Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    if let Err(error) = child.kill() {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        return Err(error);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "owned runner termination not observed",
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn startup_failure(child: &mut std::process::Child, error: AgentError) -> AgentError {
    match terminate_startup_child(child) {
        Ok(()) => AgentError {
            code: error.code,
            message: format!(
                "{}; owned runner exited; provider effects may remain uncertain",
                fleet_agent::redact_provider_error(&error.message)
            ),
        },
        Err(cleanup) => AgentError {
            code: "runner_startup_unconfirmed".into(),
            message: format!(
                "{}; owned runner termination unconfirmed: {cleanup}",
                fleet_agent::redact_provider_error(&error.message)
            ),
        },
    }
}

impl ProcessHandle {
    fn connect(identity: RunnerIdentity) -> Result<Self, AgentError> {
        let name = format!("fleet-runner-{}", identity.runner_id);
        let stream = connect_named(&name).map_err(io_error)?;
        #[cfg(windows)]
        let peer_process = Some(
            stream
                .capture_server_process(identity.pid)
                .map_err(io_error)?,
        );
        let reader = BufReader::new(stream.try_clone().map_err(io_error)?);
        let cancellation = stream.try_clone().map_err(io_error)?;
        let handle = Self {
            identity,
            io: Mutex::new(RunnerIo {
                writer: stream,
                reader,
            }),
            cancellation,
            buffered: Mutex::new(VecDeque::new()),
            next_request: AtomicU64::new(1),
            child: Mutex::new(None),
            #[cfg(windows)]
            peer_process,
            last_seq: AtomicU64::new(0),
            shutdown_requested: AtomicBool::new(false),
        };
        let token = std::fs::read_to_string(runtime_dir().join(format!("{name}.token")))
            .map_err(io_error)?;
        match handle.rpc_bounded(
            RunnerRequest::Hello {
                identity: handle.identity.clone(),
                token,
            },
            HELLO_TIMEOUT,
        )? {
            RunnerResponse::Hello {
                identity,
                protocol_version,
                last_ack_seq,
                ..
            } if identity == handle.identity && protocol_version == RUNNER_WIRE => {
                handle.last_seq.store(last_ack_seq, Ordering::SeqCst);
            }
            _ => {
                return Err(AgentError {
                    code: "runner_identity_invalid".into(),
                    message: "unexpected runner hello response or identity mismatch".into(),
                })
            }
        }
        // The fresh authenticated response binds the saved boot identity to
        // the live endpoint. Recheck the same retained process after Hello.
        #[cfg(windows)]
        if let Some(process) = &handle.peer_process {
            handle
                .cancellation
                .verify_server_process(process)
                .map_err(io_error)?;
        }
        Ok(handle)
    }

    fn ensure_rpc_open(&self) -> Result<(), AgentError> {
        if self.shutdown_requested.load(Ordering::Acquire) {
            return Err(AgentError {
                code: "runner_stopping".into(),
                message: "runner shutdown requested; new RPC refused".into(),
            });
        }
        Ok(())
    }

    fn rpc(&self, request: RunnerRequest) -> Result<RunnerResponse, AgentError> {
        self.ensure_rpc_open()?;
        let mut io = self.io.lock().expect("runner stream poisoned");
        // A caller that passed the first check before Shutdown must not
        // overtake it after acquiring the mutex.
        self.ensure_rpc_open()?;
        self.rpc_locked(&mut io, request, None, &Cell::new("writing RPC request"))
    }

    // Include contention for the per-runner RPC lock in the deadline. Cancel
    // the actual connection on expiry so a stalled ingress cannot keep the
    // shutdown waiting or allow subsequent commands on an uncertain stream.
    fn rpc_bounded(
        &self,
        request: RunnerRequest,
        timeout: Duration,
    ) -> Result<RunnerResponse, AgentError> {
        if matches!(&request, RunnerRequest::Shutdown) {
            // Stop admitting ordinary RPCs before waiting for the existing
            // one. Do not cancel a healthy in-flight frame or reopen an
            // uncertain connection on a failed Shutdown.
            self.shutdown_requested.store(true, Ordering::Release);
        }
        let deadline = Instant::now() + timeout;
        let timeout_error = |phase: &str| AgentError {
            code: "runner_rpc_timeout".into(),
            message: format!("runner RPC deadline expired while {phase}; connection cancelled"),
        };
        let mut io = loop {
            if Instant::now() >= deadline {
                let _ = self.cancellation.shutdown(std::net::Shutdown::Both);
                return Err(timeout_error("waiting for RPC lock"));
            }
            match self.io.try_lock() {
                Ok(io) => break io,
                Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(5)),
                Err(TryLockError::Poisoned(_)) => {
                    return Err(AgentError {
                        code: "runner_io".into(),
                        message: "runner RPC lock poisoned".into(),
                    })
                }
            }
        };
        let cancellation = self.cancellation.try_clone().map_err(io_error)?;
        let (done_tx, done_rx) = mpsc::channel();
        let watchdog = std::thread::Builder::new()
            .spawn(move || {
                if done_rx
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .is_err()
                {
                    let _ = cancellation.shutdown(std::net::Shutdown::Both);
                    true
                } else {
                    false
                }
            })
            .map_err(io_error)?;
        let phase = Cell::new("writing RPC request");
        let response = self.rpc_locked(&mut io, request, Some(deadline), &phase);
        let _ = done_tx.send(());
        if watchdog.join().unwrap_or(true) || Instant::now() >= deadline {
            let _ = self.cancellation.shutdown(std::net::Shutdown::Both);
            return Err(timeout_error(phase.get()));
        }
        response
    }

    fn rpc_locked(
        &self,
        io: &mut RunnerIo,
        request: RunnerRequest,
        deadline: Option<Instant>,
        phase: &Cell<&'static str>,
    ) -> Result<RunnerResponse, AgentError> {
        let request_id = format!("d{}", self.next_request.fetch_add(1, Ordering::SeqCst));
        write_runner_frame(
            &mut io.writer,
            &RunnerFrame::Request {
                request_id: request_id.clone(),
                request,
            },
        )
        .map_err(io_error)?;
        phase.set("waiting for RPC response");
        loop {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(AgentError {
                    code: "runner_rpc_timeout".into(),
                    message: "runner RPC deadline expired".into(),
                });
            }
            match read_runner_frame(&mut io.reader).map_err(io_error)? {
                Some(RunnerFrame::Response {
                    request_id: id,
                    result,
                }) if id == request_id => {
                    return result.map_err(|error| AgentError {
                        code: error.code,
                        message: error.message,
                    });
                }
                Some(RunnerFrame::Event { runner_seq, event }) => {
                    self.buffered
                        .lock()
                        .expect("buffered poisoned")
                        .push_back(RunnerEvent {
                            runner_id: self.identity.runner_id.clone(),
                            runner_seq,
                            event,
                        });
                }
                Some(RunnerFrame::Notice { code, message }) => {
                    return Err(AgentError { code, message });
                }
                Some(_) => {}
                None => {
                    return Err(AgentError {
                        code: "runner_gone".into(),
                        message: "runner closed the connection".into(),
                    })
                }
            }
        }
    }
}

impl RunnerHandle for ProcessHandle {
    fn identity(&self) -> &RunnerIdentity {
        &self.identity
    }

    fn submit(&self, command: CoreCommand) -> Result<(), AgentError> {
        let native = match command {
            CoreCommand::Prompt {
                text,
                turn_id,
                operation_key,
                controls,
            } => RunnerCommand::Prompt {
                text,
                turn_id,
                operation_key,
                controls,
            },
            CoreCommand::Answer {
                native_request_id,
                decision,
                operation_key,
                arguments,
                ..
            } => RunnerCommand::Answer {
                approval_id: String::new(),
                native_request_id,
                decision,
                operation_key,
                arguments,
            },
            CoreCommand::Interrupt { turn_id } => RunnerCommand::Interrupt { turn_id },
            CoreCommand::Stop => RunnerCommand::Stop,
        };
        let (operation_id, operation_key) = match &native {
            RunnerCommand::Prompt { operation_key, .. }
            | RunnerCommand::Answer { operation_key, .. } => {
                (format!("runner-op-{operation_key}"), operation_key.clone())
            }
            // P4.1-A.2: la interrupción es idempotente por turno; una clave
            // aleatoria permitiría despachar la misma interrupción dos veces.
            RunnerCommand::Interrupt { turn_id } => (
                format!("runner-op-interrupt-{turn_id}"),
                format!("interrupt:{turn_id}"),
            ),
            _ => {
                let operation_id = format!("runner-op-{}", uuid::Uuid::new_v4());
                (
                    operation_id.clone(),
                    format!("{operation_id}:{}", native_key(&native)),
                )
            }
        };
        match self.rpc(RunnerRequest::Submit {
            operation_id,
            operation_key,
            command: native,
        })? {
            RunnerResponse::Accepted { .. } => Ok(()),
            other => Err(AgentError {
                code: "unexpected_runner_response".into(),
                message: format!("{other:?}"),
            }),
        }
    }

    fn recv(&self, _timeout: Duration) -> Result<Option<RunnerEvent>, AgentError> {
        self.ensure_rpc_open()?;
        if let Some(event) = self.buffered.lock().expect("buffered poisoned").pop_front() {
            return Ok(Some(event));
        }
        let since = self.last_seq.load(Ordering::SeqCst);
        match self.rpc(RunnerRequest::Pending {
            since_seq: since,
            limit: 64,
        })? {
            RunnerResponse::Pending { events } => {
                let mut buffered = self.buffered.lock().expect("buffered poisoned");
                buffered.extend(events.into_iter().map(|(runner_seq, event)| RunnerEvent {
                    runner_id: self.identity.runner_id.clone(),
                    runner_seq,
                    event,
                }));
                // FIFO: ACKs are cumulative. Never return seq 4 before 2 and 3.
                Ok(buffered.pop_front())
            }
            other => Err(AgentError {
                code: "unexpected_runner_response".into(),
                message: format!("{other:?}"),
            }),
        }
    }

    fn ack(&self, runner_seq: u64) -> Result<(), AgentError> {
        let acknowledged = self.last_seq.load(Ordering::SeqCst);
        if runner_seq != acknowledged.saturating_add(1) {
            return Err(AgentError {
                code: "runner_sequence_gap".into(),
                message: format!(
                    "refusing cumulative ACK {runner_seq}; last confirmed is {acknowledged}"
                ),
            });
        }
        self.rpc(RunnerRequest::Ack { runner_seq })?;
        self.last_seq.store(runner_seq, Ordering::SeqCst);
        Ok(())
    }

    fn notices(&self) -> Vec<String> {
        match self.rpc(RunnerRequest::Status) {
            Ok(RunnerResponse::Status { notices, .. }) => notices,
            _ => Vec::new(),
        }
    }

    fn provider_alive(&self) -> Result<bool, AgentError> {
        match self.rpc(RunnerRequest::Status)? {
            RunnerResponse::Status {
                provider_alive,
                notices,
                ..
            } => {
                for notice in notices {
                    eprintln!("runner notice: {notice}");
                }
                Ok(provider_alive)
            }
            other => Err(AgentError {
                code: "unexpected_runner_response".into(),
                message: format!("{other:?}"),
            }),
        }
    }

    fn close(&self) -> Result<(), AgentError> {
        let response = self
            .rpc_bounded(RunnerRequest::Shutdown, SHUTDOWN_TIMEOUT)
            .map_err(|error| stop_unconfirmed(&error.to_string()))?;
        confirm_shutdown_response(response)?;
        let mut owned = self.child.lock().expect("child poisoned");
        if let Some(child) = owned.as_mut() {
            // Never force-kill a runner to convert uncertainty into success.
            confirm_child_exit(child, EXIT_TIMEOUT)?;
            *owned = None;
            return Ok(());
        }
        #[cfg(windows)]
        if let Some(process) = &self.peer_process {
            return process
                .wait_success(EXIT_TIMEOUT)
                .map_err(|error| stop_unconfirmed(&error.to_string()));
        }
        Err(stop_unconfirmed(
            "driver close acknowledged but runner exit cannot be verified after reattach",
        ))
    }
}

fn stop_unconfirmed(message: &str) -> AgentError {
    AgentError {
        code: "StopUnconfirmed".into(),
        message: fleet_agent::redact_provider_error(message),
    }
}

fn confirm_shutdown_response(response: RunnerResponse) -> Result<(), AgentError> {
    match response {
        RunnerResponse::Shutdown { status } if status == RUNNER_SHUTDOWN_STOPPED => Ok(()),
        _ => Err(stop_unconfirmed(
            "runner did not acknowledge a confirmed shutdown",
        )),
    }
}

fn confirm_child_exit(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Result<(), AgentError> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) => {
                return Err(stop_unconfirmed(
                    "runner exited unsuccessfully after shutdown",
                ))
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                return Err(stop_unconfirmed(
                    "runner exit not observed after shutdown acknowledgment",
                ))
            }
            Err(error) => {
                return Err(stop_unconfirmed(&format!(
                    "cannot verify runner exit: {error}"
                )))
            }
        }
    }
}

fn native_key(command: &RunnerCommand) -> String {
    match command {
        RunnerCommand::Prompt { turn_id, .. } => format!("prompt:{turn_id}"),
        RunnerCommand::Answer {
            native_request_id,
            decision,
            ..
        } => {
            format!("answer:{native_request_id}:{decision}")
        }
        RunnerCommand::Interrupt { turn_id } => format!("interrupt:{turn_id}"),
        RunnerCommand::Stop => "stop".into(),
    }
}

pub struct ProcessRunnerLauncher;

impl RunnerLauncher for ProcessRunnerLauncher {
    fn launch(&self, ctx: RunnerLaunchContext) -> Result<Box<dyn RunnerHandle>, AgentError> {
        let runner_id = ctx
            .existing
            .as_ref()
            .map(|record| record.identity.runner_id.clone())
            .unwrap_or_else(|| format!("runner-{}", ctx.session_id));
        let name = format!("fleet-runner-{runner_id}");
        if let Some(record) = &ctx.existing {
            return Ok(Box::new(ProcessHandle::connect(record.identity.clone())?));
        }
        {
            let binary = std::env::var_os("FLEET_RUNNER_BIN")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    std::env::current_exe()
                        .ok()
                        .and_then(|path| {
                            path.parent().map(|parent| {
                                parent.join(format!("fleet-runner{}", std::env::consts::EXE_SUFFIX))
                            })
                        })
                        .unwrap_or_else(|| {
                            PathBuf::from(format!("fleet-runner{}", std::env::consts::EXE_SUFFIX))
                        })
                });
            let mut command = std::process::Command::new(binary);
            command
                .arg("--runner-id")
                .arg(&runner_id)
                .arg("--session")
                .arg(&ctx.session_id)
                .arg("--provider")
                .arg(&ctx.provider_id)
                .arg("--workspace")
                .arg(&ctx.workspace)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(
                    std::fs::File::create(runtime_dir().join(format!("{name}.err")))
                        .map(std::process::Stdio::from)
                        .unwrap_or(std::process::Stdio::inherit()),
                )
                .env("FLEET_RUNTIME_DIR", runtime_dir());
            if ctx.ask_shell {
                command.env("FLEET_OPENCODE_ASK_SHELL", "1");
            }
            if ctx.fail_answer {
                command.env("FLEET_FAKE_FAIL_ANSWER", "1");
            }
            let mut child = command.spawn().map_err(io_error)?;
            let result = (|| -> Result<ProcessHandle, AgentError> {
                let stdout = child.stdout.take().ok_or_else(|| AgentError {
                    code: "runner_launch".into(),
                    message: "runner stdout is not piped".into(),
                })?;
                let (sender, receiver) = mpsc::channel();
                // The worker owns only the startup read pipe, not the Child.
                // On timeout the parent terminates the owned runner; it does
                // not wait indefinitely for EOF from inherited descendants.
                std::thread::Builder::new()
                    .spawn(move || {
                        let _ = sender.send(read_identity(stdout));
                    })
                    .map_err(io_error)?;
                let identity = wait_identity(receiver, STARTUP_TIMEOUT)?;
                let host_id =
                    std::env::var("FLEET_HOST_ID").unwrap_or_else(|_| "local-host".into());
                validate_new_identity(&identity, &ctx, &runner_id, child.id(), &host_id)?;
                let handle = ProcessHandle::connect(identity)?;
                if child.try_wait().map_err(io_error)?.is_some() {
                    return Err(AgentError {
                        code: "runner_gone".into(),
                        message: "owned runner exited during startup".into(),
                    });
                }
                Ok(handle)
            })();
            match result {
                Ok(handle) => {
                    *handle.child.lock().expect("child poisoned") = Some(child);
                    Ok(Box::new(handle))
                }
                Err(error) => Err(startup_failure(&mut child, error)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_requires_typed_stopped_response() {
        confirm_shutdown_response(RunnerResponse::Shutdown {
            status: RUNNER_SHUTDOWN_STOPPED.into(),
        })
        .unwrap();
        for response in [
            RunnerResponse::Shutdown {
                status: "running".into(),
            },
            RunnerResponse::Shutdown {
                status: "Stopped".into(),
            },
            RunnerResponse::Accepted {
                operation_id: "wrong-response".into(),
            },
        ] {
            assert_eq!(
                confirm_shutdown_response(response).unwrap_err().code,
                "StopUnconfirmed"
            );
        }
    }

    #[test]
    fn shutdown_exit_confirmation_distinguishes_live_and_failed_child() {
        const CHILD: &str = "GF_TEST_SHUTDOWN_EXIT_CHILD";
        if let Ok(mode) = std::env::var(CHILD) {
            match mode.as_str() {
                "ok" => std::process::exit(0),
                "error" => std::process::exit(1),
                _ => {
                    let mut byte = [0];
                    let _ = std::io::stdin().read(&mut byte);
                    std::process::exit(0);
                }
            }
        }
        struct Guard(std::process::Child);
        impl Drop for Guard {
            fn drop(&mut self) {
                let _ = terminate_startup_child(&mut self.0);
            }
        }
        let spawn = |mode| {
            Guard(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "process_runner::tests::shutdown_exit_confirmation_distinguishes_live_and_failed_child"])
                .env(CHILD, mode)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn().unwrap())
        };
        let mut live = spawn("live");
        assert_eq!(
            confirm_child_exit(&mut live.0, Duration::from_millis(30))
                .unwrap_err()
                .code,
            "StopUnconfirmed"
        );
        assert!(
            live.0.try_wait().unwrap().is_none(),
            "confirmation must not kill the child"
        );
        let mut failed = spawn("error");
        let error = confirm_child_exit(&mut failed.0, Duration::from_secs(5)).unwrap_err();
        assert!(error.message.contains("exited unsuccessfully"));
        let mut ok = spawn("ok");
        confirm_child_exit(&mut ok.0, Duration::from_secs(5)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn shutdown_deadline_includes_rpc_lock_and_silent_peer() {
        fn pair() -> (ProcessHandle, std::os::unix::net::UnixStream) {
            let (stream, peer) = std::os::unix::net::UnixStream::pair().unwrap();
            let stream = LocalStream::Unix(stream);
            let cancellation = stream.try_clone().unwrap();
            let reader = BufReader::new(stream.try_clone().unwrap());
            (
                ProcessHandle {
                    identity: RunnerIdentity {
                        runner_id: "runner-test".into(),
                        session_id: "ses-test".into(),
                        provider_id: "fake".into(),
                        host_id: "local-host".into(),
                        boot_id: "boot".into(),
                        pid: 1,
                        protocol_version: RUNNER_WIRE.into(),
                        generation: 1,
                    },
                    io: Mutex::new(RunnerIo {
                        writer: stream,
                        reader,
                    }),
                    cancellation,
                    buffered: Mutex::new(VecDeque::new()),
                    next_request: AtomicU64::new(1),
                    child: Mutex::new(None),
                    #[cfg(windows)]
                    peer_process: None,
                    last_seq: AtomicU64::new(0),
                    shutdown_requested: AtomicBool::new(false),
                },
                peer,
            )
        }
        let (handle, _peer) = pair();
        let held = handle.io.lock().unwrap();
        let error = handle
            .rpc_bounded(RunnerRequest::Shutdown, Duration::from_millis(50))
            .unwrap_err();
        assert_eq!(error.code, "runner_rpc_timeout");
        assert!(error.message.contains("waiting for RPC lock"));
        assert_eq!(
            handle.rpc(RunnerRequest::Status).unwrap_err().code,
            "runner_stopping"
        );
        drop(held);

        let (handle, _peer) = pair();
        let error = handle
            .rpc_bounded(RunnerRequest::Shutdown, Duration::from_millis(50))
            .unwrap_err();
        assert_eq!(error.code, "runner_rpc_timeout");
        assert!(error.message.contains("waiting for RPC response"));
    }

    #[cfg(windows)]
    #[test]
    fn shutdown_deadline_cancels_real_named_pipe_peer() {
        const CHILD: &str = "GF_TEST_NAMED_PIPE_SHUTDOWN_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let runtime =
                std::env::temp_dir().join(format!("fleet-shutdown-test-{}", uuid::Uuid::new_v4()));
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "process_runner::tests::shutdown_deadline_cancels_real_named_pipe_peer",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("FLEET_RUNTIME_DIR", &runtime)
                .spawn()
                .unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(15);
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(status.success(), "named pipe fixture failed: {status}");
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("named pipe fixture exceeded deadline");
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = std::fs::remove_dir_all(runtime);
            return;
        }

        use fleet_protocol::transport::LocalListener;
        let listener = LocalListener::bind_named("shutdown-test").unwrap();
        let (seen_tx, seen_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let stream = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let frame = read_runner_frame(&mut reader).unwrap().unwrap();
            assert!(matches!(
                frame,
                RunnerFrame::Request {
                    request: RunnerRequest::Shutdown,
                    ..
                }
            ));
            seen_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        let stream = connect_named("shutdown-test").unwrap();
        let cancellation = stream.try_clone().unwrap();
        let reader = BufReader::new(stream.try_clone().unwrap());
        let handle = ProcessHandle {
            identity: RunnerIdentity {
                runner_id: "runner-test".into(),
                session_id: "ses-test".into(),
                provider_id: "fake".into(),
                host_id: "local-host".into(),
                boot_id: "boot".into(),
                pid: 1,
                protocol_version: RUNNER_WIRE.into(),
                generation: 1,
            },
            io: Mutex::new(RunnerIo {
                writer: stream,
                reader,
            }),
            cancellation,
            buffered: Mutex::new(VecDeque::new()),
            next_request: AtomicU64::new(1),
            child: Mutex::new(None),
            #[cfg(windows)]
            peer_process: None,
            last_seq: AtomicU64::new(0),
            shutdown_requested: AtomicBool::new(false),
        };
        let handle = std::sync::Arc::new(handle);
        let held = handle.io.lock().unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        let worker_handle = std::sync::Arc::clone(&handle);
        let worker = std::thread::spawn(move || {
            let _ = done_tx
                .send(worker_handle.rpc_bounded(RunnerRequest::Shutdown, Duration::from_secs(2)));
        });
        let deadline = Instant::now() + Duration::from_secs(1);
        while !handle.shutdown_requested.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "Shutdown was not announced");
            std::thread::yield_now();
        }
        // The old RPC still owns the mutex. A poll must reject immediately,
        // not queue behind it or send another frame ahead of Shutdown.
        assert_eq!(
            handle.rpc(RunnerRequest::Status).unwrap_err().code,
            "runner_stopping"
        );
        drop(held);
        seen_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("peer did not receive Shutdown");
        let result = done_rx
            .recv_timeout(Duration::from_secs(4))
            .expect("Shutdown cancellation hung");
        let error = result.unwrap_err();
        assert_eq!(error.code, "runner_rpc_timeout");
        assert!(error.message.contains("waiting for RPC response"));
        release_tx.send(()).unwrap();
        worker.join().unwrap();
        server.join().unwrap();
    }

    #[test]
    fn hello_rejects_wrong_identity_on_native_transport() {
        const CHILD: &str = "GF_TEST_WRONG_HELLO_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let runtime = std::env::temp_dir().join(format!("gfh-{}", uuid::Uuid::new_v4()));
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "process_runner::tests::hello_rejects_wrong_identity_on_native_transport",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("FLEET_RUNTIME_DIR", &runtime)
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    let _ = std::fs::remove_dir_all(&runtime);
                    assert!(status.success(), "native Hello fixture failed: {status}");
                    return;
                }
                if Instant::now() >= deadline {
                    terminate_startup_child(&mut child).unwrap();
                    let _ = std::fs::remove_dir_all(&runtime);
                    panic!("native Hello fixture exceeded deadline");
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        use fleet_protocol::transport::LocalListener;
        let identity = RunnerIdentity {
            runner_id: "hello-test".into(),
            session_id: "ses-test".into(),
            provider_id: "fake".into(),
            host_id: "local-host".into(),
            boot_id: "expected-boot".into(),
            pid: std::process::id(),
            protocol_version: RUNNER_WIRE.into(),
            generation: 1,
        };
        let listener = LocalListener::bind_named("fleet-runner-hello-test").unwrap();
        let token = uuid::Uuid::new_v4().to_string();
        let path = runtime_dir().join("fleet-runner-hello-test.token");
        #[cfg(windows)]
        fleet_protocol::transport::write_private_file(&path, token.as_bytes()).unwrap();
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .unwrap()
                .write_all(token.as_bytes())
                .unwrap();
        }
        let expected = identity.clone();
        let (release_tx, release_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut stream = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let frame = read_runner_frame(&mut reader).unwrap().unwrap();
            let RunnerFrame::Request {
                request_id,
                request:
                    RunnerRequest::Hello {
                        identity: offered,
                        token: offered_token,
                    },
            } = frame
            else {
                panic!("expected authenticated Hello");
            };
            assert_eq!(offered, expected);
            assert_eq!(offered_token, token);
            let mut wrong = expected;
            wrong.boot_id = "wrong-boot".into();
            write_runner_frame(
                &mut stream,
                &RunnerFrame::Response {
                    request_id,
                    result: Ok(RunnerResponse::Hello {
                        identity: wrong,
                        status: "active".into(),
                        last_ack_seq: 0,
                        pending_count: 0,
                        protocol_version: RUNNER_WIRE.into(),
                    }),
                },
            )
            .unwrap();
            release_rx.recv_timeout(Duration::from_secs(8)).unwrap();
        });
        let error = ProcessHandle::connect(identity)
            .err()
            .expect("wrong Hello identity was accepted");
        assert_eq!(error.code, "runner_identity_invalid");
        release_tx.send(()).unwrap();
        server.join().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn owned_runner_spawn_does_not_inherit_unrelated_private_file() {
        use std::os::windows::fs::OpenOptionsExt;

        const CHILD: &str = "GF_TEST_PRIVATE_HANDLE_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let mut byte = [0];
            let _ = std::io::stdin().read(&mut byte);
            return;
        }
        let path = std::env::temp_dir().join(format!("fleet-handle-{}", uuid::Uuid::new_v4()));
        let open_exclusive = || {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(0)
                .open(&path)
        };
        let private = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        assert!(open_exclusive().is_err(), "exclusive-handle control failed");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "process_runner::tests::owned_runner_spawn_does_not_inherit_unrelated_private_file",
            ])
            .env(CHILD, "1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        drop(private);
        // Reopening succeeds only after every exclusive handle to the file
        // has closed. Keep the child alive with its stdin pipe for the probe.
        let child_alive = child.try_wait().unwrap().is_none();
        let reopened = open_exclusive();
        terminate_startup_child(&mut child).unwrap();
        let exclusive_released = reopened.is_ok();
        drop(reopened);
        std::fs::remove_file(path).unwrap();
        assert!(child_alive, "child exited before handle inheritance probe");
        assert!(
            exclusive_released,
            "child retained an unrelated private handle"
        );
    }

    #[test]
    fn startup_identity_requires_complete_bounded_frame() {
        for bytes in [
            b"".to_vec(),
            b"{}".to_vec(),
            b"invalid\n".to_vec(),
            vec![b'x'; MAX_IDENTITY_BYTES as usize + 1],
        ] {
            assert_eq!(
                read_identity(bytes.as_slice()).unwrap_err().code,
                "runner_identity_invalid"
            );
        }
        let identity = RunnerIdentity {
            runner_id: "runner-ses_1".into(),
            session_id: "ses_1".into(),
            provider_id: "fake".into(),
            host_id: "local-host".into(),
            boot_id: "boot".into(),
            pid: 42,
            protocol_version: RUNNER_WIRE.into(),
            generation: 1,
        };
        let mut bytes = serde_json::to_vec(&identity).unwrap();
        bytes.push(b'\n');
        assert_eq!(read_identity(bytes.as_slice()).unwrap(), identity);
        let ctx = RunnerLaunchContext {
            session_id: "ses_1".into(),
            provider_id: "fake".into(),
            workspace: PathBuf::new(),
            mode: fleet_domain::RunnerMode::Auto,
            existing: None,
            ask_shell: false,
            fail_answer: false,
        };
        validate_new_identity(&identity, &ctx, "runner-ses_1", 42, "local-host").unwrap();
        assert!(validate_new_identity(&identity, &ctx, "runner-ses_1", 43, "local-host").is_err());
        assert!(validate_new_identity(&identity, &ctx, "runner-other", 42, "local-host").is_err());
        assert!(validate_new_identity(&identity, &ctx, "runner-ses_1", 42, "other-host").is_err());
    }

    #[test]
    fn silent_startup_identity_is_bounded() {
        let (_sender, receiver) = mpsc::channel();
        let error = wait_identity(receiver, Duration::from_millis(20)).unwrap_err();
        assert_eq!(error.code, "runner_startup_timeout");
    }

    #[test]
    fn startup_cleanup_terminates_only_owned_child() {
        const CHILD: &str = "GF_TEST_STARTUP_CLEANUP_CHILD";
        if std::env::var_os(CHILD).is_some() {
            // Parent owns stdin and keeps the pipe open until cleanup.
            let mut byte = [0];
            let _ = std::io::stdin().read(&mut byte);
            return;
        }
        struct Guard(std::process::Child);
        impl Drop for Guard {
            fn drop(&mut self) {
                let _ = terminate_startup_child(&mut self.0);
            }
        }
        let spawn = || {
            Guard(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "process_runner::tests::startup_cleanup_terminates_only_owned_child",
                    ])
                    .env(CHILD, "1")
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .unwrap(),
            )
        };
        let mut owned = spawn();
        let mut sentinel = spawn();
        let failure = startup_failure(
            &mut owned.0,
            AgentError {
                code: "runner_startup_timeout".into(),
                message: "fixture".into(),
            },
        );
        assert_eq!(failure.code, "runner_startup_timeout");
        assert!(owned.0.try_wait().unwrap().is_some());
        assert!(sentinel.0.try_wait().unwrap().is_none());
        terminate_startup_child(&mut sentinel.0).unwrap();
    }
}

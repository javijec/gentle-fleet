//! Runner real: lanza y reconecta procesos `fleet-runner` mediante IPC local.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Mutex};
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
    buffered: Mutex<VecDeque<RunnerEvent>>,
    next_request: AtomicU64,
    child: Mutex<Option<std::process::Child>>,
    last_seq: AtomicU64,
}

fn io_error(error: std::io::Error) -> AgentError {
    AgentError {
        code: "runner_io".into(),
        message: error.to_string(),
    }
}

const STARTUP_TIMEOUT: Duration = Duration::from_secs(70);
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
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
            message: "runner startup identity does not match the owned child or launch context".into(),
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
        let reader = BufReader::new(stream.try_clone().map_err(io_error)?);
        let handle = Self {
            identity,
            io: Mutex::new(RunnerIo {
                writer: stream,
                reader,
            }),
            buffered: Mutex::new(VecDeque::new()),
            next_request: AtomicU64::new(1),
            child: Mutex::new(None),
            last_seq: AtomicU64::new(0),
        };
        let token = std::fs::read_to_string(runtime_dir().join(format!("{name}.token")))
            .map_err(io_error)?;
        // Cancel a stalled Hello on the actual connection, not by abandoning
        // an RPC thread. The clone shares the Windows cancellation state.
        let cancellation = handle.io.lock().expect("runner stream poisoned")
            .writer.try_clone().map_err(io_error)?;
        let (done_tx, done_rx) = mpsc::channel();
        let watchdog = std::thread::Builder::new().spawn(move || {
            if done_rx.recv_timeout(HELLO_TIMEOUT).is_err() {
                let _ = cancellation.shutdown(std::net::Shutdown::Both);
                true
            } else {
                false
            }
        }).map_err(io_error)?;
        let response = handle.rpc(RunnerRequest::Hello {
            identity: handle.identity.clone(),
            token,
        });
        let _ = done_tx.send(());
        if watchdog.join().unwrap_or(true) {
            return Err(AgentError {
                code: "runner_hello_timeout".into(),
                message: "runner Hello did not finish before the deadline".into(),
            });
        }
        match response? {
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
        Ok(handle)
    }

    fn rpc(&self, request: RunnerRequest) -> Result<RunnerResponse, AgentError> {
        let request_id = format!("d{}", self.next_request.fetch_add(1, Ordering::SeqCst));
        let mut io = self.io.lock().expect("runner stream poisoned");
        write_runner_frame(
            &mut io.writer,
            &RunnerFrame::Request {
                request_id: request_id.clone(),
                request,
            },
        )
        .map_err(io_error)?;
        loop {
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
        self.rpc(RunnerRequest::Shutdown)?;
        if let Some(mut child) = self.child.lock().expect("child poisoned").take() {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while std::time::Instant::now() < deadline {
                match child.try_wait() {
                    Ok(Some(_)) => return Ok(()),
                    Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                    Err(_) => break,
                }
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        Ok(())
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
                        .and_then(|path| path.parent().map(|parent| parent.join(format!("fleet-runner{}", std::env::consts::EXE_SUFFIX))))
                        .unwrap_or_else(|| PathBuf::from(format!("fleet-runner{}", std::env::consts::EXE_SUFFIX)))
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
                std::thread::Builder::new().spawn(move || {
                    let _ = sender.send(read_identity(stdout));
                }).map_err(io_error)?;
                let identity = wait_identity(receiver, STARTUP_TIMEOUT)?;
                let host_id = std::env::var("FLEET_HOST_ID").unwrap_or_else(|_| "local-host".into());
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
    fn startup_identity_requires_complete_bounded_frame() {
        for bytes in [b"".to_vec(), b"{}".to_vec(), b"invalid\n".to_vec(), vec![b'x'; MAX_IDENTITY_BYTES as usize + 1]] {
            assert_eq!(read_identity(bytes.as_slice()).unwrap_err().code, "runner_identity_invalid");
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
            Guard(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "process_runner::tests::startup_cleanup_terminates_only_owned_child"])
                .env(CHILD, "1")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap())
        };
        let mut owned = spawn();
        let mut sentinel = spawn();
        let failure = startup_failure(&mut owned.0, AgentError {
            code: "runner_startup_timeout".into(),
            message: "fixture".into(),
        });
        assert_eq!(failure.code, "runner_startup_timeout");
        assert!(owned.0.try_wait().unwrap().is_some());
        assert!(sentinel.0.try_wait().unwrap().is_none());
        terminate_startup_child(&mut sentinel.0).unwrap();
    }
}

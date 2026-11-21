//! Runner real: lanza y reconecta procesos `fleet-runner` mediante IPC local.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

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

impl ProcessHandle {
    fn connect(
        identity: RunnerIdentity,
        child: Option<std::process::Child>,
    ) -> Result<Self, AgentError> {
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
            child: Mutex::new(child),
            last_seq: AtomicU64::new(0),
        };
        let token = std::fs::read_to_string(runtime_dir().join(format!("{name}.token")))
            .map_err(io_error)?;
        match handle.rpc(RunnerRequest::Hello {
            identity: handle.identity.clone(),
            token,
        })? {
            RunnerResponse::Hello {
                identity,
                protocol_version,
                last_ack_seq,
                ..
            } if identity == handle.identity && protocol_version == RUNNER_WIRE => {
                handle.last_seq.store(last_ack_seq, Ordering::SeqCst);
            }
            other => {
                return Err(AgentError {
                    code: "runner_identity_invalid".into(),
                    message: format!("unexpected runner hello response: {other:?}"),
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
            CoreCommand::Prompt { text, turn_id } => RunnerCommand::Prompt { text, turn_id },
            CoreCommand::Answer {
                native_request_id,
                decision,
                ..
            } => RunnerCommand::Answer {
                approval_id: String::new(),
                native_request_id,
                decision,
            },
            CoreCommand::Interrupt { turn_id } => RunnerCommand::Interrupt { turn_id },
            CoreCommand::Stop => RunnerCommand::Stop,
        };
        let operation_id = format!("runner-op-{}", uuid::Uuid::new_v4());
        let operation_key = format!("{}:{}", operation_id, native_key(&native));
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

    fn provider_alive(&self) -> Result<bool, AgentError> {
        match self.rpc(RunnerRequest::Status)? {
            RunnerResponse::Status { provider_alive, .. } => Ok(provider_alive),
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
        let (identity, child) = if let Some(record) = &ctx.existing {
            (record.identity.clone(), None)
        } else {
            let binary = std::env::var("FLEET_RUNNER_BIN")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    std::env::current_exe()
                        .ok()
                        .and_then(|path| path.parent().map(|parent| parent.join("fleet-runner")))
                        .unwrap_or_else(|| PathBuf::from("fleet-runner"))
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
            let stdout = child.stdout.take().ok_or_else(|| AgentError {
                code: "runner_launch".into(),
                message: "runner stdout is not piped".into(),
            })?;
            let mut line = String::new();
            BufReader::new(stdout)
                .read_line(&mut line)
                .map_err(io_error)?;
            let identity: RunnerIdentity =
                serde_json::from_str(line.trim()).map_err(|error| AgentError {
                    code: "runner_identity_invalid".into(),
                    message: error.to_string(),
                })?;
            (identity, Some(child))
        };
        Ok(Box::new(ProcessHandle::connect(identity, child)?))
    }
}

//! Fleet Daemon P1: API JSONL en loopback, SessionManager y registro de drivers.

use std::io::{BufReader, BufWriter};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::Arc;
use std::time::Duration;

use fleet_agent::fake::FakeFactory;
use fleet_agent::ProviderRegistry;
use fleet_core::SessionManager;
use fleet_driver_opencode::OpenCodeFactory;
use fleet_protocol::{read_frame, write_frame, AgentSummary, Frame, Request, Response, WIRE};

fn runtime_dir() -> PathBuf {
    std::env::var_os("FLEET_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("gentle-fleet"))
}

fn address_file() -> PathBuf {
    runtime_dir().join("fleetd.addr")
}

fn main() -> std::io::Result<()> {
    let mut registry = ProviderRegistry::new();
    registry.register(Box::new(FakeFactory));
    registry.register(Box::new(OpenCodeFactory));
    let manager = Arc::new(SessionManager::new(registry));

    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let address = listener.local_addr()?;
    std::fs::create_dir_all(runtime_dir())?;
    std::fs::write(address_file(), address.to_string())?;
    println!("fleetd listening on {address} ({WIRE})");

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let manager = Arc::clone(&manager);
                std::thread::spawn(move || {
                    if let Err(error) = handle_client(stream, manager) {
                        eprintln!("client connection ended: {error}");
                    }
                });
            }
            Err(error) => eprintln!("accept failed: {error}"),
        }
    }
    Ok(())
}

fn handle_client(stream: TcpStream, manager: Arc<SessionManager>) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let writer = Arc::new(std::sync::Mutex::new(BufWriter::new(stream)));
    let last_seq = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let mut subscription: Option<u64> = None;

    while let Some(frame) = read_frame(&mut reader)? {
        let Frame::Request {
            request_id,
            request,
        } = frame
        else {
            continue;
        };
        match request {
            Request::Hello { client_id } => {
                let response = Response::Hello {
                    server_id: format!("fleetd-{}", std::process::id()),
                    wire: WIRE.into(),
                };
                let _ = client_id;
                write_frame(
                    &mut *writer.lock().expect("writer poisoned"),
                    &Frame::Response {
                        request_id,
                        result: Ok(response),
                    },
                )?;
            }
            Request::RegisterWorkspace { path } => {
                let result = manager
                    .register_workspace(&path)
                    .map(|workspace| Response::WorkspaceRegistered {
                        workspace: fleet_protocol::WorkspaceSummary {
                            id: workspace.id,
                            path: workspace.path.to_string_lossy().into_owned(),
                        },
                    })
                    .map_err(to_api_error);
                write_frame(
                    &mut *writer.lock().expect("writer poisoned"),
                    &Frame::Response { request_id, result },
                )?;
            }
            Request::ListWorkspaces => {
                let workspaces = manager
                    .list_workspaces()
                    .into_iter()
                    .map(|workspace| fleet_protocol::WorkspaceSummary {
                        id: workspace.id,
                        path: workspace.path.to_string_lossy().into_owned(),
                    })
                    .collect();
                write_frame(
                    &mut *writer.lock().expect("writer poisoned"),
                    &Frame::Response {
                        request_id,
                        result: Ok(Response::Workspaces { workspaces }),
                    },
                )?;
            }
            Request::ListAgents => {
                let agents = manager
                    .registered_providers()
                    .into_iter()
                    .map(|id| AgentSummary {
                        id: id.clone(),
                        version: "per-version".into(),
                        mode: "auto".into(),
                        capabilities: Default::default(),
                    })
                    .collect();
                write_frame(
                    &mut *writer.lock().expect("writer poisoned"),
                    &Frame::Response {
                        request_id,
                        result: Ok(Response::Agents { agents }),
                    },
                )?;
            }
            Request::CreateSession {
                workspace_id,
                provider_id,
            } => {
                let result = manager
                    .create_session(&workspace_id, &provider_id)
                    .map(|session| Response::SessionCreated {
                        session: fleet_protocol::SessionSummary {
                            id: session.id,
                            workspace_id: session.workspace_id,
                            provider_id: session.provider_id,
                            state: format!("{:?}", session.state),
                        },
                    })
                    .map_err(to_api_error);
                write_frame(
                    &mut *writer.lock().expect("writer poisoned"),
                    &Frame::Response { request_id, result },
                )?;
            }
            Request::ListSessions => {
                let sessions = manager
                    .list_sessions()
                    .into_iter()
                    .map(|session| fleet_protocol::SessionSummary {
                        id: session.id,
                        workspace_id: session.workspace_id,
                        provider_id: session.provider_id,
                        state: format!("{:?}", session.state),
                    })
                    .collect();
                write_frame(
                    &mut *writer.lock().expect("writer poisoned"),
                    &Frame::Response {
                        request_id,
                        result: Ok(Response::Sessions { sessions }),
                    },
                )?;
            }
            Request::SendPrompt {
                session_id,
                text,
                operation_key,
            } => {
                let result = manager
                    .send_prompt(&session_id, text, &operation_key)
                    .map(|receipt| Response::PromptAccepted {
                        operation_id: receipt.operation_id,
                        session_id: receipt.session_id,
                        turn_id: receipt.turn_id,
                    })
                    .map_err(to_api_error);
                write_frame(
                    &mut *writer.lock().expect("writer poisoned"),
                    &Frame::Response { request_id, result },
                )?;
            }
            Request::SubscribeTimeline {
                session_id,
                since_seq,
            } => match manager.subscribe(&session_id, since_seq) {
                Ok((id, snapshot, rx)) => {
                    let latest = snapshot.last().map(|event| event.seq).unwrap_or(since_seq);
                    last_seq.store(latest, std::sync::atomic::Ordering::SeqCst);
                    write_frame(
                        &mut *writer.lock().expect("writer poisoned"),
                        &Frame::Response {
                            request_id: request_id.clone(),
                            result: Ok(Response::Subscribed {
                                session_id: session_id.clone(),
                                latest_seq: latest,
                            }),
                        },
                    )?;
                    for event in snapshot {
                        last_seq.store(event.seq, std::sync::atomic::Ordering::SeqCst);
                        write_event(&mut writer.lock().expect("writer poisoned"), event)?;
                    }
                    if let Some(old_id) = subscription.take() {
                        manager.bus().unsubscribe(old_id);
                    }
                    subscription = Some(id);
                    let writer = Arc::clone(&writer);
                    let last_seq = Arc::clone(&last_seq);
                    std::thread::spawn(move || loop {
                        match rx.recv_timeout(Duration::from_millis(50)) {
                            Ok(event) => {
                                if event.seq <= last_seq.load(std::sync::atomic::Ordering::SeqCst) {
                                    continue;
                                }
                                last_seq.store(event.seq, std::sync::atomic::Ordering::SeqCst);
                                let session_id = event.session_id.clone();
                                let seq = event.seq;
                                let frame = Frame::Event {
                                    session_id,
                                    seq,
                                    event,
                                };
                                let mut guard = writer.lock().expect("writer poisoned");
                                if write_frame(&mut *guard, &frame).is_err() {
                                    break;
                                }
                            }
                            Err(RecvTimeoutError::Timeout) => continue,
                            Err(RecvTimeoutError::Disconnected) => break,
                        }
                    });
                }
                Err(error) => write_frame(
                    &mut *writer.lock().expect("writer poisoned"),
                    &Frame::Response {
                        request_id,
                        result: Err(to_api_error(error)),
                    },
                )?,
            },
            Request::StopSession { session_id } => {
                let result = manager
                    .stop_session(&session_id)
                    .map(|_| Response::Stopped { session_id })
                    .map_err(to_api_error);
                write_frame(
                    &mut *writer.lock().expect("writer poisoned"),
                    &Frame::Response { request_id, result },
                )?;
            }
        }
    }

    if let Some(id) = subscription {
        manager.bus().unsubscribe(id);
    }
    Ok(())
}

fn write_event(
    writer: &mut BufWriter<TcpStream>,
    event: fleet_domain::Event,
) -> std::io::Result<()> {
    let session_id = event.session_id.clone();
    let seq = event.seq;
    write_frame(
        writer,
        &Frame::Event {
            session_id,
            seq,
            event,
        },
    )
}

fn to_api_error(error: fleet_agent::AgentError) -> fleet_protocol::ApiError {
    fleet_protocol::ApiError {
        code: error.code,
        message: error.message,
    }
}

//! Fleet Daemon P1.1: API JSONL, SessionManager y transporte local por usuario.

use std::io::{BufReader, BufWriter};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fleet_agent::fake::{FakeConfig, FakeFactory};
use fleet_agent::ProviderRegistry;
use fleet_core::{SessionManager, Subscription, SubscriptionHandle, SUBSCRIBER_LIMIT};
use fleet_driver_opencode::OpenCodeFactory;
use fleet_protocol::transport::{expected_token, LocalListener, LocalStream};
use fleet_protocol::{read_frame, write_frame, AgentSummary, Frame, Request, Response, WIRE};

fn to_api_error(error: fleet_agent::AgentError) -> fleet_protocol::ApiError {
    fleet_protocol::ApiError {
        code: error.code,
        message: error.message,
    }
}

fn main() -> std::io::Result<()> {
    let mut registry = ProviderRegistry::new();
    let fake_config = FakeConfig {
        block_send: std::env::var("FLEET_FAKE_BLOCK_SEND").is_ok(),
        block_close: std::env::var("FLEET_FAKE_BLOCK_CLOSE").is_ok(),
        fail_send: std::env::var("FLEET_FAKE_FAIL_SEND").is_ok(),
        emit_provider_error: std::env::var("FLEET_FAKE_PROVIDER_ERROR").is_ok(),
        emit_terminal_failure: std::env::var("FLEET_FAKE_TERMINAL_FAILURE").is_ok(),
    };
    registry.register(Box::new(FakeFactory::with_config(fake_config)));
    registry.register(Box::new(OpenCodeFactory));
    let manager = Arc::new(SessionManager::new(registry));
    // bind() crea el token de Windows: no leerlo antes.
    let listener = LocalListener::bind()?;
    let expected = expected_token()?;
    println!("fleetd listening on {} ({WIRE})", listener.description());

    loop {
        match listener.accept() {
            Ok(stream) => {
                let manager = Arc::clone(&manager);
                let expected = expected.clone();
                std::thread::spawn(move || {
                    if let Err(error) = handle_client(stream, manager, expected) {
                        eprintln!("client connection ended: {error}");
                    }
                });
            }
            Err(error) => eprintln!("accept failed: {error}"),
        }
    }
}

fn handle_client(
    stream: LocalStream,
    manager: Arc<SessionManager>,
    expected_token: Option<String>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let writer = Arc::new(Mutex::new(BufWriter::new(stream)));
    let mut subscription: Option<SubscriptionHandle> = None;
    let mut authenticated = false;

    while let Some(frame) = read_frame(&mut reader)? {
        let Frame::Request {
            request_id,
            request,
        } = frame
        else {
            continue;
        };
        if !authenticated {
            let Request::Hello { token, .. } = &request else {
                write_frame(
                    &mut *writer.lock().expect("writer poisoned"),
                    &Frame::Response {
                        request_id,
                        result: Err(to_api_error(fleet_agent::AgentError {
                            code: "Unauthorized".into(),
                            message: "Hello is required before any other request".into(),
                        })),
                    },
                )?;
                continue;
            };
            if expected_token.is_some() && token != &expected_token {
                write_frame(
                    &mut *writer.lock().expect("writer poisoned"),
                    &Frame::Response {
                        request_id,
                        result: Err(to_api_error(fleet_agent::AgentError {
                            code: "Unauthorized".into(),
                            message: "local token does not match".into(),
                        })),
                    },
                )?;
                continue;
            }
            authenticated = true;
        }

        let response: Result<Response, fleet_agent::AgentError> = match request {
            Request::Hello {
                client_id: _,
                token: _,
            } => Ok(Response::Hello {
                server_id: format!("fleetd-{}", std::process::id()),
                wire: WIRE.into(),
            }),
            Request::RegisterWorkspace { path } => {
                manager
                    .register_workspace(&path)
                    .map(|workspace| Response::WorkspaceRegistered {
                        workspace: fleet_protocol::WorkspaceSummary {
                            id: workspace.id,
                            path: workspace.path.to_string_lossy().into_owned(),
                        },
                    })
            }
            Request::ListWorkspaces => Ok(Response::Workspaces {
                workspaces: manager
                    .list_workspaces()
                    .into_iter()
                    .map(|workspace| fleet_protocol::WorkspaceSummary {
                        id: workspace.id,
                        path: workspace.path.to_string_lossy().into_owned(),
                    })
                    .collect(),
            }),
            Request::ListAgents => Ok(Response::Agents {
                agents: manager
                    .provider_descriptors()
                    .into_iter()
                    .map(|descriptor| AgentSummary {
                        id: descriptor.id,
                        version: if descriptor.version.text == "0.0.0" {
                            None
                        } else {
                            Some(descriptor.version.text)
                        },
                        availability: format!("{:?}", descriptor.availability),
                        capabilities: descriptor.capabilities,
                        mode: format!("{:?}", descriptor.mode),
                    })
                    .collect(),
            }),
            Request::CreateSession {
                workspace_id,
                provider_id,
            } => manager
                .create_session(&workspace_id, &provider_id)
                .map(|session| Response::SessionCreated {
                    session: fleet_protocol::SessionSummary {
                        id: session.id,
                        workspace_id: session.workspace_id,
                        provider_id: session.provider_id,
                        state: format!("{:?}", session.state),
                        turn_state: format!("{:?}", session.turn_state),
                    },
                }),
            Request::ListSessions => Ok(Response::Sessions {
                sessions: manager
                    .list_sessions()
                    .into_iter()
                    .map(|session| fleet_protocol::SessionSummary {
                        id: session.id,
                        workspace_id: session.workspace_id,
                        provider_id: session.provider_id,
                        state: format!("{:?}", session.state),
                        turn_state: format!("{:?}", session.turn_state),
                    })
                    .collect(),
            }),
            Request::SendPrompt {
                session_id,
                text,
                operation_key,
            } => manager
                .send_prompt(&session_id, text, &operation_key)
                .map(|receipt| Response::PromptAccepted {
                    operation_id: receipt.operation_id,
                    session_id: receipt.session_id,
                    turn_id: receipt.turn_id,
                }),
            Request::StopSession { session_id } => manager
                .stop_session(&session_id)
                .map(|_| Response::Stopped { session_id }),
            Request::SubscribeTimeline {
                session_id,
                since_seq,
            } => {
                let limit = std::env::var("FLEET_SUBSCRIBER_LIMIT")
                    .ok()
                    .and_then(|value| value.parse::<usize>().ok())
                    .filter(|value| *value > 0)
                    .unwrap_or(SUBSCRIBER_LIMIT);
                match manager.subscribe(&session_id, since_seq, limit) {
                    Ok((new_subscription, snapshot)) => {
                        let latest = snapshot.last().map(|event| event.seq).unwrap_or(since_seq);
                        // Cada suscripción tiene cursor propio.
                        let last_seq = Arc::new(AtomicU64::new(since_seq));
                        if let Some(old) = subscription.take() {
                            old.cancel();
                            manager.bus().unsubscribe(old.id);
                        }
                        let handle = new_subscription.handle();
                        subscription = Some(handle);
                        for event in snapshot {
                            last_seq.store(event.seq, Ordering::SeqCst);
                            write_frame(
                                &mut *writer.lock().expect("writer poisoned"),
                                &Frame::Event {
                                    session_id: session_id.clone(),
                                    seq: event.seq,
                                    event,
                                },
                            )?;
                        }
                        let writer_for_events = Arc::clone(&writer);
                        let seq_for_events = Arc::clone(&last_seq);
                        let session_for_events = session_id.clone();
                        std::thread::spawn(move || {
                            forward_events(
                                new_subscription,
                                session_for_events,
                                writer_for_events,
                                seq_for_events,
                            );
                        });
                        Ok(Response::Subscribed {
                            session_id,
                            latest_seq: latest,
                        })
                    }
                    Err(error) => Err(error),
                }
            }
        };

        write_frame(
            &mut *writer.lock().expect("writer poisoned"),
            &Frame::Response {
                request_id,
                result: response.map_err(to_api_error),
            },
        )?;
    }

    if let Some(subscription) = subscription {
        subscription.cancel();
        manager.bus().unsubscribe(subscription.id);
    }
    Ok(())
}

fn forward_events(
    subscription: Subscription,
    session_id: String,
    writer: Arc<Mutex<BufWriter<LocalStream>>>,
    last_seq: Arc<AtomicU64>,
) {
    loop {
        let overflow = matches!(
            subscription.recv_signal(Duration::from_millis(1)),
            Ok(Some(fleet_core::SubscriberSignal::Overflow))
        );
        if overflow
            || (subscription.cancelled.load(Ordering::SeqCst)
                && matches!(
                    subscription.recv_signal(Duration::from_millis(1)),
                    Ok(Some(fleet_core::SubscriberSignal::Overflow))
                ))
        {
            let _ = write_frame(
                &mut *writer.lock().expect("writer poisoned"),
                &Frame::Notice {
                    session_id: Some(session_id.clone()),
                    code: "Backpressure".into(),
                    message: format!(
                        "subscriber queue limit {} reached; subscription closed without silent loss",
                        subscription.limit
                    ),
                },
            );
            break;
        }
        if subscription.cancelled.load(Ordering::SeqCst) {
            break;
        }
        match subscription.recv_event(Duration::from_millis(50)) {
            Ok(Some(event)) => {
                if event.seq <= last_seq.load(Ordering::SeqCst) {
                    continue;
                }
                last_seq.store(event.seq, Ordering::SeqCst);
                let frame = Frame::Event {
                    session_id: session_id.clone(),
                    seq: event.seq,
                    event,
                };
                let mut guard = writer.lock().expect("writer poisoned");
                if subscription.cancelled.load(Ordering::SeqCst) {
                    break;
                }
                if write_frame(&mut *guard, &frame).is_err() {
                    break;
                }
            }
            Ok(None) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        }
    }
}

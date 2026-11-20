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
use fleet_store_sqlite::SqliteStore;

fn to_api_error(error: fleet_agent::AgentError) -> fleet_protocol::ApiError {
    fleet_protocol::ApiError {
        code: error.code,
        message: error.message,
    }
}

fn database_path() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("FLEET_DB_PATH") {
        return std::path::PathBuf::from(path);
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".local/share"))
        })
        .unwrap_or_else(std::env::temp_dir);
    base.join("gentle-fleet").join("fleet.sqlite")
}

fn main() -> std::io::Result<()> {
    let mut registry = ProviderRegistry::new();
    let fake_config = FakeConfig {
        block_send: std::env::var("FLEET_FAKE_BLOCK_SEND").is_ok(),
        block_close: std::env::var("FLEET_FAKE_BLOCK_CLOSE").is_ok(),
        fail_send: std::env::var("FLEET_FAKE_FAIL_SEND").is_ok(),
        emit_provider_error: std::env::var("FLEET_FAKE_PROVIDER_ERROR").is_ok(),
        emit_terminal_failure: std::env::var("FLEET_FAKE_TERMINAL_FAILURE").is_ok(),
        emit_approval: std::env::var("FLEET_FAKE_APPROVAL").is_ok(),
    };
    registry.register(Box::new(FakeFactory::with_config(fake_config)));
    registry.register(Box::new(OpenCodeFactory::with_ask_shell(
        std::env::var("FLEET_OPENCODE_ASK_SHELL").is_ok(),
    )));
    let store = Arc::new(
        SqliteStore::open(database_path())
            .map_err(|e| std::io::Error::other(format!("{}: {}", e.code, e.message)))?,
    );
    let manager =
        Arc::new(SessionManager::new(registry, store).map_err(|error| {
            std::io::Error::other(format!("{}: {}", error.code, error.message))
        })?);
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
            Request::ReadTimeline {
                session_id,
                since_seq,
                limit,
            } => manager
                .read_timeline(&session_id, since_seq, limit)
                .map(|events| Response::Timeline { session_id, events }),
            Request::GetOperation { operation_key } => {
                manager.get_operation(&operation_key).and_then(|operation| {
                    operation
                        .map(|operation| Response::Operation { operation })
                        .ok_or_else(|| fleet_agent::AgentError {
                            code: "not_found".into(),
                            message: format!("unknown operation key `{operation_key}`"),
                        })
                })
            }
            Request::ListApprovals { session_id } => Ok(Response::Approvals {
                approvals: manager.list_approvals(session_id.as_deref()),
            }),
            Request::GetApproval { approval_id } => manager
                .get_approval(&approval_id)
                .map(|approval| Response::Approval { approval })
                .ok_or_else(|| fleet_agent::AgentError {
                    code: "not_found".into(),
                    message: format!("unknown approval `{approval_id}`"),
                }),
            Request::AnswerApproval {
                approval_id,
                decision,
                operation_key,
            } => manager
                .answer_approval(&approval_id, decision, &operation_key)
                .map(|approval| Response::ApprovalResolved { approval }),
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
                    Ok((new_subscription, snapshot, cutoff)) => {
                        let latest = cutoff;
                        // Cada suscripción tiene cursor propio.
                        let last_seq = Arc::new(AtomicU64::new(since_seq));
                        if let Some(old) = subscription.take() {
                            old.cancel();
                            manager.bus().unsubscribe(old.id);
                        }
                        let handle = new_subscription.handle();
                        subscription = Some(handle);
                        // Stream the complete durable backlog in bounded pages
                        // before the live receiver starts. Snapshot cut-off was
                        // captured under the same lock as publishing.
                        let mut pending = snapshot;
                        let mut cursor = since_seq;
                        while cursor < cutoff {
                            if pending.is_empty() {
                                pending = manager
                                    .read_timeline(&session_id, cursor, fleet_core::TIMELINE_PAGE)
                                    .map_err(|error| std::io::Error::other(error.to_string()))?;
                                if pending.is_empty() {
                                    return Err(std::io::Error::other(
                                        "durable timeline gap while replaying subscription",
                                    ));
                                }
                            }
                            for event in pending.drain(..) {
                                if event.seq > cutoff {
                                    break;
                                }
                                if event.seq <= cursor {
                                    continue;
                                }
                                if event.seq != cursor + 1 {
                                    return Err(std::io::Error::other(
                                        "non-contiguous durable event sequence",
                                    ));
                                }
                                cursor = event.seq;
                                last_seq.store(cursor, Ordering::SeqCst);
                                write_frame(
                                    &mut *writer.lock().expect("writer poisoned"),
                                    &Frame::Event {
                                        session_id: session_id.clone(),
                                        seq: cursor,
                                        event,
                                    },
                                )?;
                            }
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
    // Solo para builds de desarrollo: pausa determinista del consumidor
    // utilizada por tests/e2e_backpressure.sh.
    #[cfg(debug_assertions)]
    if let Some(delay) = std::env::var("FLEET_TEST_FORWARD_DELAY_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    {
        std::thread::sleep(Duration::from_millis(delay.min(5_000)));
    }
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

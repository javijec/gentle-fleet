//! Fleet CLI P1: cliente delgado de la API compartida; no gestiona procesos.

use std::io::{BufReader, BufWriter};

use fleet_domain::EventKind;
use fleet_protocol::transport::{connect, expected_token, LocalStream};
use fleet_protocol::{read_frame, write_frame, Frame, Request, Response};

struct Client {
    reader: BufReader<LocalStream>,
    writer: BufWriter<LocalStream>,
    next_id: u64,
}

impl Client {
    fn connect() -> Result<Self, String> {
        let stream =
            connect().map_err(|e| format!("cannot connect to fleetd (is it running?): {e}"))?;
        Ok(Self {
            reader: BufReader::new(stream.try_clone().map_err(|e| e.to_string())?),
            writer: BufWriter::new(stream),
            next_id: 0,
        })
    }

    fn hello() -> Request {
        Request::Hello {
            client_id: "fleetctl".into(),
            token: expected_token().ok().flatten(),
        }
    }

    fn send(&mut self, request: Request) -> Result<String, String> {
        self.next_id += 1;
        let request_id = format!("r{}", self.next_id);
        write_frame(
            &mut self.writer,
            &Frame::Request {
                request_id: request_id.clone(),
                request,
            },
        )
        .map_err(|e| e.to_string())?;
        Ok(request_id)
    }

    fn read_frame(&mut self) -> Result<Frame, String> {
        read_frame(&mut self.reader)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "fleetd closed the connection".into())
    }

    fn call(&mut self, request: Request) -> Result<Response, String> {
        let request_id = self.send(request)?;
        loop {
            match self.read_frame()? {
                Frame::Response {
                    request_id: id,
                    result,
                } if id == request_id => {
                    return result.map_err(|e| format!("{}: {}", e.code, e.message));
                }
                Frame::Event { .. } => continue,
                Frame::Notice { code, message, .. } => {
                    return Err(format!("{code}: {message}"));
                }
                _ => continue,
            }
        }
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first() else {
        return Err(usage());
    };
    match command.as_str() {
        "workspace" => workspace(&args[1..]),
        "session" => session(&args[1..]),
        "prompt" => prompt(&args[1..]),
        "watch" => watch(&args[1..]),
        "stop" => stop(&args[1..]),
        "timeline" => timeline(&args[1..]),
        "operation" => operation(&args[1..]),
        "approvals" => approvals(&args[1..]),
        "approval" => approval(&args[1..]),
        "answer" => answer(&args[1..]),
        "agents" => {
            let mut client = Client::connect()?;
            client.call(Client::hello())?;
            match client.call(Request::ListAgents)? {
                Response::Agents { agents } => {
                    for agent in agents {
                        println!(
                            "{}\t{}\t{}\t{}\t{}",
                            agent.id,
                            agent.version.unwrap_or_else(|| "unknown".into()),
                            agent.availability,
                            agent.mode,
                            agent.capabilities.streaming_text
                        );
                    }
                    Ok(())
                }
                other => Err(format!("unexpected response: {other:?}")),
            }
        }
        _ => Err(usage()),
    }
}

fn usage() -> String {
    "usage:\n  fleetctl workspace add <path>\n  fleetctl workspace list\n  fleetctl session create --workspace <id> --provider <id>\n  fleetctl session list\n  fleetctl prompt <session_id> <text>\n  fleetctl watch <session_id> [--since N] [--until-turn-end] [--count N]\n  fleetctl stop <session_id>\n  fleetctl timeline <session_id> [--since N] [--limit N]\n  fleetctl operation <operation_key>\n  fleetctl approvals [--session <id>]\n  fleetctl approval <approval_id>\n  fleetctl answer <approval_id> <decision>\n  fleetctl agents"
        .into()
}

fn workspace(args: &[String]) -> Result<(), String> {
    let mut client = Client::connect()?;
    client.call(Client::hello())?;
    match args.first().map(String::as_str) {
        Some("add") => {
            let path = args.get(1).ok_or_else(usage)?;
            match client.call(Request::RegisterWorkspace { path: path.clone() })? {
                Response::WorkspaceRegistered { workspace } => {
                    println!("{}\t{}", workspace.id, workspace.path);
                    Ok(())
                }
                other => Err(format!("unexpected response: {other:?}")),
            }
        }
        Some("list") => match client.call(Request::ListWorkspaces)? {
            Response::Workspaces { workspaces } => {
                for workspace in workspaces {
                    println!("{}\t{}", workspace.id, workspace.path);
                }
                Ok(())
            }
            other => Err(format!("unexpected response: {other:?}")),
        },
        _ => Err(usage()),
    }
}

fn session(args: &[String]) -> Result<(), String> {
    let mut client = Client::connect()?;
    client.call(Client::hello())?;
    match args.first().map(String::as_str) {
        Some("create") => {
            let workspace = flag(args, "--workspace").ok_or_else(usage)?;
            let provider = flag(args, "--provider").unwrap_or_else(|| "opencode".into());
            match client.call(Request::CreateSession {
                workspace_id: workspace,
                provider_id: provider,
            })? {
                Response::SessionCreated { session } => {
                    println!(
                        "{}\t{}\t{}\t{}",
                        session.id, session.provider_id, session.state, session.turn_state
                    );
                    Ok(())
                }
                other => Err(format!("unexpected response: {other:?}")),
            }
        }
        Some("list") => match client.call(Request::ListSessions)? {
            Response::Sessions { sessions } => {
                for session in sessions {
                    println!(
                        "{}\t{}\t{}\t{}\t{}",
                        session.id,
                        session.provider_id,
                        session.workspace_id,
                        session.state,
                        session.turn_state
                    );
                }
                Ok(())
            }
            other => Err(format!("unexpected response: {other:?}")),
        },
        _ => Err(usage()),
    }
}

fn prompt(args: &[String]) -> Result<(), String> {
    let session_id = args.first().ok_or_else(usage)?.clone();
    let text = args[1..].join(" ");
    if text.is_empty() {
        return Err(usage());
    }
    let operation_key = std::env::var("FLEET_OPERATION_KEY")
        .ok()
        .filter(|key| !key.is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mut client = Client::connect()?;
    client.call(Client::hello())?;
    match client.call(Request::SendPrompt {
        session_id,
        text,
        operation_key,
    })? {
        Response::PromptAccepted {
            operation_id,
            session_id,
            turn_id,
        } => {
            println!("{operation_id}\t{session_id}\t{turn_id}");
            Ok(())
        }
        other => Err(format!("unexpected response: {other:?}")),
    }
}

fn stop(args: &[String]) -> Result<(), String> {
    let session_id = args.first().ok_or_else(usage)?.clone();
    let mut client = Client::connect()?;
    client.call(Client::hello())?;
    match client.call(Request::StopSession { session_id })? {
        Response::Stopped { session_id } => {
            println!("stopped\t{session_id}");
            Ok(())
        }
        other => Err(format!("unexpected response: {other:?}")),
    }
}

fn watch(args: &[String]) -> Result<(), String> {
    let session_id = args.first().ok_or_else(usage)?.clone();
    let since = flag(args, "--since")
        .map(|value| value.parse::<u64>())
        .transpose()
        .map_err(|e| format!("invalid --since: {e}"))?
        .unwrap_or(0);
    let until_turn_end = args.iter().any(|a| a == "--until-turn-end");
    let count = flag(args, "--count")
        .map(|value| value.parse::<usize>())
        .transpose()
        .map_err(|e| format!("invalid --count: {e}"))?;
    let mut client = Client::connect()?;
    client.call(Client::hello())?;
    let request_id = client.send(Request::SubscribeTimeline {
        session_id: session_id.clone(),
        since_seq: since,
    })?;
    let mut last_seq = since;
    let mut printed = 0usize;
    loop {
        match client.read_frame()? {
            Frame::Response {
                request_id: id,
                result,
            } if id == request_id => {
                let latest = match result.map_err(|e| format!("{}: {}", e.code, e.message))? {
                    Response::Subscribed { latest_seq, .. } => latest_seq,
                    other => return Err(format!("unexpected response: {other:?}")),
                };
                eprintln!("subscribed\t{session_id}\tlatest={latest}");
            }
            Frame::Notice { code, message, .. } => {
                eprintln!("notice\t{code}\t{message}");
                if code == "Backpressure" {
                    return Err("subscription closed by backpressure; use --since to resume".into());
                }
            }
            Frame::Event { seq, event, .. } => {
                if seq <= last_seq {
                    continue; // deducción defensiva, nunca duplica mensajes
                }
                last_seq = seq;
                println!("{}\t{}", seq, describe(&event.kind));
                printed += 1;
                if count.is_some_and(|limit| printed >= limit) {
                    return Ok(());
                }
                if until_turn_end && matches!(event.kind, EventKind::TurnCompleted { .. }) {
                    return Ok(());
                }
            }
            _ => {}
        }
    }
}

fn describe(kind: &EventKind) -> String {
    match kind {
        EventKind::SessionCreated { provider_id, .. } => format!("session_created\t{provider_id}"),
        EventKind::SessionStateChanged { state } => format!("session_state\t{state:?}"),
        EventKind::TurnStarted { .. } => "turn_started".into(),
        EventKind::UserMessage { text, .. } => format!("user\t{text}"),
        EventKind::AssistantDelta { text, .. } => format!("delta\t{text}"),
        EventKind::AssistantMessage { text, .. } => format!("assistant\t{text}"),
        EventKind::ToolStarted { name, input, .. } => format!("tool_started\t{name}\t{input}"),
        EventKind::ToolOutput { name, output, .. } => format!("tool_output\t{name}\t{output}"),
        EventKind::ToolCompleted { name, success, .. } => {
            format!("tool_completed\t{name}\t{success}")
        }
        EventKind::TurnCompleted { status, .. } => format!("turn_completed\t{status:?}"),
        EventKind::AgentNotice { message } => format!("agent_notice\t{message}"),
        EventKind::AgentError { message } => format!("agent_error\t{message}"),
        EventKind::TurnUncertain { turn_id, reason } => {
            format!("turn_uncertain\t{turn_id}\t{reason}")
        }
        EventKind::Backpressure { limit } => format!("backpressure\t{limit}"),
        EventKind::ApprovalRequested {
            approval_id,
            kind,
            resources,
            allowed_decisions,
            ..
        } => format!(
            "approval_requested\t{approval_id}\t{kind}\t{}\t{}",
            resources.join(","),
            allowed_decisions.join(",")
        ),
        EventKind::ApprovalResolved {
            approval_id,
            decision,
            ..
        } => format!("approval_resolved\t{approval_id}\t{decision}"),
    }
}

fn timeline(args: &[String]) -> Result<(), String> {
    let session_id = args.first().ok_or_else(usage)?.clone();
    let since = flag(args, "--since")
        .map(|value| value.parse::<u64>())
        .transpose()
        .map_err(|e| format!("invalid --since: {e}"))?
        .unwrap_or(0);
    let limit = flag(args, "--limit")
        .map(|value| value.parse::<usize>())
        .transpose()
        .map_err(|e| format!("invalid --limit: {e}"))?
        .unwrap_or(256);
    let mut client = Client::connect()?;
    client.call(Client::hello())?;
    match client.call(Request::ReadTimeline {
        session_id,
        since_seq: since,
        limit,
    })? {
        Response::Timeline { events, .. } => {
            for event in events {
                println!(
                    "{}\t{}\t{}",
                    event.seq,
                    event.event_id,
                    describe(&event.kind)
                );
            }
            Ok(())
        }
        other => Err(format!("unexpected response: {other:?}")),
    }
}

fn operation(args: &[String]) -> Result<(), String> {
    let key = args.first().ok_or_else(usage)?.clone();
    let mut client = Client::connect()?;
    client.call(Client::hello())?;
    match client.call(Request::GetOperation { operation_key: key })? {
        Response::Operation { operation } => {
            println!(
                "{}\t{}\t{:?}\t{:?}\t{}",
                operation.operation_id,
                operation.operation_key,
                operation.kind,
                operation.status,
                operation.message.unwrap_or_default()
            );
            Ok(())
        }
        other => Err(format!("unexpected response: {other:?}")),
    }
}

fn approvals(args: &[String]) -> Result<(), String> {
    let session_id = flag(args, "--session");
    let mut client = Client::connect()?;
    client.call(Client::hello())?;
    match client.call(Request::ListApprovals { session_id })? {
        Response::Approvals { approvals } => {
            for approval in approvals {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    approval.approval_id,
                    approval.session_id,
                    approval.kind,
                    approval.status_text(),
                    approval.allowed_decisions.join(",")
                );
            }
            Ok(())
        }
        other => Err(format!("unexpected response: {other:?}")),
    }
}

fn approval(args: &[String]) -> Result<(), String> {
    let approval_id = args.first().ok_or_else(usage)?.clone();
    let mut client = Client::connect()?;
    client.call(Client::hello())?;
    match client.call(Request::GetApproval { approval_id })? {
        Response::Approval { approval } => {
            println!(
                "{}\t{}\t{}\t{}\t{}",
                approval.approval_id,
                approval.session_id,
                approval.kind,
                approval.status_text(),
                approval.message
            );
            Ok(())
        }
        other => Err(format!("unexpected response: {other:?}")),
    }
}

fn answer(args: &[String]) -> Result<(), String> {
    let approval_id = args.first().ok_or_else(usage)?.clone();
    let decision = args.get(1).ok_or_else(usage)?.clone();
    let operation_key = std::env::var("FLEET_OPERATION_KEY")
        .ok()
        .filter(|key| !key.is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mut client = Client::connect()?;
    client.call(Client::hello())?;
    match client.call(Request::AnswerApproval {
        approval_id,
        decision,
        operation_key,
    })? {
        Response::ApprovalResolved { approval } => {
            println!("{}\t{}", approval.approval_id, approval.status_text());
            Ok(())
        }
        other => Err(format!("unexpected response: {other:?}")),
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

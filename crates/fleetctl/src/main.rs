//! Fleet CLI P1: cliente delgado de la API compartida; no gestiona procesos.

use fleet_client::{
    inspect_operation_journal, operation_journal_path, recorded_operation_attempts, FleetClient,
    Host,
};
use fleet_domain::EventKind;
use fleet_protocol::{Request, Response};
use fleet_ssh::{add_host, find_host, load_hosts, remove_host, HostConfig};

fn show_pending(selected_host: Option<&str>, inspect: bool) -> Result<(), String> {
    let (attempts, incomplete_tail_bytes) = if inspect {
        let result = inspect_operation_journal().map_err(|error| error.to_string())?;
        (result.attempts, result.incomplete_tail_bytes)
    } else {
        (
            recorded_operation_attempts().map_err(|error| error.to_string())?,
            0,
        )
    };
    for record in attempts {
        if selected_host.is_some_and(|selected| selected != record.host) {
            continue;
        }
        println!(
            "{}\t{}\t{}\t{}",
            record.host, record.kind, record.target, record.operation_key
        );
    }
    if incomplete_tail_bytes != 0 {
        return Err(format!(
            "inspection only: {incomplete_tail_bytes} incomplete tail bytes preserved; journal still blocked; reconcile recorded keys without resending"
        ));
    }
    Ok(())
}

struct Client(FleetClient);

impl Client {
    fn connect(host: Option<&str>) -> Result<Self, String> {
        let host = host.map_or(Host::Local, |alias| Host::Ssh(alias.into()));
        FleetClient::connect(host)
            .map(Self)
            .map_err(|error| error.to_string())
    }

    fn call(&mut self, request: Request) -> Result<Response, String> {
        if let fleet_client::Host::Ssh(alias) = self.0.host() {
            let entry = match &request {
                Request::SendPrompt { operation_key, .. } => Some(operation_key),
                Request::AnswerApproval { operation_key, .. } => Some(operation_key),
                Request::ImportNativeHistory { operation_key, .. } => Some(operation_key),
                _ => None,
            };
            if let Some(key) = entry {
                let path = operation_journal_path().map_err(|error| error.to_string())?;
                eprintln!(
                    "remote operation_key={key} host={alias} (journal={})",
                    path.display()
                );
            }
        }
        self.0.request(request).map_err(|error| error.to_string())
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let all_args: Vec<String> = std::env::args().skip(1).collect();
    let mut args = all_args.clone();
    let selected_host = if args.first().map(String::as_str) == Some("--host") {
        let alias = args
            .get(1)
            .ok_or_else(|| "--host requires an alias".to_string())?
            .clone();
        // Los comandos de gestión de hosts no necesitan conexión.
        if args.get(2).map(String::as_str) != Some("host") {
            find_host(&alias).map_err(|error| error.to_string())?;
        }
        args = args[2..].to_vec();
        Some(alias)
    } else {
        None
    };
    let Some(command) = args.first() else {
        return Err(usage());
    };
    if command == "host" {
        return host_command(&args[1..]);
    }
    if command == "pending" {
        return match args.as_slice() {
            [_] => show_pending(selected_host.as_deref(), false),
            [_, flag] if flag == "--inspect-torn" => show_pending(selected_host.as_deref(), true),
            _ => Err("usage: fleetctl [--host alias] pending [--inspect-torn]".into()),
        };
    }
    match command.as_str() {
        "workspace" => workspace(&args[1..], selected_host.as_deref()),
        "session" => session(&args[1..], selected_host.as_deref()),
        "prompt" => prompt(&args[1..], selected_host.as_deref()),
        "interrupt" => interrupt(&args[1..], selected_host.as_deref()),
        "controls" => controls(&args[1..], selected_host.as_deref()),
        "set-control" => set_control(&args[1..], selected_host.as_deref()),
        "native-sessions" => native_sessions(&args[1..], selected_host.as_deref()),
        "import-history" => import_history(&args[1..], selected_host.as_deref()),
        "watch" => watch(&args[1..], selected_host.as_deref()),
        "stop" => stop(&args[1..], selected_host.as_deref()),
        "timeline" => timeline(&args[1..], selected_host.as_deref()),
        "operation" => operation(&args[1..], selected_host.as_deref()),
        "approvals" => approvals(&args[1..], selected_host.as_deref()),
        "approval" => approval(&args[1..], selected_host.as_deref()),
        "answer" => answer(&args[1..], selected_host.as_deref()),
        "agents" => {
            let mut client = Client::connect(selected_host.as_deref())?;
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

fn host_command(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("list") => {
            for host in load_hosts().map_err(|error| error.to_string())? {
                println!(
                    "{}\t{}\t{}\t{}",
                    host.alias,
                    host.destination,
                    host.bridge_path,
                    host.last_status.unwrap_or_else(|| "unknown".into())
                );
            }
            Ok(())
        }
        Some("show") => {
            let alias = args.get(1).ok_or_else(|| "host show <alias>".to_string())?;
            let host = find_host(alias).map_err(|error| error.to_string())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&host).map_err(|e| e.to_string())?
            );
            Ok(())
        }
        Some("add") => {
            let alias = args
                .get(1)
                .ok_or_else(|| "host add <alias> <destination>".to_string())?
                .clone();
            let destination = args
                .get(2)
                .ok_or_else(|| "host add <alias> <destination>".to_string())?
                .clone();
            let mut bridge = None;
            let mut port = None;
            let mut identity = None;
            let mut index = 3;
            while index < args.len() {
                match args[index].as_str() {
                    "--bridge" => {
                        bridge = Some(args.get(index + 1).ok_or("missing bridge path")?.clone());
                        index += 2;
                    }
                    "--port" => {
                        port = Some(
                            args.get(index + 1)
                                .ok_or("missing port")?
                                .parse::<u16>()
                                .map_err(|error| format!("invalid port: {error}"))?,
                        );
                        index += 2;
                    }
                    "--identity" => {
                        identity = Some(args.get(index + 1).ok_or("missing identity")?.clone());
                        index += 2;
                    }
                    other => return Err(format!("unknown host option: {other}")),
                }
            }
            add_host(HostConfig {
                alias: alias.clone(),
                destination,
                bridge_path: bridge.unwrap_or_else(|| "/usr/local/bin/fleet-ssh-bridge".into()),
                port,
                identity_file: identity,
                expected_host_key: None,
                last_status: None,
                protocol_version: None,
            })
            .map_err(|error| error.to_string())?;
            println!("added\t{alias}");
            Ok(())
        }
        Some("remove") => {
            let alias = args
                .get(1)
                .ok_or_else(|| "host remove <alias>".to_string())?;
            remove_host(alias).map_err(|error| error.to_string())?;
            println!("removed\t{alias}");
            Ok(())
        }
        Some("check") => {
            let alias = args
                .get(1)
                .ok_or_else(|| "host check <alias>".to_string())?;
            let _client = Client::connect(Some(alias))?;
            println!("connected\t{alias}\t{}", fleet_protocol::WIRE);
            Ok(())
        }
        _ => Err("usage: fleetctl host add|list|show|remove|check ...".into()),
    }
}

fn usage() -> String {
    "usage:\n  fleetctl workspace add <path>\n  fleetctl workspace list\n  fleetctl session create --workspace <id> --provider <id>\n  fleetctl session list\n  fleetctl prompt <session_id> <text>\n  fleetctl interrupt <session_id> <turn_id>\n  fleetctl controls <session_id>\n  fleetctl set-control <session_id> <name> <value> [turn|session]\n  fleetctl native-sessions <provider_id>\n  fleetctl import-history <session_id> <provider_id> <native_ref> [operation_key]\n  fleetctl watch <session_id> [--since N] [--until-turn-end] [--count N]\n  fleetctl stop <session_id>\n  fleetctl timeline <session_id> [--since N] [--limit N]\n  fleetctl operation <operation_key>\n  fleetctl approvals [--session <id>]\n  fleetctl approval <approval_id>\n  fleetctl answer <approval_id> <decision> [name=value[,value] ...]\n  fleetctl agents\n  fleetctl [--host alias] pending [--inspect-torn]  # list durable remote operation keys"
        .into()
}

fn workspace(args: &[String], selected_host: Option<&str>) -> Result<(), String> {
    let mut client = Client::connect(selected_host)?;
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

fn session(args: &[String], selected_host: Option<&str>) -> Result<(), String> {
    let mut client = Client::connect(selected_host)?;
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

fn native_sessions(args: &[String], selected_host: Option<&str>) -> Result<(), String> {
    let provider_id = args.first().ok_or_else(usage)?.clone();
    let mut client = Client::connect(selected_host)?;
    match client.call(Request::ListNativeSessions { provider_id })? {
        Response::NativeSessions { sessions } => {
            for session in sessions {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    session.native_ref,
                    session.workspace_hint.unwrap_or_else(|| "unknown".into()),
                    session.state,
                    session.activity,
                    session.model.unwrap_or_else(|| "unknown".into()),
                    session.effort.unwrap_or_else(|| "unknown".into()),
                    session.externally_active
                );
            }
            Ok(())
        }
        other => Err(format!("unexpected response: {other:?}")),
    }
}

fn import_history(args: &[String], selected_host: Option<&str>) -> Result<(), String> {
    let session_id = args.first().ok_or_else(usage)?.clone();
    let provider_id = args.get(1).ok_or_else(usage)?.clone();
    let native_ref = args.get(2).ok_or_else(usage)?.clone();
    let operation_key = args.get(3).cloned().unwrap_or_else(|| {
        std::env::var("FLEET_OPERATION_KEY").unwrap_or_else(|_| uuid::Uuid::new_v4().to_string())
    });
    let mut client = Client::connect(selected_host)?;
    match client.call(Request::ImportNativeHistory {
        session_id,
        provider_id,
        native_ref,
        operation_key,
    })? {
        Response::HistoryImported { count } => {
            println!("imported\t{count}");
            Ok(())
        }
        other => Err(format!("unexpected response: {other:?}")),
    }
}

fn controls(args: &[String], selected_host: Option<&str>) -> Result<(), String> {
    let session_id = args.first().ok_or_else(usage)?.clone();
    let mut client = Client::connect(selected_host)?;
    match client.call(Request::ListControls { session_id })? {
        Response::Controls { controls } => {
            for control in controls {
                println!(
                    "{}\t{}\t{}\t{}",
                    control.name,
                    control.current.unwrap_or_else(|| "unknown".into()),
                    control.available.join(","),
                    control.status
                );
            }
            Ok(())
        }
        other => Err(format!("unexpected response: {other:?}")),
    }
}

fn set_control(args: &[String], selected_host: Option<&str>) -> Result<(), String> {
    let session_id = args.first().ok_or_else(usage)?.clone();
    let name = args.get(1).ok_or_else(usage)?.clone();
    let value = args.get(2).ok_or_else(usage)?.clone();
    let scope = args.get(3).cloned().unwrap_or_else(|| "turn".into());
    let mut client = Client::connect(selected_host)?;
    match client.call(Request::SetControl {
        session_id,
        name,
        value,
        scope,
    })? {
        Response::ControlChanged { control } => {
            println!(
                "{}\t{}\t{}",
                control.name,
                control.current.unwrap_or_default(),
                control.status
            );
            Ok(())
        }
        other => Err(format!("unexpected response: {other:?}")),
    }
}

fn interrupt(args: &[String], selected_host: Option<&str>) -> Result<(), String> {
    let session_id = args.first().ok_or_else(usage)?.clone();
    let turn_id = args.get(1).ok_or_else(usage)?.clone();
    let mut client = Client::connect(selected_host)?;
    match client.call(Request::InterruptTurn {
        session_id,
        turn_id,
    })? {
        Response::TurnInterrupted {
            session_id,
            turn_id,
        } => {
            println!("interrupted\t{session_id}\t{turn_id}");
            Ok(())
        }
        other => Err(format!("unexpected response: {other:?}")),
    }
}

fn prompt(args: &[String], selected_host: Option<&str>) -> Result<(), String> {
    let session_id = args.first().ok_or_else(usage)?.clone();
    let text = args[1..].join(" ");
    if text.is_empty() {
        return Err(usage());
    }
    let operation_key = std::env::var("FLEET_OPERATION_KEY")
        .ok()
        .filter(|key| !key.is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mut client = Client::connect(selected_host)?;
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

fn stop(args: &[String], selected_host: Option<&str>) -> Result<(), String> {
    let session_id = args.first().ok_or_else(usage)?.clone();
    let mut client = Client::connect(selected_host)?;
    match client.call(Request::StopSession { session_id })? {
        Response::Stopped { session_id } => {
            println!("stopped\t{session_id}");
            Ok(())
        }
        other => Err(format!("unexpected response: {other:?}")),
    }
}

fn watch(args: &[String], selected_host: Option<&str>) -> Result<(), String> {
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
    let host = selected_host.map_or(Host::Local, |alias| Host::Ssh(alias.into()));
    let client = FleetClient::connect(host).map_err(|error| error.to_string())?;
    let subscription = client
        .subscribe_timeline(session_id.clone(), since)
        .map_err(|error| error.to_string())?;
    eprintln!("subscribed\t{session_id}\tsince={since}");
    let mut printed = 0usize;
    loop {
        let event = match subscription.recv() {
            Ok(event) => event,
            // The SDK observer stays alive and reconnects after transient errors.
            Err(fleet_client::ClientError::Transport(_)) => continue,
            Err(fleet_client::ClientError::Api { code, .. }) if code == "Timeout" => {
                continue;
            }
            Err(error) => return Err(error.to_string()),
        };
        println!("{}\t{}", event.seq, describe(&event.kind));
        printed += 1;
        if count.is_some_and(|limit| printed >= limit) {
            return Ok(());
        }
        if until_turn_end && matches!(event.kind, EventKind::TurnCompleted { .. }) {
            return Ok(());
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
        EventKind::HistoryMessage {
            role,
            text,
            provenance,
            ..
        } => format!("history\t{role}\t{provenance}\t{text}"),
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

fn timeline(args: &[String], selected_host: Option<&str>) -> Result<(), String> {
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
    let mut client = Client::connect(selected_host)?;
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

fn operation(args: &[String], selected_host: Option<&str>) -> Result<(), String> {
    let key = args.first().ok_or_else(usage)?.clone();
    let mut client = Client::connect(selected_host)?;
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

fn approvals(args: &[String], selected_host: Option<&str>) -> Result<(), String> {
    let session_id = flag(args, "--session");
    let mut client = Client::connect(selected_host)?;
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

fn approval(args: &[String], selected_host: Option<&str>) -> Result<(), String> {
    let approval_id = args.first().ok_or_else(usage)?.clone();
    let mut client = Client::connect(selected_host)?;
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

fn answer(args: &[String], selected_host: Option<&str>) -> Result<(), String> {
    let approval_id = args.first().ok_or_else(usage)?.clone();
    let decision = args.get(1).ok_or_else(usage)?.clone();
    let operation_key = std::env::var("FLEET_OPERATION_KEY")
        .ok()
        .filter(|key| !key.is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mut client = Client::connect(selected_host)?;
    // Argumentos tipados opcionales: name=value[,value] ...
    let arguments = args[2..]
        .iter()
        .filter_map(|item| item.split_once('='))
        .map(|(name, values)| fleet_domain::ApprovalArgument {
            name: name.to_string(),
            values: values.split(',').map(str::to_string).collect(),
        })
        .collect();
    match client.call(Request::AnswerApproval {
        approval_id,
        decision,
        operation_key,
        arguments,
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

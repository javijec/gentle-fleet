//! Fleet CLI P1: cliente delgado de la API compartida; no gestiona procesos.

use std::fs::{self, OpenOptions};
use std::io::{BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::sync::Mutex;

use fleet_domain::EventKind;
use fleet_protocol::transport::{connect, expected_token};
use fleet_protocol::{read_frame, write_frame, Frame, Request, Response};
use fleet_ssh::{
    find_host, load_hosts, save_hosts, FleetReader, FleetWriter, HostConfig, SshStream,
};

static SELECTED_HOST: Mutex<Option<String>> = Mutex::new(None);

fn selected_host() -> Option<String> {
    SELECTED_HOST.lock().expect("host lock poisoned").clone()
}

/// Keep an operation key durably on the CLIENT before sending any remote
/// effect. SSH can fail after the daemon accepts a request but before its ACK.
fn operation_journal_path() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("FLEET_OPERATION_JOURNAL") {
        return Ok(PathBuf::from(path));
    }
    let root = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .ok_or("HOME or XDG_STATE_HOME required for durable remote operations")?;
    Ok(root.join("gentle-fleet/operations.jsonl"))
}

fn persist_remote_operation(kind: &str, target: &str, key: &str) -> Result<(), String> {
    let Some(host) = selected_host() else {
        return Ok(());
    };
    let path = operation_journal_path()?;
    let parent = path
        .parent()
        .ok_or("operation journal needs a parent directory")?;
    fs::create_dir_all(parent).map_err(|e| format!("cannot create operation journal: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Restrict our own Fleet state directory; do not chmod an arbitrary
        // override directory such as /tmp.
        if std::env::var_os("FLEET_OPERATION_JOURNAL").is_none() {
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("cannot protect operation journal directory: {e}"))?;
        }
    }
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err("operation journal is not a regular file".into());
        }
    }
    let record = serde_json::json!({
        "host": host,
        "kind": kind,
        "target": target,
        "operation_key": key,
        "created_at_unix_ms": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default().as_millis(),
    });
    let encoded = serde_json::to_vec(&record).map_err(|e| e.to_string())?;
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&path)
        .map_err(|e| format!("cannot open operation journal: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if file
            .metadata()
            .map_err(|e| e.to_string())?
            .permissions()
            .mode()
            & 0o077
            != 0
        {
            return Err("operation journal permissions are too permissive".into());
        }
    }
    file.write_all(&encoded)
        .and_then(|_| file.write_all(b"\n"))
        .and_then(|_| file.sync_data())
        .map_err(|e| format!("cannot durably record remote operation: {e}"))?;
    eprintln!(
        "remote operation_key={key} host={host} (journal={})",
        path.display()
    );
    Ok(())
}

fn show_pending() -> Result<(), String> {
    let path = operation_journal_path()?;
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("cannot read operation journal: {e}")),
    };
    for line in content.lines() {
        let record: serde_json::Value =
            serde_json::from_str(line).map_err(|e| format!("invalid operation journal: {e}"))?;
        let host = record["host"].as_str().unwrap_or("");
        if selected_host()
            .as_deref()
            .is_some_and(|selected| selected != host)
        {
            continue;
        }
        println!(
            "{}\t{}\t{}\t{}",
            host,
            record["kind"].as_str().unwrap_or(""),
            record["target"].as_str().unwrap_or(""),
            record["operation_key"].as_str().unwrap_or("")
        );
    }
    Ok(())
}

struct Client {
    reader: BufReader<FleetReader>,
    writer: BufWriter<FleetWriter>,
    next_id: u64,
    remote: bool,
}

impl Client {
    fn connect(host: Option<&str>) -> Result<Self, String> {
        let (reader_stream, writer_stream, remote) = match host {
            None => {
                let local = connect()
                    .map_err(|e| format!("cannot connect to local fleetd (is it running?): {e}"))?;
                let reader = local
                    .try_clone()
                    .map_err(|e| format!("cannot clone local stream: {e}"))?;
                (FleetReader::Local(reader), FleetWriter::Local(local), false)
            }
            Some(alias) => {
                let config = find_host(alias).map_err(|error| error.to_string())?;
                let ssh = SshStream::connect(&config).map_err(|error| error.to_string())?;
                let (reader, writer) = ssh.split().map_err(|error| error.to_string())?;
                (reader, writer, true)
            }
        };
        Ok(Self {
            reader: BufReader::new(reader_stream),
            writer: BufWriter::new(writer_stream),
            next_id: 0,
            remote,
        })
    }

    fn hello(&self) -> Request {
        Request::Hello {
            client_id: "fleetctl".into(),
            // El token local nunca viaja por SSH: el bridge lo inyecta en el host remoto.
            token: if self.remote {
                None
            } else {
                expected_token().ok().flatten()
            },
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
        match read_frame(&mut self.reader).map_err(|e| e.to_string())? {
            Some(frame) => Ok(frame),
            None => {
                if !self.remote {
                    return Err("fleetd closed the connection".into());
                }
                let code = self.writer.get_mut().ssh_exit_code();
                let diagnostics = self.writer.get_ref().ssh_diagnostics();
                let context = diagnostics.join(" | ");
                let lower = context.to_ascii_lowercase();
                // OpenSSH uses 255 for transport/auth/host-key failures;
                // exit codes 5 and 6 are not reliable SSH classifications.
                if lower.contains("host key verification failed")
                    || lower.contains("remote host identification has changed")
                    || lower.contains("no matching host key")
                {
                    Err("HostKeyRejected: SSH server identity verification failed".into())
                } else if lower.contains("permission denied (publickey")
                    || lower.contains("permission denied (")
                {
                    Err("AuthFailed: SSH authentication rejected".into())
                } else if lower.contains("fleetd unavailable") {
                    Err("DaemonDown: remote fleetd is unavailable".into())
                } else if code == Some(127)
                    || lower.contains("fleet-ssh-bridge: not found")
                    || lower.contains("fleet-ssh-bridge: no such file")
                {
                    Err("BridgeMissing: remote bridge executable not found".into())
                } else if lower.contains("connection refused") {
                    Err("SSHUnreachable: connection refused".into())
                } else if lower.contains("connection timed out")
                    || lower.contains("operation timed out")
                {
                    Err("SSHTimeout: connection timed out".into())
                } else if lower.contains("could not resolve hostname") {
                    Err("SSHUnreachable: name resolution failed".into())
                } else {
                    // stderr may contain local paths, account names or even
                    // unexpected secrets; classify it without echoing it.
                    Err(format!("SSHDisconnected: SSH exited with status {code:?}"))
                }
            }
        }
    }

    fn call(&mut self, request: Request) -> Result<Response, String> {
        let request_id = self.send(request)?;
        loop {
            match self.read_frame()? {
                Frame::Response {
                    request_id: id,
                    result,
                } if id == request_id => {
                    let response = result.map_err(|e| format!("{}: {}", e.code, e.message))?;
                    if let Response::Hello { wire, .. } = &response {
                        if wire != fleet_protocol::WIRE {
                            return Err(format!(
                                "IncompatibleProtocol: expected {}, server reports {wire}",
                                fleet_protocol::WIRE
                            ));
                        }
                    }
                    return Ok(response);
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
    let all_args: Vec<String> = std::env::args().skip(1).collect();
    let mut args = all_args.clone();
    if args.first().map(String::as_str) == Some("--host") {
        let alias = args
            .get(1)
            .ok_or_else(|| "--host requires an alias".to_string())?
            .clone();
        // Los comandos de gestión de hosts no necesitan conexión.
        if args.get(2).map(String::as_str) != Some("host") {
            find_host(&alias).map_err(|error| error.to_string())?;
        }
        *SELECTED_HOST.lock().expect("host lock poisoned") = Some(alias);
        args = args[2..].to_vec();
    }
    let Some(command) = args.first() else {
        return Err(usage());
    };
    if command == "host" {
        return host_command(&args[1..]);
    }
    if command == "pending" {
        return show_pending();
    }
    match command.as_str() {
        "workspace" => workspace(&args[1..]),
        "session" => session(&args[1..]),
        "prompt" => prompt(&args[1..]),
        "interrupt" => interrupt(&args[1..]),
        "controls" => controls(&args[1..]),
        "set-control" => set_control(&args[1..]),
        "native-sessions" => native_sessions(&args[1..]),
        "import-history" => import_history(&args[1..]),
        "watch" => watch(&args[1..]),
        "stop" => stop(&args[1..]),
        "timeline" => timeline(&args[1..]),
        "operation" => operation(&args[1..]),
        "approvals" => approvals(&args[1..]),
        "approval" => approval(&args[1..]),
        "answer" => answer(&args[1..]),
        "agents" => {
            let mut client = Client::connect(selected_host().as_deref())?;
            client.call(client.hello())?;
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
            let mut hosts = load_hosts().map_err(|error| error.to_string())?;
            if hosts.iter().any(|host| host.alias == alias) {
                return Err(format!("host `{alias}` already exists"));
            }
            hosts.push(HostConfig {
                alias: alias.clone(),
                destination,
                bridge_path: bridge.unwrap_or_else(|| "/usr/local/bin/fleet-ssh-bridge".into()),
                port,
                identity_file: identity,
                expected_host_key: None,
                last_status: None,
                protocol_version: None,
            });
            save_hosts(&hosts).map_err(|error| error.to_string())?;
            println!("added\t{alias}");
            Ok(())
        }
        Some("remove") => {
            let alias = args
                .get(1)
                .ok_or_else(|| "host remove <alias>".to_string())?;
            let hosts: Vec<HostConfig> = load_hosts()
                .map_err(|error| error.to_string())?
                .into_iter()
                .filter(|host| &host.alias != alias)
                .collect();
            save_hosts(&hosts).map_err(|error| error.to_string())?;
            println!("removed\t{alias}");
            Ok(())
        }
        Some("check") => {
            let alias = args
                .get(1)
                .ok_or_else(|| "host check <alias>".to_string())?;
            let mut client = Client::connect(Some(alias))?;
            match client.call(client.hello())? {
                Response::Hello { wire, .. } => {
                    println!("connected\t{alias}\t{wire}");
                    Ok(())
                }
                other => Err(format!("unexpected Hello response: {other:?}")),
            }
        }
        _ => Err("usage: fleetctl host add|list|show|remove|check ...".into()),
    }
}

fn usage() -> String {
    "usage:\n  fleetctl workspace add <path>\n  fleetctl workspace list\n  fleetctl session create --workspace <id> --provider <id>\n  fleetctl session list\n  fleetctl prompt <session_id> <text>\n  fleetctl interrupt <session_id> <turn_id>\n  fleetctl controls <session_id>\n  fleetctl set-control <session_id> <name> <value> [turn|session]\n  fleetctl native-sessions <provider_id>\n  fleetctl import-history <session_id> <provider_id> <native_ref> [operation_key]\n  fleetctl watch <session_id> [--since N] [--until-turn-end] [--count N]\n  fleetctl stop <session_id>\n  fleetctl timeline <session_id> [--since N] [--limit N]\n  fleetctl operation <operation_key>\n  fleetctl approvals [--session <id>]\n  fleetctl approval <approval_id>\n  fleetctl answer <approval_id> <decision> [name=value[,value] ...]\n  fleetctl agents\n  fleetctl [--host alias] pending  # list durable remote operation keys"
        .into()
}

fn workspace(args: &[String]) -> Result<(), String> {
    let mut client = Client::connect(selected_host().as_deref())?;
    client.call(client.hello())?;
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
    let mut client = Client::connect(selected_host().as_deref())?;
    client.call(client.hello())?;
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

fn native_sessions(args: &[String]) -> Result<(), String> {
    let provider_id = args.first().ok_or_else(usage)?.clone();
    let mut client = Client::connect(selected_host().as_deref())?;
    client.call(client.hello())?;
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

fn import_history(args: &[String]) -> Result<(), String> {
    let session_id = args.first().ok_or_else(usage)?.clone();
    let provider_id = args.get(1).ok_or_else(usage)?.clone();
    let native_ref = args.get(2).ok_or_else(usage)?.clone();
    let operation_key = args.get(3).cloned().unwrap_or_else(|| {
        std::env::var("FLEET_OPERATION_KEY").unwrap_or_else(|_| uuid::Uuid::new_v4().to_string())
    });
    let mut client = Client::connect(selected_host().as_deref())?;
    client.call(client.hello())?;
    persist_remote_operation("import_history", &session_id, &operation_key)?;
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

fn controls(args: &[String]) -> Result<(), String> {
    let session_id = args.first().ok_or_else(usage)?.clone();
    let mut client = Client::connect(selected_host().as_deref())?;
    client.call(client.hello())?;
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

fn set_control(args: &[String]) -> Result<(), String> {
    let session_id = args.first().ok_or_else(usage)?.clone();
    let name = args.get(1).ok_or_else(usage)?.clone();
    let value = args.get(2).ok_or_else(usage)?.clone();
    let scope = args.get(3).cloned().unwrap_or_else(|| "turn".into());
    let mut client = Client::connect(selected_host().as_deref())?;
    client.call(client.hello())?;
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

fn interrupt(args: &[String]) -> Result<(), String> {
    let session_id = args.first().ok_or_else(usage)?.clone();
    let turn_id = args.get(1).ok_or_else(usage)?.clone();
    let mut client = Client::connect(selected_host().as_deref())?;
    client.call(client.hello())?;
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
    let mut client = Client::connect(selected_host().as_deref())?;
    client.call(client.hello())?;
    persist_remote_operation("prompt", &session_id, &operation_key)?;
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
    let mut client = Client::connect(selected_host().as_deref())?;
    client.call(client.hello())?;
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
    let mut client = Client::connect(selected_host().as_deref())?;
    client.call(client.hello())?;
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
    let mut client = Client::connect(selected_host().as_deref())?;
    client.call(client.hello())?;
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
    let mut client = Client::connect(selected_host().as_deref())?;
    client.call(client.hello())?;
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
    let mut client = Client::connect(selected_host().as_deref())?;
    client.call(client.hello())?;
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
    let mut client = Client::connect(selected_host().as_deref())?;
    client.call(client.hello())?;
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
    let mut client = Client::connect(selected_host().as_deref())?;
    client.call(client.hello())?;
    // Argumentos tipados opcionales: name=value[,value] ...
    let arguments = args[2..]
        .iter()
        .filter_map(|item| item.split_once('='))
        .map(|(name, values)| fleet_domain::ApprovalArgument {
            name: name.to_string(),
            values: values.split(',').map(str::to_string).collect(),
        })
        .collect();
    persist_remote_operation("answer", &approval_id, &operation_key)?;
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

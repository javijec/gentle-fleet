use fleet_client::{FleetClient, Host};
use fleet_domain::EventKind;
use std::collections::BTreeSet;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let (host, workspace_path, reconnect) = match args.as_slice() {
        [workspace] => (Host::Local, workspace.clone(), false),
        [flag, alias, workspace] if flag == "--host" => {
            (Host::Ssh(alias.clone()), workspace.clone(), false)
        }
        [flag, alias, workspace] if flag == "--reconnect" => {
            (Host::Ssh(alias.clone()), workspace.clone(), true)
        }
        _ => {
            return Err("usage: sdk_smoke [--host alias|--reconnect alias] <workspace-path>".into())
        }
    };
    let mut client = FleetClient::connect(host)?;
    let provider = std::env::var("FLEET_SDK_PROVIDER").unwrap_or_else(|_| "fake".into());
    let prompt = std::env::var("FLEET_SDK_PROMPT").unwrap_or_else(|_| "SDK smoke".into());
    let agents = client.list_agents()?;
    assert!(agents.iter().any(|agent| agent.id == provider));
    let mut agent_ids = agents
        .iter()
        .map(|agent| agent.id.clone())
        .collect::<Vec<_>>();
    agent_ids.sort();
    for id in agent_ids {
        println!("SDK_AGENT\t{id}");
    }
    let workspace = client.register_workspace(workspace_path)?;
    let session = client.create_session(workspace.id, provider)?;
    println!("SDK_SESSION\t{}", session.id);
    let mut subscription = client.subscribe_timeline(session.id.clone(), 0)?;
    let operation_key = client.new_operation_key();
    client.send_prompt(session.id.clone(), prompt.clone(), operation_key)?;
    if reconnect {
        println!("SDK_PROMPT_SENT");
        std::io::Write::flush(&mut std::io::stdout())?;
    }
    let mut saw_user = false;
    let mut saw_terminal = false;
    let mut seen = BTreeSet::new();
    while !saw_terminal {
        let event = match subscription.recv_timeout(Duration::from_secs(25)) {
            Ok(event) => event,
            // A lost transport is surfaced once while its observer reconnects.
            Err(fleet_client::ClientError::Transport(_)) if reconnect => continue,
            Err(error) => return Err(error.into()),
        };
        assert!(seen.insert(event.seq), "duplicate SDK timeline sequence");
        if reconnect {
            println!("SDK_EVENT\t{}", event.seq);
            std::io::Write::flush(&mut std::io::stdout())?;
        }
        match event.kind {
            EventKind::UserMessage { text, .. } => saw_user |= text == prompt,
            EventKind::AssistantMessage { text, .. } => {
                assert!(!text.trim().is_empty(), "SDK assistant reply was empty");
            }
            EventKind::TurnCompleted { .. } => {
                saw_terminal = true;
                break;
            }
            _ => {}
        }
    }
    assert!(saw_user && saw_terminal, "SDK stream missed chat events");
    let last_sequence = subscription.last_sequence();
    subscription.cancel();
    let mut resumed = client.resume_timeline(session.id.clone())?;
    assert_eq!(
        resumed.last_sequence(),
        last_sequence,
        "resume did not use the persisted per-host/session cursor"
    );
    resumed.cancel();
    if reconnect {
        client = FleetClient::connect(client.host().clone())?;
    }
    assert!(client.stop_session(session.id)?);
    println!("PASS: fleet-client API, prompt, timeline and cancellation");
    Ok(())
}

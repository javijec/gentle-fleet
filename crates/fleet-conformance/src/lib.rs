//! Suite de conformidad multiproveedor parametrizada por capacidades.
//!
//! Un único conjunto de comprobaciones se ejecuta para cada proveedor real.
//! El estado de cada celda es explícito: PASS, FAIL, UNTESTED, UNSUPPORTED
//! o SKIP con justificación. Ninguna comprobación que no ejecuta aserciones
//! se reporta como PASS.

mod client;

use std::time::Duration;

use client::ApiClient;
use fleet_domain::{EventKind, ProviderDescriptor};
use fleet_protocol::Response;
use serde::Serialize;

pub const REAL_PROVIDERS: [&str; 4] = ["opencode", "codex", "pi", "gentle-shell"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Status {
    Pass,
    Fail,
    Untested,
    Unsupported,
    Skip,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub provider: String,
    pub check: String,
    pub status: Status,
    pub detail: String,
}

impl CheckResult {
    fn new(provider: &str, check: &str, status: Status, detail: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            check: check.into(),
            status,
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    pub real: bool,
    pub timeout: Duration,
    /// Workspace explicitamente registrado para la ejecución. Nunca usar
    /// el primer workspace de un daemon potencialmente personal.
    pub workspace_id: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            real: false,
            timeout: Duration::from_secs(120),
            workspace_id: None,
        }
    }
}

fn descriptor_for(client: &mut ApiClient, provider: &str) -> Result<ProviderDescriptor, String> {
    let agents = match client.call(fleet_protocol::Request::ListAgents)? {
        Response::Agents { agents } => agents,
        _ => return Err("unexpected ListAgents response".into()),
    };
    let agent = agents
        .into_iter()
        .find(|agent| agent.id == provider)
        .ok_or_else(|| format!("provider `{provider}` is not registered"))?;
    Ok(ProviderDescriptor {
        id: agent.id,
        version: fleet_domain::ProviderVersion::parse(
            agent.version.unwrap_or_else(|| "0.0.0".into()),
        ),
        capabilities: agent.capabilities,
        mode: match agent.mode.as_str() {
            "Attached" => fleet_domain::RunnerMode::Attached,
            "Auto" => fleet_domain::RunnerMode::Auto,
            _ => fleet_domain::RunnerMode::Owned,
        },
        availability: match agent.availability.as_str() {
            "Available" => fleet_domain::ProviderAvailability::Available,
            "Unavailable" => fleet_domain::ProviderAvailability::Unavailable,
            _ => fleet_domain::ProviderAvailability::Unknown,
        },
        controls: agent.controls,
    })
}

fn ok_or_fail(provider: &str, check: &str, result: Result<(), String>) -> CheckResult {
    match result {
        Ok(()) => CheckResult::new(provider, check, Status::Pass, "assertions passed"),
        Err(error) => CheckResult::new(provider, check, Status::Fail, error),
    }
}

/// Ejecución común sin excepciones ocultas por nombre de proveedor.
pub fn run_provider(provider: &str, options: &Options) -> Vec<CheckResult> {
    use fleet_protocol::Request;

    let mut results = Vec::new();
    let mut client = match ApiClient::connect() {
        Ok(client) => client,
        Err(error) => {
            results.push(CheckResult::new(provider, "api_connection", Status::Fail, error));
            return results;
        }
    };

    let descriptor = match descriptor_for(&mut client, provider) {
        Ok(descriptor) => descriptor,
        Err(error) => {
            results.push(CheckResult::new(provider, "agent_registered", Status::Fail, error));
            return results;
        }
    };
    let available = descriptor.availability == fleet_domain::ProviderAvailability::Available;
    results.push(CheckResult::new(
        provider, "agent_registered",
        if available { Status::Pass } else { Status::Unsupported },
        format!("id={} version={} availability={:?}",
            descriptor.id, descriptor.version.text, descriptor.availability),
    ));
    let valid_capabilities = descriptor.capabilities.streaming_text
        && (descriptor.capabilities.can_own_process
            || descriptor.capabilities.can_attach_service
            || provider == "fake");
    results.push(ok_or_fail(
        provider, "capability_contract",
        if valid_capabilities { Ok(()) }
        else { Err("provider does not declare a usable streaming transport".into()) },
    ));

    let Some(workspace_id) = options.workspace_id.as_ref() else {
        results.push(CheckResult::new(
            provider, "isolated_workspace",
            Status::Fail,
            "explicit --workspace ID required; refusing to touch an arbitrary existing project",
        ));
        return results;
    };
    let workspace_ok = match client.call(Request::ListWorkspaces) {
        Ok(Response::Workspaces { workspaces }) =>
            workspaces.iter().any(|workspace| workspace.id == *workspace_id),
        _ => false,
    };
    if !workspace_ok {
        results.push(CheckResult::new(
            provider, "isolated_workspace", Status::Fail,
            "the explicitly selected workspace is not registered",
        ));
        return results;
    }
    results.push(CheckResult::new(
        provider, "isolated_workspace", Status::Pass, "explicit registered workspace",
    ));

    let session_id = match client.call(Request::CreateSession {
        workspace_id: workspace_id.clone(),
        provider_id: provider.into(),
    }) {
        Ok(Response::SessionCreated { session }) => session.id,
        other => {
            results.push(CheckResult::new(
                provider, "session_creation", Status::Fail,
                format!("CreateSession failed: {other:?}"),
            ));
            return results;
        }
    };

    let lifecycle = (|| -> Result<(), String> {
        let sessions = match client.call(Request::ListSessions)? {
            Response::Sessions { sessions } => sessions,
            other => return Err(format!("unexpected ListSessions: {other:?}")),
        };
        if !sessions.iter().any(|session| session.id == session_id && session.provider_id == provider) {
            return Err("session not listed with correct provider ID".into());
        }
        Ok(())
    })();
    results.push(ok_or_fail(provider, "session_lifecycle", lifecycle));

    let timeline = (|| -> Result<(), String> {
        let events = client.timeline_all(&session_id)?;
        if !events.iter().any(|event| matches!(event.kind, EventKind::SessionCreated { .. })) {
            return Err("missing SessionCreated in durable timeline".into());
        }
        Ok(())
    })();
    results.push(ok_or_fail(provider, "timeline_integrity", timeline));

    let two_clients = (|| -> Result<(), String> {
        let mut a = ApiClient::connect()?;
        let mut b = ApiClient::connect()?;
        let first = a.subscribe_snapshot(&session_id)?;
        let second = b.subscribe_snapshot(&session_id)?;
        if first.is_empty() || second.is_empty() || first != second {
            return Err("independent client snapshots differ or are empty".into());
        }
        Ok(())
    })();
    results.push(ok_or_fail(provider, "two_client_replay", two_clients));

    let controls = match client.call(Request::ListControls {
        session_id: session_id.clone(),
    }) {
        Ok(Response::Controls { controls }) => controls,
        other => {
            results.push(CheckResult::new(
                provider, "control_discovery", Status::Fail,
                format!("ListControls failed: {other:?}"),
            ));
            Vec::new()
        }
    };
    results.push(CheckResult::new(
        provider, "control_discovery",
        if controls.is_empty() { Status::Unsupported } else { Status::Pass },
        format!("controls={} models={}",
            controls.len(),
            controls.iter().find(|c| c.name == "model")
                .map(|c| c.available.len()).unwrap_or(0)),
    ));
    if let Some(model) = controls.iter().find(|c| c.name == "model" && c.mutable) {
        let invalid = match client.call(Request::SetControl {
            session_id: session_id.clone(),
            name: model.name.clone(),
            value: "__conformance_invalid_model__".into(),
            scope: "turn".into(),
        }) {
            Err(error) if error.starts_with("InvalidArgument:")
                || error.starts_with("invalid_argument:") => Ok(()),
            other => Err(format!("invalid control was not rejected: {other:?}")),
        };
        results.push(ok_or_fail(provider, "control_invalid_rejected", invalid));
    } else {
        results.push(CheckResult::new(
            provider, "control_invalid_rejected", Status::Unsupported,
            "no mutable native model control",
        ));
    }

    let before = match client.call(Request::ListSessions) {
        Ok(Response::Sessions { sessions }) => sessions.len(),
        _ => usize::MAX,
    };
    let discovery = match client.call(Request::ListNativeSessions {
        provider_id: provider.into(),
    }) {
        Ok(Response::NativeSessions { sessions }) => {
            let after = match client.call(Request::ListSessions) {
                Ok(Response::Sessions { sessions }) => sessions.len(),
                _ => usize::MAX - 1,
            };
            if after != before {
                CheckResult::new(provider, "native_discovery_readonly", Status::Fail,
                    "read-only discovery changed Fleet sessions")
            } else if sessions.is_empty() {
                CheckResult::new(provider, "native_discovery_readonly", Status::Untested,
                    "empty native list cannot prove discovery support")
            } else {
                CheckResult::new(provider, "native_discovery_readonly", Status::Pass,
                    format!("native_sessions={}", sessions.len()))
            }
        }
        Err(error) if error.starts_with("unsupported_capability:")
            || error.contains("sessions_unavailable") => {
            CheckResult::new(provider, "native_discovery_readonly", Status::Unsupported,
                "native session listing unavailable in this mode")
        }
        other => CheckResult::new(provider, "native_discovery_readonly", Status::Fail,
            format!("ListNativeSessions failed: {other:?}")),
    };
    results.push(discovery);

    let no_autoapproval = match client.call(Request::GetApproval {
        approval_id: "conformance-nonexistent".into(),
    }) {
        Err(error) if error.starts_with("not_found:") => Ok(()),
        other => Err(format!("unknown approval must fail: {other:?}")),
    };
    results.push(ok_or_fail(provider, "no_autoapproval", no_autoapproval));
    results.push(CheckResult::new(provider, "approval_contract",
        if descriptor.capabilities.approvals { Status::Untested } else { Status::Unsupported },
        if descriptor.capabilities.approvals {
            "native approval response requires a separate interactive fixture"
        } else {
            "driver does not expose approvals"
        },
    ));

    if options.real {
        results.push(run_real_chat(&mut client, provider, &session_id, options.timeout));
    } else {
        results.push(CheckResult::new(provider, "real_chat", Status::Skip,
            "requires explicit --real with paid-model opt-in"));
    }

    let stop = (|| -> Result<(), String> {
        match client.call(Request::StopSession { session_id: session_id.clone() })? {
            Response::Stopped { session_id: id } if id == session_id => {}
            other => return Err(format!("StopSession mismatch: {other:?}")),
        }
        match client.call(Request::StopSession { session_id: session_id.clone() }) {
            Err(error) if error.starts_with("session_stopped:") => Ok(()),
            other => Err(format!("repeated stop should fail explicitly: {other:?}")),
        }
    })();
    results.push(ok_or_fail(provider, "stop_idempotence", stop));
    results
}

fn verify_real_chat_events(
    events: &[fleet_domain::Event], turn_id: &str, token: &str,
    idempotent: bool, conflict_ok: bool,
) -> Result<(), String> {
    use fleet_domain::TurnStatus;
    let users = events.iter().filter(|event| matches!(
        &event.kind, EventKind::UserMessage { turn_id: id, .. } if id == turn_id
    )).count();
    let started = events.iter().filter(|event| matches!(
        &event.kind, EventKind::TurnStarted { turn_id: id } if id == turn_id
    )).count();
    let terminals: Vec<&EventKind> = events.iter().filter_map(|event| match &event.kind {
        EventKind::TurnCompleted { turn_id: id, .. } | EventKind::TurnUncertain { turn_id: id, .. }
            if id == turn_id => Some(&event.kind),
        _ => None,
    }).collect();
    let terminal_ok = matches!(terminals.as_slice(),
        [EventKind::TurnCompleted { status: TurnStatus::Completed, .. }]);
    let deltas: String = events.iter().filter_map(|event| match &event.kind {
        EventKind::AssistantDelta { turn_id: id, text } if id == turn_id => Some(text.as_str()),
        _ => None,
    }).collect();
    let messages = events.iter().filter_map(|event| match &event.kind {
        EventKind::AssistantMessage { turn_id: id, text } if id == turn_id => Some(text.as_str()),
        _ => None,
    }).collect::<Vec<_>>().join("");
    if users != 1 || started != 1 || !terminal_ok || !idempotent || !conflict_ok
        || !(deltas.contains(token) || messages.contains(token))
    {
        return Err(format!("real turn contract violated: user={users} started={started} \
            terminal_ok={terminal_ok} idempotent={idempotent} conflict={conflict_ok} \
            text_seen={}", deltas.contains(token) || messages.contains(token)));
    }
    Ok(())
}

fn run_real_chat(
    client: &mut ApiClient, provider: &str, session_id: &str, timeout: Duration,
) -> CheckResult {
    use fleet_protocol::Request;
    let token = format!("CONFORMITY {provider} OK");
    let operation_key = format!("conformance-{provider}-{}-{session_id}", std::process::id());
    let prompt = client.call(Request::SendPrompt {
        session_id: session_id.into(),
        text: format!("Reply with exactly: {token}"),
        operation_key: operation_key.clone(),
    });
    let (operation_id, turn_id) = match prompt {
        Ok(Response::PromptAccepted { operation_id, turn_id, .. }) => (operation_id, turn_id),
        other => {
            // Un --real explícito requiere funcionalidad real. Nunca degradar
            // un fallo del modelo a UNTESTED por substring del mensaje.
            return CheckResult::new(provider, "real_chat", Status::Fail,
                format!("SendPrompt failed: {other:?}"));
        }
    };
    let duplicate = client.call(Request::SendPrompt {
        session_id: session_id.into(),
        text: format!("Reply with exactly: {token}"),
        operation_key: operation_key.clone(),
    });
    let idempotent = matches!(
        &duplicate, Ok(Response::PromptAccepted { turn_id: id, .. }) if id == &turn_id
    );
    let conflict = client.call(Request::SendPrompt {
        session_id: session_id.into(),
        text: "different payload".into(),
        operation_key,
    });
    let conflict_ok = matches!(
        &conflict, Err(error) if error.starts_with("Conflict:")
            || error.starts_with("operation_conflict:")
            || error.starts_with("conflict:")
    );
    let outcome = client.wait_for_terminal(session_id, &turn_id, timeout)
        .and_then(|events| verify_real_chat_events(&events, &turn_id, &token, idempotent, conflict_ok));
    match outcome {
        Ok(()) => CheckResult::new(provider, "real_chat", Status::Pass,
            format!("op={operation_id} turn={turn_id}; terminal Completed, once-only and text verified")),
        Err(error) => CheckResult::new(provider, "real_chat", Status::Fail, error),
    }
}

pub fn run_all(providers: &[String], options: &Options) -> Vec<CheckResult> {
    if providers.is_empty() {
        return vec![CheckResult::new("*", "providers", Status::Fail,
            "no providers selected")];
    }
    let mut results = Vec::new();
    for provider in providers {
        results.extend(run_provider(provider, options));
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use fleet_domain::{Event, TurnStatus};

    fn event(seq: u64, kind: EventKind) -> Event {
        Event { seq, event_id: format!("event-{seq}"), session_id: "session".into(),
            created_at_ms: 0, kind }
    }

    #[test]
    fn real_check_rejects_failed_interrupted_or_wrong_turn_terminals() {
        let mut events = vec![
            event(1, EventKind::TurnStarted { turn_id: "expected".into() }),
            event(2, EventKind::UserMessage { turn_id: "expected".into(), text: "hi".into() }),
            event(3, EventKind::AssistantMessage { turn_id: "expected".into(), text: "TOKEN".into() }),
            event(4, EventKind::TurnCompleted { turn_id: "other".into(), status: TurnStatus::Completed }),
            event(5, EventKind::TurnCompleted { turn_id: "expected".into(), status: TurnStatus::Failed }),
        ];
        assert!(verify_real_chat_events(&events, "expected", "TOKEN", true, true).is_err());
        events[4].kind = EventKind::TurnCompleted {
            turn_id: "expected".into(), status: TurnStatus::Completed
        };
        assert!(verify_real_chat_events(&events, "expected", "TOKEN", true, true).is_ok());
        assert!(verify_real_chat_events(&events, "expected", "TOKEN", false, true).is_err());
        events.push(event(6, EventKind::TurnUncertain {
            turn_id: "expected".into(), reason: "ambiguous".into()
        }));
        assert!(verify_real_chat_events(&events, "expected", "TOKEN", true, true).is_err());
    }

    #[test]
    fn no_providers_never_pass() {
        assert_eq!(run_all(&[], &Options::default())[0].status, Status::Fail);
    }
}

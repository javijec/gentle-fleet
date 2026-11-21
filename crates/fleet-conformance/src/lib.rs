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
}

impl Default for Options {
    fn default() -> Self {
        Self {
            real: false,
            timeout: Duration::from_secs(120),
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

/// Ejecuta todas las comprobaciones aplicables a un proveedor.
pub fn run_provider(provider: &str, options: &Options) -> Vec<CheckResult> {
    let mut results = Vec::new();
    let mut client = match ApiClient::connect() {
        Ok(client) => client,
        Err(error) => {
            results.push(CheckResult::new(
                provider,
                "api_connection",
                Status::Fail,
                error,
            ));
            return results;
        }
    };

    let descriptor = match descriptor_for(&mut client, provider) {
        Ok(descriptor) => descriptor,
        Err(error) => {
            results.push(CheckResult::new(
                provider,
                "agent_registered",
                Status::Fail,
                error,
            ));
            return results;
        }
    };
    results.push(CheckResult::new(
        provider,
        "agent_registered",
        if descriptor.availability == fleet_domain::ProviderAvailability::Available {
            Status::Pass
        } else {
            Status::Unsupported
        },
        format!(
            "id={} version={} availability={:?}",
            descriptor.id, descriptor.version.text, descriptor.availability
        ),
    ));

    results.push(ok_or_fail(
        provider,
        "capability_contract",
        if descriptor.capabilities.streaming_text && descriptor.capabilities.can_own_process {
            Ok(())
        } else {
            Err("streaming_text or can_own_process not declared".into())
        },
    ));

    // Ciclo de vida y timeline: mismo contrato para los cuatro proveedores.
    let workspaces = match client.call(fleet_protocol::Request::ListWorkspaces) {
        Ok(Response::Workspaces { workspaces }) => workspaces,
        _ => Vec::new(),
    };
    let Some(workspace_id) = workspaces.first().map(|workspace| workspace.id.clone()) else {
        results.push(CheckResult::new(
            provider,
            "session_lifecycle",
            Status::Skip,
            "no workspace registered in this daemon",
        ));
        return results;
    };
    let session_id = match client.call(fleet_protocol::Request::CreateSession {
        workspace_id,
        provider_id: provider.into(),
    }) {
        Ok(Response::SessionCreated { session }) => session.id,
        other => {
            results.push(CheckResult::new(
                provider,
                "session_lifecycle",
                Status::Fail,
                format!("CreateSession failed: {other:?}"),
            ));
            return results;
        }
    };

    let lifecycle = (|| -> Result<(), String> {
        let sessions = match client.call(fleet_protocol::Request::ListSessions)? {
            Response::Sessions { sessions } => sessions,
            _ => return Err("unexpected ListSessions response".into()),
        };
        if !sessions.iter().any(|session| session.id == session_id) {
            return Err("session was not listed after creation".into());
        }
        match client.call(fleet_protocol::Request::StopSession {
            session_id: session_id.clone(),
        })? {
            Response::Stopped { .. } => {}
            other => return Err(format!("StopSession failed: {other:?}")),
        }
        match client.call(fleet_protocol::Request::StopSession {
            session_id: session_id.clone(),
        }) {
            Err(error) if error.contains("session_stopped") => {}
            other => return Err(format!("second StopSession should fail: {other:?}")),
        }
        Ok(())
    })();
    results.push(ok_or_fail(provider, "session_lifecycle", lifecycle));

    let timeline = (|| -> Result<(), String> {
        let events = match client.call(fleet_protocol::Request::ReadTimeline {
            session_id: session_id.clone(),
            since_seq: 0,
            limit: 256,
        })? {
            Response::Timeline { events, .. } => events,
            _ => return Err("unexpected ReadTimeline response".into()),
        };
        for (expected, event) in (1u64..).zip(events.iter()) {
            if event.seq != expected {
                return Err(format!(
                    "non-contiguous sequence: expected {expected}, got {}",
                    event.seq
                ));
            }
        }
        if !events
            .iter()
            .any(|event| matches!(event.kind, EventKind::SessionCreated { .. }))
        {
            return Err("missing SessionCreated".into());
        }
        Ok(())
    })();
    results.push(ok_or_fail(provider, "timeline_integrity", timeline));

    let subscribers = (|| -> Result<(), String> {
        for _ in 0..2 {
            match client.call(fleet_protocol::Request::SubscribeTimeline {
                session_id: session_id.clone(),
                since_seq: 0,
            })? {
                Response::Subscribed { .. } => {}
                other => return Err(format!("SubscribeTimeline failed: {other:?}")),
            }
        }
        Ok(())
    })();
    results.push(ok_or_fail(provider, "two_subscribers", subscribers));

    let controls = match client.call(fleet_protocol::Request::ListControls {
        session_id: session_id.clone(),
    }) {
        Ok(Response::Controls { controls }) => controls,
        other => {
            results.push(CheckResult::new(
                provider,
                "control_discovery",
                Status::Fail,
                format!("ListControls failed: {other:?}"),
            ));
            Vec::new()
        }
    };
    let models = controls
        .iter()
        .find(|control| control.name == "model")
        .map(|control| control.available.clone())
        .unwrap_or_default();
    let status = if controls.is_empty() {
        Status::Unsupported
    } else if models.is_empty() {
        Status::Untested
    } else {
        Status::Pass
    };
    results.push(CheckResult::new(
        provider,
        "control_discovery",
        status,
        format!("controls={} models={}", controls.len(), models.len()),
    ));

    let before = match client.call(fleet_protocol::Request::ListSessions) {
        Ok(Response::Sessions { sessions }) => sessions.len(),
        _ => 0,
    };
    let discovery = match client.call(fleet_protocol::Request::ListNativeSessions {
        provider_id: provider.into(),
    }) {
        Ok(Response::NativeSessions { sessions }) => {
            let after = match client.call(fleet_protocol::Request::ListSessions) {
                Ok(Response::Sessions { sessions }) => sessions.len(),
                _ => before,
            };
            if after != before {
                Err("read-only discovery changed Fleet sessions".into())
            } else {
                Ok(format!("native_sessions={}", sessions.len()))
            }
        }
        other => Err(format!("ListNativeSessions failed: {other:?}")),
    };
    results.push(match discovery {
        Ok(detail) => CheckResult::new(provider, "native_discovery_readonly", Status::Pass, detail),
        Err(error) if error.contains("sessions_unavailable") => CheckResult::new(
            provider,
            "native_discovery_readonly",
            Status::Unsupported,
            "native provider reports sessions_unavailable in this mode",
        ),
        Err(error) => CheckResult::new(provider, "native_discovery_readonly", Status::Fail, error),
    });

    let no_autoapproval = match client.call(fleet_protocol::Request::GetApproval {
        approval_id: "conformance-nonexistent".into(),
    }) {
        Err(error) if error.contains("not_found") => Ok(()),
        other => Err(format!("unknown approval must fail: {other:?}")),
    };
    results.push(ok_or_fail(provider, "no_autoapproval", no_autoapproval));

    let approvals = if descriptor.capabilities.approvals {
        CheckResult::new(
            provider,
            "approval_contract",
            Status::Untested,
            "native approval requires an interactive request; covered by driver E2E",
        )
    } else {
        CheckResult::new(
            provider,
            "approval_contract",
            Status::Unsupported,
            "driver does not declare approvals",
        )
    };
    results.push(approvals);

    if controls.is_empty() {
        results.push(CheckResult::new(
            provider,
            "control_invalid_rejected",
            Status::Unsupported,
            "provider exposes no controls",
        ));
    } else {
        let invalid = match client.call(fleet_protocol::Request::SetControl {
            session_id: session_id.clone(),
            name: "model".into(),
            value: "__not_a_model__".into(),
            scope: "turn".into(),
        }) {
            Err(error) if error.contains("InvalidArgument") => Ok(()),
            other => Err(format!("invalid control accepted: {other:?}")),
        };
        results.push(ok_or_fail(provider, "control_invalid_rejected", invalid));
    }

    if options.real {
        results.push(run_real_chat(&mut client, provider, options.timeout));
    } else {
        results.push(CheckResult::new(
            provider,
            "real_chat",
            Status::Skip,
            "GF-style opt-in real checks disabled",
        ));
    }

    results
}

fn run_real_chat(client: &mut ApiClient, provider: &str, timeout: Duration) -> CheckResult {
    let workspace_id = match client.call(fleet_protocol::Request::ListWorkspaces) {
        Ok(Response::Workspaces { workspaces }) => workspaces
            .first()
            .map(|workspace| workspace.id.clone())
            .unwrap_or_default(),
        _ => String::new(),
    };
    let session_id = match client.call(fleet_protocol::Request::CreateSession {
        workspace_id,
        provider_id: provider.into(),
    }) {
        Ok(Response::SessionCreated { session }) => session.id,
        other => {
            return CheckResult::new(
                provider,
                "real_chat",
                Status::Fail,
                format!("CreateSession failed: {other:?}"),
            )
        }
    };
    let token = format!("CONFORMITY {provider} OK");
    let operation_key = format!("conformance-{provider}-{}", std::process::id());
    let prompt = client.call(fleet_protocol::Request::SendPrompt {
        session_id: session_id.clone(),
        text: format!("Reply with exactly: {token}"),
        operation_key: operation_key.clone(),
    });
    let prompt_receipt = match prompt {
        Ok(receipt) => receipt,
        Err(error) => {
            let lower = error.to_lowercase();
            let status = if lower.contains("model") || lower.contains("unsupported") {
                Status::Untested
            } else {
                Status::Fail
            };
            return CheckResult::new(
                provider,
                "real_chat",
                status,
                format!("SendPrompt failed: {error}"),
            );
        }
    };
    let (op_id, first_turn) = match prompt_receipt {
        Response::PromptAccepted {
            operation_id,
            turn_id,
            ..
        } => (operation_id, turn_id),
        _ => {
            return CheckResult::new(
                provider,
                "real_chat",
                Status::Fail,
                "unexpected prompt receipt",
            )
        }
    };
    // Misma clave y mismo payload: no debe despachar un segundo prompt.
    let duplicate = client.call(fleet_protocol::Request::SendPrompt {
        session_id: session_id.clone(),
        text: format!("Reply with exactly: {token}"),
        operation_key: operation_key.clone(),
    });
    let idempotent = matches!(
        &duplicate,
        Ok(Response::PromptAccepted { turn_id, .. }) if turn_id == &first_turn
    );
    let conflict = client.call(fleet_protocol::Request::SendPrompt {
        session_id: session_id.clone(),
        text: "different payload".into(),
        operation_key,
    });
    let conflict_ok = matches!(&conflict, Err(error) if error.contains("Conflict"));
    let events = match client.wait_for_terminal(&session_id, timeout) {
        Ok(events) => events,
        Err(error) => {
            return CheckResult::new(provider, "real_chat", Status::Fail, error);
        }
    };
    let users = events
        .iter()
        .filter(|event| matches!(event.kind, EventKind::UserMessage { .. }))
        .count();
    let terminal = events.iter().find(|event| {
        matches!(
            event.kind,
            EventKind::TurnCompleted { .. } | EventKind::TurnUncertain { .. }
        )
    });
    // Un token puede llegar repartido en varios deltas: se concatena el texto
    // observado antes de comprobarlo.
    let assistant_text: String = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::AssistantDelta { text, .. } | EventKind::AssistantMessage { text, .. } => {
                Some(text.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    let saw_text = assistant_text.contains(&token);
    let status = if users == 1
        && idempotent
        && conflict_ok
        && saw_text
        && matches!(
            terminal.map(|event| &event.kind),
            Some(EventKind::TurnCompleted { .. })
        ) {
        Status::Pass
    } else {
        Status::Fail
    };
    let _ = client.call(fleet_protocol::Request::StopSession {
        session_id: session_id.clone(),
    });
    CheckResult::new(
        provider,
        "real_chat",
        status,
        format!(
            "op={op_id} user_events={users} idempotent={idempotent} conflict={conflict_ok} text_seen={saw_text}"
        ),
    )
}

pub fn run_all(providers: &[String], options: &Options) -> Vec<CheckResult> {
    let mut results = Vec::new();
    for provider in providers {
        results.extend(run_provider(provider, options));
    }
    results
}

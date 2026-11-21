//! Traducción pura de notificaciones/requests de Codex a eventos Fleet.
//! Ningún ID nativo (`threadId`, `turnId`, `itemId`, ID JSON-RPC) se publica.

use std::collections::{HashMap, HashSet};

use fleet_agent::{redact_provider_error, AgentError};
use fleet_domain::{
    ApprovalArgSpec, ApprovalArgument, ApprovalOptionSpec, DomainEvent, TurnStatus,
};
use serde_json::{json, Value};

use crate::rpc::ServerRequest;

#[derive(Debug, Clone, Default)]
pub struct EventMapper {
    pub native_thread: Option<String>,
    pub native_turn: Option<String>,
    pub fleet_turn: Option<String>,
    unscoped_delta: bool,
    delta_items: HashSet<String>,
    last_tool_name: Option<String>,
    tool_names: HashMap<String, String>,
}

fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

impl EventMapper {
    pub fn set_native_thread(&mut self, id: String) {
        self.native_thread = Some(id);
    }

    pub fn begin_turn(&mut self, native_turn: String, fleet_turn: String) {
        self.native_turn = Some(native_turn);
        self.fleet_turn = Some(fleet_turn);
        self.unscoped_delta = false;
        self.delta_items.clear();
        self.last_tool_name = None;
        self.tool_names.clear();
    }

    pub fn end_turn(&mut self) {
        self.native_turn = None;
        self.fleet_turn = None;
        self.unscoped_delta = false;
        self.delta_items.clear();
        self.last_tool_name = None;
        self.tool_names.clear();
    }

    fn scoped(&self, params: &Value) -> bool {
        match (
            &self.native_thread,
            params.get("threadId").and_then(Value::as_str),
        ) {
            (Some(expected), Some(actual)) => expected == actual,
            _ => true,
        }
    }

    fn event(&self, kind: DomainEvent) -> Option<fleet_domain::AgentEvent> {
        Some(fleet_domain::AgentEvent { kind })
    }

    /// Devuelve cero, uno o varios eventos. Las notificaciones de estado
    /// (`thread/status/changed`) NO se usan como terminalidad del turno.
    pub fn map(&mut self, frame: &Value) -> Vec<fleet_domain::AgentEvent> {
        let method = match frame.get("method").and_then(Value::as_str) {
            Some(method) => method,
            None => return Vec::new(),
        };
        let params = frame.get("params").cloned().unwrap_or(json!({}));
        if !self.scoped(&params) {
            return Vec::new();
        }
        let Some(turn) = self.fleet_turn.clone() else {
            return Vec::new();
        };
        match method {
            "item/agentMessage/delta" => {
                if let Some(id) = text(&params, "itemId") {
                    self.delta_items.insert(id);
                } else {
                    self.unscoped_delta = true;
                }
                let Some(delta) = text(&params, "delta") else {
                    return Vec::new();
                };
                self.event(DomainEvent::AssistantDelta {
                    turn_id: turn,
                    text: delta,
                })
                .into_iter()
                .collect()
            }
            "item/started" | "item/completed" => {
                let item = params.get("item").cloned().unwrap_or(json!({}));
                let item_type = text(&item, "type").unwrap_or_default();
                let is_tool = matches!(
                    item_type.as_str(),
                    "commandExecution" | "fileChange" | "mcpToolCall" | "dynamicToolCall"
                );
                if method == "item/started" {
                    if !is_tool {
                        return Vec::new();
                    }
                    let name = if item_type.is_empty() {
                        "tool".to_string()
                    } else {
                        item_type.clone()
                    };
                    self.last_tool_name = Some(name.clone());
                    if let Some(id) = text(&item, "id") {
                        self.tool_names.insert(id, name.clone());
                    }
                    let input = text(&item, "command")
                        .or_else(|| text(&item, "query"))
                        .or_else(|| {
                            let changes = strings(item.get("changes")).join(", ");
                            if changes.is_empty() {
                                None
                            } else {
                                Some(changes)
                            }
                        })
                        .unwrap_or_default();
                    return self
                        .event(DomainEvent::ToolStarted {
                            turn_id: turn,
                            name,
                            input: redact_provider_error(&input),
                        })
                        .into_iter()
                        .collect();
                }
                if item_type == "agentMessage" {
                    // No duplicar texto: si ya hubo deltas, el mensaje final
                    // no se vuelve a publicar como AssistantMessage.
                    let streamed = text(&item, "id").is_some_and(|id| self.delta_items.remove(&id));
                    if streamed || self.unscoped_delta {
                        self.unscoped_delta = false;
                        return Vec::new();
                    }
                    let full = text(&item, "text").or_else(|| {
                        let content = strings(item.get("content")).join(" ");
                        if content.trim().is_empty() {
                            None
                        } else {
                            Some(content.trim().to_string())
                        }
                    });
                    let Some(full) = full else {
                        return Vec::new();
                    };
                    return self
                        .event(DomainEvent::AssistantMessage {
                            turn_id: turn,
                            text: full,
                        })
                        .into_iter()
                        .collect();
                }
                if !is_tool {
                    return Vec::new();
                }
                let name = text(&item, "id")
                    .and_then(|id| self.tool_names.remove(&id))
                    .or_else(|| self.last_tool_name.clone())
                    .unwrap_or_else(|| "tool".into());
                let output = text(&item, "aggregatedOutput")
                    .or_else(|| text(&item, "output"))
                    .or_else(|| text(&item, "result"))
                    .filter(|value| !value.is_empty());
                let item_status = text(&item, "status").unwrap_or_default();
                let success = matches!(item_status.as_str(), "completed" | "succeeded" | "ok" | "");
                let mut events = Vec::new();
                if let Some(output) = output {
                    if let Some(event) = self.event(DomainEvent::ToolOutput {
                        turn_id: turn.clone(),
                        name: name.clone(),
                        output: redact_provider_error(&output),
                    }) {
                        events.push(event);
                    }
                }
                if let Some(event) = self.event(DomainEvent::ToolCompleted {
                    turn_id: turn,
                    name,
                    success,
                }) {
                    events.push(event);
                }
                events
            }
            "turn/completed" => {
                let turn_status = text(&params.get("turn").cloned().unwrap_or(json!({})), "status")
                    .unwrap_or_default();
                let status = match turn_status.as_str() {
                    "interrupted" => TurnStatus::Interrupted,
                    "failed" => TurnStatus::Failed,
                    _ => TurnStatus::Completed,
                };
                let events = self
                    .event(DomainEvent::TurnCompleted {
                        turn_id: turn,
                        status,
                    })
                    .into_iter()
                    .collect();
                self.end_turn();
                events
            }
            "turn/failed" | "turn/error" => {
                let message = redact_provider_error(&text(&params, "message").unwrap_or_default());
                let events = self
                    .event(DomainEvent::TurnFailed {
                        turn_id: turn,
                        message,
                    })
                    .into_iter()
                    .collect();
                self.end_turn();
                events
            }
            "deprecationNotice" => self
                .event(DomainEvent::AgentNotice {
                    message: redact_provider_error(&text(&params, "message").unwrap_or_default()),
                })
                .into_iter()
                .collect(),
            "thread/status/changed" | "turn/started" | "thread/started" | "item/updated" => {
                Vec::new()
            }
            _ => Vec::new(),
        }
    }
}

/// Datos privados de una solicitud de aprobación. Solo viven en el driver.
#[derive(Debug, Clone)]
pub struct NativeApproval {
    pub native_request_id: String,
    pub request_id: Value,
    pub method: String,
    pub params: Value,
    pub message: String,
    pub resources: Vec<String>,
    pub options: Vec<ApprovalOptionSpec>,
}

fn arg(name: &str, kind: &str, required: bool) -> ApprovalArgSpec {
    ApprovalArgSpec {
        name: name.into(),
        kind: kind.into(),
        required,
    }
}

fn option(option_id: &str, label: &str, args: Vec<ApprovalArgSpec>) -> ApprovalOptionSpec {
    ApprovalOptionSpec {
        option_id: option_id.into(),
        label: label.into(),
        args,
    }
}

fn defaults_for(method: &str) -> Vec<ApprovalOptionSpec> {
    match method {
        "item/commandExecution/requestApproval" | "execCommandApproval" => vec![
            option("accept", "Accept", vec![]),
            option("accept_for_session", "Accept for this session", vec![]),
            option("decline", "Deny and continue", vec![]),
            option("cancel", "Deny and stop the turn", vec![]),
            option(
                "amend_execpolicy",
                "Accept with a policy amendment",
                vec![arg("execpolicy_amendment", "text_list", true)],
            ),
            option(
                "apply_network_policy",
                "Accept with a network policy",
                vec![arg("action", "text", true), arg("host", "text", true)],
            ),
        ],
        "item/fileChange/requestApproval" | "applyPatchApproval" => vec![
            option("accept", "Accept", vec![]),
            option("accept_for_session", "Accept for this session", vec![]),
            option("decline", "Deny and continue", vec![]),
            option("cancel", "Deny and stop the turn", vec![]),
        ],
        "item/tool/requestUserInput" => vec![option(
            "answer",
            "Answer",
            vec![arg("answers", "text_list", true)],
        )],
        "item/permissions/requestApproval" => vec![
            option(
                "grant",
                "Grant permissions",
                vec![
                    arg("permissions", "text_list", true),
                    arg("scope", "text", false),
                ],
            ),
            option("decline", "Deny", vec![]),
        ],
        _ => vec![
            option("accept", "Accept", vec![]),
            option("decline", "Deny", vec![]),
        ],
    }
}

/// Filtra las opciones anunciadas por el proveedor. `availableDecisions`
/// aparece en runtime aunque no esté en el esquema generado (P4.0).
fn announced_options(method: &str, params: &Value) -> Vec<ApprovalOptionSpec> {
    let allowed = params
        .get("availableDecisions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if allowed.is_empty() {
        return defaults_for(method);
    }
    let mut result = Vec::new();
    for candidate in defaults_for(method) {
        let announced = allowed.iter().any(|value| match value {
            Value::String(name) => {
                name == &candidate.option_id
                    || name.replace('_', "") == candidate.option_id.replace('_', "")
                    || native_name(&candidate.option_id) == name.as_str()
            }
            Value::Object(map) => map.contains_key(native_name(&candidate.option_id)),
            _ => false,
        });
        if announced {
            result.push(candidate);
        }
    }
    if result.is_empty() {
        // No inventar opciones: si el proveedor anunció algo que no sabemos
        // traducir, no ofrecemos una decisión incorrecta.
        return Vec::new();
    }
    result
}

fn native_name(option_id: &str) -> &'static str {
    match option_id {
        "accept" => "accept",
        "accept_for_session" => "acceptForSession",
        "decline" => "decline",
        "cancel" => "cancel",
        "amend_execpolicy" => "acceptWithExecpolicyAmendment",
        "apply_network_policy" => "applyNetworkPolicyAmendment",
        "answer" => "answers",
        "grant" => "permissions",
        _ => "unknown",
    }
}

pub fn parse_approval_request(request: &ServerRequest) -> Option<NativeApproval> {
    let support = [
        "item/commandExecution/requestApproval",
        "item/fileChange/requestApproval",
        "item/tool/requestUserInput",
        "item/permissions/requestApproval",
        "applyPatchApproval",
        "execCommandApproval",
    ];
    if !support.contains(&request.method.as_str()) {
        return None;
    }
    // Construir el esquema de preguntas con sus IDs nativos reales.
    let options = if request.method == "item/tool/requestUserInput" {
        let questions = request.params.get("questions")?.as_array()?;
        let mut seen = std::collections::HashSet::new();
        let mut fields = Vec::new();
        for question in questions {
            let id = text(question, "id")?;
            if !seen.insert(id.clone()) {
                return None;
            }
            fields.push(arg(&format!("question:{id}"), "text_list", true));
        }
        if fields.is_empty() {
            return None;
        }
        vec![option("answer", "Answer questions", fields)]
    } else {
        announced_options(&request.method, &request.params)
    };
    if options.is_empty() {
        return None;
    }
    let native_request_id = match &request.id {
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        _ => return None,
    };
    let message = text(&request.params, "reason")
        .or_else(|| text(&request.params, "message"))
        .or_else(|| text(&request.params, "command"))
        .or_else(|| text(&request.params, "itemDescription"))
        .unwrap_or_else(|| "Codex requests a decision".into());
    let resources: Vec<String> = strings(request.params.get("resources"))
        .into_iter()
        .chain(strings(request.params.get("paths")))
        .chain(
            request
                .params
                .get("command")
                .and_then(Value::as_str)
                .map(|command| vec![command.to_string()])
                .unwrap_or_default(),
        )
        .collect();
    Some(NativeApproval {
        native_request_id,
        request_id: request.id.clone(),
        method: request.method.clone(),
        params: request.params.clone(),
        message: redact_provider_error(&message),
        resources: resources
            .into_iter()
            .map(|resource| redact_provider_error(&resource))
            .collect(),
        options,
    })
}

fn arg_values(args: &[ApprovalArgument], name: &str) -> Vec<String> {
    args.iter()
        .find(|arg| arg.name == name)
        .map(|arg| arg.values.clone())
        .unwrap_or_default()
}

/// Construye el payload nativo SOLO para una opción anunciada y validada.
pub fn build_native_answer(
    method: &str,
    option_id: &str,
    options: &[ApprovalOptionSpec],
    args: &[ApprovalArgument],
    params: &Value,
) -> Result<Value, AgentError> {
    let spec = options
        .iter()
        .find(|option| option.option_id == option_id)
        .ok_or_else(|| AgentError {
            code: "InvalidDecision".into(),
            message: "decision was not announced for this approval".into(),
        })?;
    let mut used = std::collections::HashSet::new();
    for input in args {
        if !used.insert(input.name.as_str()) {
            return Err(AgentError {
                code: "InvalidArgument".into(),
                message: "duplicate argument".into(),
            });
        }
        let field = spec
            .args
            .iter()
            .find(|field| field.name == input.name)
            .ok_or_else(|| AgentError {
                code: "InvalidArgument".into(),
                message: "argument not announced".into(),
            })?;
        if input.values.is_empty()
            || input.values.iter().any(String::is_empty)
            || (field.kind == "text" && input.values.len() != 1)
            || !matches!(field.kind.as_str(), "text" | "text_list")
        {
            return Err(AgentError {
                code: "InvalidArgument".into(),
                message: "invalid argument type or values".into(),
            });
        }
    }
    for required in spec.args.iter().filter(|arg| arg.required) {
        if arg_values(args, &required.name).is_empty() {
            return Err(AgentError {
                code: "InvalidArgument".into(),
                message: format!("missing required argument `{}`", required.name),
            });
        }
    }
    let answer = match option_id {
        "accept" => json!({"decision": "accept"}),
        "accept_for_session" => json!({"decision": "acceptForSession"}),
        "decline" => json!({"decision": "decline"}),
        "cancel" => json!({"decision": "cancel"}),
        "amend_execpolicy" => json!({
            "decision": {
                "acceptWithExecpolicyAmendment": {
                    "execpolicy_amendment": arg_values(args, "execpolicy_amendment")
                }
            }
        }),
        "apply_network_policy" => json!({
            "decision": {
                "applyNetworkPolicyAmendment": {
                    "network_policy_amendment": {
                        "action": arg_values(args, "action").first().cloned().unwrap_or_default(),
                        "host": arg_values(args, "host").first().cloned().unwrap_or_default()
                    }
                }
            }
        }),
        "answer" => {
            let mut answer_map = serde_json::Map::new();
            for field in &spec.args {
                let id = field
                    .name
                    .strip_prefix("question:")
                    .ok_or_else(|| AgentError {
                        code: "InvalidArgument".into(),
                        message: "unrecognized question identifier".into(),
                    })?;
                let values = arg_values(args, &field.name);
                if values.is_empty() {
                    return Err(AgentError {
                        code: "InvalidArgument".into(),
                        message: "a required native question was not answered".into(),
                    });
                }
                answer_map.insert(id.to_string(), json!({"answers": values}));
            }
            json!({"answers": answer_map})
        }
        "grant" => {
            let allowed = strings(params.get("permissions"));
            let requested = arg_values(args, "permissions");
            if allowed.is_empty()
                || requested.is_empty()
                || !requested.iter().all(|value| allowed.contains(value))
            {
                return Err(AgentError {
                    code: "InvalidArgument".into(),
                    message: "permission grant must be a subset of the native request".into(),
                });
            }
            let scope = arg_values(args, "scope")
                .first()
                .cloned()
                .unwrap_or_else(|| "turn".into());
            let scopes = strings(params.get("availableScopes"));
            if !matches!(scope.as_str(), "turn" | "session")
                || (scopes.is_empty() && scope != "turn")
                || (!scopes.is_empty() && !scopes.contains(&scope))
            {
                return Err(AgentError {
                    code: "InvalidArgument".into(),
                    message: "permission scope is not allowed by native request".into(),
                });
            }
            json!({"permissions": requested, "scope": scope})
        }
        _ => {
            return Err(AgentError::unsupported("approval decision"));
        }
    };
    // Compatibilidad con las decisiones legacy del App Server.
    if method == "applyPatchApproval" || method == "execCommandApproval" {
        let legacy = match option_id {
            "accept" => "approved",
            "accept_for_session" => "approved_for_session",
            "decline" => "abort",
            "cancel" => "abort",
            _ => return Err(AgentError::unsupported("legacy approval decision")),
        };
        return Ok(json!({"decision": legacy}));
    }
    Ok(answer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deltas_suppress_duplicate_final_message() {
        let mut mapper = EventMapper::default();
        mapper.begin_turn("native-turn".into(), "turn-1".into());
        let delta = json!({"method":"item/agentMessage/delta","params":{"threadId":"native-thread","delta":"hola"}});
        let events = mapper.map(&delta);
        assert!(matches!(
            &events[0].kind,
            DomainEvent::AssistantDelta { text, .. } if text == "hola"
        ));
        let final_item = json!({
            "method":"item/completed",
            "params":{"threadId":"native-thread","item":{"type":"agentMessage","text":"hola"}}
        });
        assert!(mapper.map(&final_item).is_empty());
    }

    #[test]
    fn second_message_without_delta_is_not_suppressed_by_first() {
        let mut mapper = EventMapper::default();
        mapper.begin_turn("native-turn".into(), "fleet-turn".into());
        mapper.map(&json!({
            "method": "item/agentMessage/delta",
            "params": {"itemId": "m1", "delta": "first"}
        }));
        assert!(mapper
            .map(&json!({
                "method": "item/completed",
                "params": {"item": {"id": "m1", "type": "agentMessage", "text": "first"}}
            }))
            .is_empty());
        let second = mapper.map(&json!({
            "method": "item/completed",
            "params": {"item": {"id": "m2", "type": "agentMessage", "text": "second"}}
        }));
        assert!(matches!(
            second[0].kind,
            DomainEvent::AssistantMessage { .. }
        ));
    }

    #[test]
    fn final_message_is_emitted_without_deltas() {
        let mut mapper = EventMapper::default();
        mapper.begin_turn("native-turn".into(), "turn-1".into());
        let frame = json!({
            "method":"item/completed",
            "params":{"threadId":"native-thread","item":{"type":"agentMessage","text":"hola"}}
        });
        let events = mapper.map(&frame);
        assert!(
            matches!(&events[0].kind, DomainEvent::AssistantMessage { text, .. } if text == "hola")
        );
    }

    #[test]
    fn idle_status_is_not_a_terminal_turn() {
        let mut mapper = EventMapper::default();
        mapper.begin_turn("native-turn".into(), "turn-1".into());
        let frame = json!({
            "method":"thread/status/changed",
            "params":{"threadId":"native-thread","status":{"type":"idle"}}
        });
        assert!(mapper.map(&frame).is_empty());
        assert!(mapper.fleet_turn.is_some());
    }

    #[test]
    fn native_ids_are_never_used_as_fleet_turn_ids() {
        let mut mapper = EventMapper::default();
        mapper.set_native_thread("native-thread".into());
        mapper.begin_turn("native-turn".into(), "turn-fleet".into());
        let other =
            json!({"method":"item/agentMessage/delta","params":{"threadId":"other","delta":"x"}});
        assert!(mapper.map(&other).is_empty());
        let frame = json!({"method":"item/agentMessage/delta","params":{"threadId":"native-thread","delta":"x"}});
        let events = mapper.map(&frame);
        match &events[0].kind {
            DomainEvent::AssistantDelta { turn_id, .. } => assert_eq!(turn_id, "turn-fleet"),
            _ => panic!("unexpected event"),
        }
    }

    #[test]
    fn only_announced_options_are_accepted_and_args_are_validated() {
        let request = ServerRequest {
            id: json!(7),
            method: "item/commandExecution/requestApproval".into(),
            params: json!({
                "availableDecisions": ["accept", {"acceptWithExecpolicyAmendment":{}}],
                "command": "cargo test"
            }),
        };
        let approval = parse_approval_request(&request).unwrap();
        assert_eq!(approval.native_request_id, "7");
        assert_eq!(approval.options.len(), 2);
        assert!(approval
            .options
            .iter()
            .any(|o| o.option_id == "amend_execpolicy"));
        assert!(build_native_answer(
            &approval.method,
            "decline",
            &approval.options,
            &[],
            &request.params
        )
        .is_err());
        assert!(build_native_answer(
            &approval.method,
            "amend_execpolicy",
            &approval.options,
            &[],
            &request.params
        )
        .is_err());
        let answer = build_native_answer(
            &approval.method,
            "amend_execpolicy",
            &approval.options,
            &[ApprovalArgument {
                name: "execpolicy_amendment".into(),
                values: vec!["allow cargo test".into()],
            }],
            &request.params,
        )
        .unwrap();
        assert!(answer.to_string().contains("acceptWithExecpolicyAmendment"));
    }
    #[test]
    fn native_user_question_ids_are_preserved() {
        let request = ServerRequest {
            id: json!(19),
            method: "item/tool/requestUserInput".into(),
            params: json!({"questions":[{"id":"language"},{"id":"style"}]}),
        };
        let approval = parse_approval_request(&request).unwrap();
        let answer = build_native_answer(
            &approval.method,
            "answer",
            &approval.options,
            &[
                ApprovalArgument {
                    name: "question:language".into(),
                    values: vec!["Rust".into()],
                },
                ApprovalArgument {
                    name: "question:style".into(),
                    values: vec!["compact".into()],
                },
            ],
            &request.params,
        )
        .unwrap();
        assert_eq!(answer["answers"]["language"]["answers"][0], "Rust");
        assert_eq!(answer["answers"]["style"]["answers"][0], "compact");
    }

    #[test]
    fn native_permission_grant_rejects_broader_scope_and_resources() {
        let options = vec![option(
            "grant",
            "Grant",
            vec![
                arg("permissions", "text_list", true),
                arg("scope", "text", false),
            ],
        )];
        let params = json!({"permissions":["read"],"availableScopes":["turn"]});
        assert!(build_native_answer(
            "item/permissions/requestApproval",
            "grant",
            &options,
            &[ApprovalArgument {
                name: "permissions".into(),
                values: vec!["write".into()]
            }],
            &params,
        )
        .is_err());
        assert!(build_native_answer(
            "item/permissions/requestApproval",
            "grant",
            &options,
            &[
                ApprovalArgument {
                    name: "permissions".into(),
                    values: vec!["read".into()]
                },
                ApprovalArgument {
                    name: "scope".into(),
                    values: vec!["session".into()]
                },
            ],
            &params,
        )
        .is_err());
    }
}

//! Traducción pura de eventos Pi RPC a eventos de dominio Fleet.
//! No se inventan deltas ni se reinterpreta `agent_end` como finalización.

use fleet_agent::redact_provider_error;
use fleet_domain::{ApprovalArgSpec, ApprovalOptionSpec, DomainEvent, TurnStatus};

use crate::rpc::UiRequest;

#[derive(Debug, Clone, Default)]
pub struct EventMapper {
    pub fleet_turn: Option<String>,
    message_delta_seen: bool,
}

fn as_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Object(_) => value.get("content").map(as_text).unwrap_or_default(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                if part.get("type").and_then(serde_json::Value::as_str) == Some("text") {
                    part.get("text")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                } else {
                    part.as_str().map(str::to_string)
                }
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn field<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    value.get(key)
}

fn text(value: &serde_json::Value, key: &str) -> String {
    field(value, key).map(as_text).unwrap_or_default()
}

impl EventMapper {
    pub fn begin_turn(&mut self, turn: String) {
        self.fleet_turn = Some(turn);
        self.message_delta_seen = false;
    }

    pub fn end_turn(&mut self) {
        self.fleet_turn = None;
        self.message_delta_seen = false;
    }

    fn scoped(&self, kind: DomainEvent) -> Option<fleet_domain::AgentEvent> {
        self.fleet_turn.as_ref()?;
        Some(fleet_domain::AgentEvent { kind })
    }

    pub fn map(&mut self, event: &serde_json::Value) -> Vec<fleet_domain::AgentEvent> {
        let kind = match event.get("type").and_then(serde_json::Value::as_str) {
            Some(kind) => kind,
            None => return Vec::new(),
        };
        let Some(turn) = self.fleet_turn.clone() else {
            return Vec::new();
        };
        match kind {
            "message_start" => {
                // Pi delimita cada mensaje; el streaming de uno no debe
                // suprimir el texto final de un mensaje posterior.
                self.message_delta_seen = false;
                Vec::new()
            }
            "message_update" => {
                let update = field(event, "assistantMessageEvent")
                    .cloned()
                    .unwrap_or_default();
                let update_type = update
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                match update_type {
                    "text_delta" => {
                        let delta = text(&update, "delta");
                        if delta.is_empty() {
                            return Vec::new();
                        }
                        self.message_delta_seen = true;
                        self.scoped(DomainEvent::AssistantDelta {
                            turn_id: turn,
                            text: delta,
                        })
                        .into_iter()
                        .collect()
                    }
                    "thinking_delta" => {
                        let delta = text(&update, "delta");
                        if delta.is_empty() {
                            return Vec::new();
                        }
                        self.scoped(DomainEvent::AgentNotice {
                            message: format!("reasoning: {}", redact_provider_error(&delta)),
                        })
                        .into_iter()
                        .collect()
                    }
                    "error" => self
                        .scoped(DomainEvent::AgentError {
                            message: redact_provider_error(
                                &text_opt(&update, "error")
                                    .or_else(|| text_opt(&update, "reason"))
                                    .unwrap_or_default(),
                            ),
                        })
                        .into_iter()
                        .collect(),
                    _ => Vec::new(),
                }
            }
            "message_end" => {
                let message = field(event, "message").cloned().unwrap_or_default();
                let role = text(&message, "role");
                let had_deltas = std::mem::replace(&mut self.message_delta_seen, false);
                if role != "assistant" || had_deltas {
                    return Vec::new();
                }
                let body = as_text(field(&message, "content").unwrap_or(&serde_json::Value::Null));
                if body.is_empty() {
                    return Vec::new();
                }
                self.scoped(DomainEvent::AssistantMessage {
                    turn_id: turn,
                    text: body,
                })
                .into_iter()
                .collect()
            }
            "tool_execution_start" => {
                let name = text(event, "toolName");
                let input = field(event, "args")
                    .map(|value| value.to_string())
                    .unwrap_or_default();
                self.scoped(DomainEvent::ToolStarted {
                    turn_id: turn,
                    name,
                    input: redact_provider_error(&input),
                })
                .into_iter()
                .collect()
            }
            "tool_execution_update" => {
                let name = text(event, "toolName");
                let output = field(event, "partialResult")
                    .map(as_text)
                    .unwrap_or_default();
                if output.is_empty() {
                    return Vec::new();
                }
                self.scoped(DomainEvent::ToolOutput {
                    turn_id: turn,
                    name,
                    output: redact_provider_error(&output),
                })
                .into_iter()
                .collect()
            }
            "tool_execution_end" => {
                let name = text(event, "toolName");
                let output = field(event, "result").map(as_text).unwrap_or_default();
                let is_error =
                    field(event, "isError").and_then(serde_json::Value::as_bool) == Some(true);
                let mut events = Vec::new();
                if !output.is_empty() {
                    if let Some(e) = self.scoped(DomainEvent::ToolOutput {
                        turn_id: turn.clone(),
                        name: name.clone(),
                        output: redact_provider_error(&output),
                    }) {
                        events.push(e);
                    }
                }
                if let Some(e) = self.scoped(DomainEvent::ToolCompleted {
                    turn_id: turn,
                    name,
                    success: !is_error,
                }) {
                    events.push(e);
                }
                events
            }
            "bash_execution_update" => {
                let output = field(event, "output")
                    .map(as_text)
                    .or_else(|| text_opt(event, "message"))
                    .unwrap_or_default();
                if output.is_empty() {
                    return Vec::new();
                }
                self.scoped(DomainEvent::ToolOutput {
                    turn_id: turn,
                    name: "bash".into(),
                    output: redact_provider_error(&output),
                })
                .into_iter()
                .collect()
            }
            "agent_settled" => {
                let aborted =
                    field(event, "aborted").and_then(serde_json::Value::as_bool) == Some(true);
                let events = self
                    .scoped(DomainEvent::TurnCompleted {
                        turn_id: turn,
                        status: if aborted {
                            TurnStatus::Interrupted
                        } else {
                            TurnStatus::Completed
                        },
                    })
                    .into_iter()
                    .collect();
                self.end_turn();
                events
            }
            "extension_error" => self
                .scoped(DomainEvent::AgentError {
                    message: redact_provider_error(&text(event, "error")),
                })
                .into_iter()
                .collect(),
            "thinking_level_changed" => self
                .scoped(DomainEvent::AgentNotice {
                    message: format!(
                        "thinking level is now {}",
                        redact_provider_error(&text(event, "level"))
                    ),
                })
                .into_iter()
                .collect(),
            // agent_end / turn_end NO son terminalidad: pueden venir antes de
            // reintentos, compactación o mensajes en cola.
            _ => Vec::new(),
        }
    }
}

fn text_opt(value: &serde_json::Value, key: &str) -> Option<String> {
    let result = text(value, key);
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

/// Solicitud interactiva de Extension UI traducida a aprobación Fleet.
#[derive(Debug, Clone)]
pub struct NativeApproval {
    pub request_id: String,
    pub method: String,
    pub message: String,
    pub options: Vec<ApprovalOptionSpec>,
}

pub fn parse_ui_request(request: &UiRequest) -> Option<NativeApproval> {
    if request.id.is_empty() {
        return None;
    }
    let dialog = matches!(
        request.method.as_str(),
        "select" | "confirm" | "input" | "editor"
    );
    if !dialog {
        return None; // notify/setStatus/setWidget no requieren decisión.
    }
    let message = text_opt(&request.value, "title")
        .or_else(|| text_opt(&request.value, "message"))
        .unwrap_or_else(|| "Pi requests an interaction".into());
    let options = match request.method.as_str() {
        "confirm" => vec![
            ApprovalOptionSpec {
                option_id: "confirm".into(),
                label: "Confirm".into(),
                args: vec![],
            },
            ApprovalOptionSpec {
                option_id: "cancel".into(),
                label: "Cancel".into(),
                args: vec![],
            },
        ],
        "select" => {
            let labels: Vec<String> = field(&request.value, "options")
                .and_then(serde_json::Value::as_array)
                .map(|items| items.iter().map(as_text).collect())
                .unwrap_or_default();
            if labels.is_empty() {
                return None;
            }
            let mut options: Vec<ApprovalOptionSpec> = labels
                .into_iter()
                .enumerate()
                .map(|(index, label)| ApprovalOptionSpec {
                    option_id: format!("opt_{index}"),
                    label,
                    args: vec![],
                })
                .collect();
            options.push(ApprovalOptionSpec {
                option_id: "cancel".into(),
                label: "Cancel".into(),
                args: vec![],
            });
            options
        }
        "input" | "editor" => vec![
            ApprovalOptionSpec {
                option_id: "submit".into(),
                label: "Submit".into(),
                args: vec![ApprovalArgSpec {
                    name: "value".into(),
                    kind: "text".into(),
                    required: true,
                }],
            },
            ApprovalOptionSpec {
                option_id: "cancel".into(),
                label: "Cancel".into(),
                args: vec![],
            },
        ],
        _ => return None,
    };
    Some(NativeApproval {
        request_id: request.id.clone(),
        method: request.method.clone(),
        message: redact_provider_error(&message),
        options,
    })
}

/// Construye la respuesta nativa solo para una opción anunciada.
pub fn build_ui_answer(
    approval: &NativeApproval,
    option_id: &str,
    args: &[fleet_domain::ApprovalArgument],
) -> Result<serde_json::Value, fleet_agent::AgentError> {
    let spec = approval
        .options
        .iter()
        .find(|option| option.option_id == option_id)
        .ok_or_else(|| fleet_agent::AgentError {
            code: "InvalidDecision".into(),
            message: "decision was not announced for this interaction".into(),
        })?;
    match approval.method.as_str() {
        "confirm" => Ok(serde_json::json!({"confirmed": option_id == "confirm"})),
        "select" => {
            if option_id == "cancel" {
                return Ok(serde_json::json!({"cancelled": true}));
            }
            let index: usize = option_id
                .strip_prefix("opt_")
                .and_then(|value| value.parse().ok())
                .ok_or_else(|| fleet_agent::AgentError {
                    code: "InvalidDecision".into(),
                    message: "invalid option id".into(),
                })?;
            let label = spec.label.clone();
            let _ = index;
            Ok(serde_json::json!({"value": label}))
        }
        "input" | "editor" => {
            if option_id == "cancel" {
                return Ok(serde_json::json!({"cancelled": true}));
            }
            let value = args
                .iter()
                .find(|arg| arg.name == "value")
                .and_then(|arg| arg.values.first())
                .cloned()
                .ok_or_else(|| fleet_agent::AgentError {
                    code: "InvalidArgument".into(),
                    message: "value argument is required".into(),
                })?;
            Ok(serde_json::json!({"value": value}))
        }
        _ => Err(fleet_agent::AgentError::unsupported("interactive approval")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn agent_end_is_not_terminal_but_agent_settled_is() {
        let mut mapper = EventMapper::default();
        mapper.begin_turn("turn-1".into());
        assert!(mapper
            .map(&json!({"type":"agent_end","messages":[],"willRetry":true}))
            .is_empty());
        let events = mapper.map(&json!({"type":"agent_settled","aborted":false}));
        assert!(matches!(
            &events[0].kind,
            DomainEvent::TurnCompleted {
                status: TurnStatus::Completed,
                ..
            }
        ));
        assert!(mapper.fleet_turn.is_none());
    }

    #[test]
    fn aborted_settlement_is_interrupted() {
        let mut mapper = EventMapper::default();
        mapper.begin_turn("turn-1".into());
        let events = mapper.map(&json!({"type":"agent_settled","aborted":true}));
        assert!(matches!(
            &events[0].kind,
            DomainEvent::TurnCompleted {
                status: TurnStatus::Interrupted,
                ..
            }
        ));
    }

    #[test]
    fn empty_delta_does_not_hide_final_message() {
        let mut mapper = EventMapper::default();
        mapper.begin_turn("turn-1".into());
        assert!(mapper
            .map(&json!({"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":""}}))
            .is_empty());
        let events = mapper.map(&json!({
            "type":"message_end",
            "message":{"role":"assistant","content":[{"type":"text","text":"final"}]}
        }));
        assert!(matches!(
            &events[0].kind,
            DomainEvent::AssistantMessage {text, ..} if text == "final"
        ));
    }

    #[test]
    fn deltas_suppress_duplicate_final_message() {
        let mut mapper = EventMapper::default();
        mapper.begin_turn("turn-1".into());
        let delta = json!({"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"hola"}});
        let events = mapper.map(&delta);
        assert!(
            matches!(&events[0].kind, DomainEvent::AssistantDelta { text, .. } if text == "hola")
        );
        let final_message = json!({"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"hola"}]}});
        assert!(mapper.map(&final_message).is_empty());
    }

    #[test]
    fn a_streamed_message_does_not_hide_the_next_assistant_message() {
        let mut mapper = EventMapper::default();
        mapper.begin_turn("turn-2".into());
        mapper.map(&json!({"type":"message_start","message":{"role":"assistant"}}));
        mapper.map(&json!({
            "type":"message_update",
            "assistantMessageEvent":{"type":"text_delta","delta":"first"}
        }));
        assert!(mapper
            .map(&json!({
                "type":"message_end",
                "message":{"role":"assistant","content":[{"type":"text","text":"first"}]}
            }))
            .is_empty());
        let second = mapper.map(&json!({
            "type":"message_end",
            "message":{"role":"assistant","content":[{"type":"text","text":"second"}]}
        }));
        assert!(matches!(
            &second[0].kind,
            DomainEvent::AssistantMessage {text, ..} if text == "second"
        ));
    }

    #[test]
    fn tools_and_reasoning_are_distinct_events() {
        let mut mapper = EventMapper::default();
        mapper.begin_turn("turn-1".into());
        let reasoning = mapper.map(&json!({"type":"message_update","assistantMessageEvent":{"type":"thinking_delta","delta":"check input"}}));
        assert!(matches!(
            &reasoning[0].kind,
            DomainEvent::AgentNotice { .. }
        ));
        let started = mapper.map(
            &json!({"type":"tool_execution_start","toolName":"bash","args":{"command":"pwd"}}),
        );
        assert!(
            matches!(&started[0].kind, DomainEvent::ToolStarted { name, .. } if name == "bash")
        );
        let ended = mapper.map(&json!({"type":"tool_execution_end","toolName":"bash","result":{"content":[{"type":"text","text":"/tmp"}]},"isError":false}));
        assert!(ended.iter().any(
            |e| matches!(&e.kind, DomainEvent::ToolOutput { output, .. } if output == "/tmp")
        ));
        assert!(ended
            .iter()
            .any(|e| matches!(&e.kind, DomainEvent::ToolCompleted { success: true, .. })));
    }

    #[cfg(unix)]
    #[test]
    fn synthetic_native_select_roundtrip_delivers_explicit_deny() {
        // El productor sintético pide permiso. Fleet debe responder con la
        // opción seleccionada, nunca autoaprobar ni cambiar el modo del usuario.
        let script = r#"IFS= read -r command
printf '%s\n' '{"type":"response","id":"fleet-1","success":true,"data":{"disposition":"handled"}}'
printf '%s\n' '{"type":"extension_ui_request","id":"permission-fixture","method":"select","title":"Allow harmless fixture?","options":["Allow once","Deny"]}'
IFS= read -r answer
case "$answer" in
  *'"value":"Deny"'*) printf '%s\n' '{"type":"agent_settled","aborted":false}' ;;
  *) printf '%s\n' '{"type":"rpc_fatal_error","message":"wrong permission response"}' ;;
esac
"#;
        let args = vec!["-c".into(), script.into()];
        let mut rpc = crate::rpc::JsonlRpc::spawn_with_widgets(
            std::path::Path::new("/tmp"),
            "sh",
            &args,
            &[],
            true,
        )
        .unwrap();
        rpc.call(
            "prompt",
            json!({"message":"synthetic approval fixture"}),
            std::time::Duration::from_secs(5),
        )
        .unwrap();
        let request = (0..200)
            .find_map(|_| {
                let value = rpc.try_ui_request();
                if value.is_none() {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                value
            })
            .expect("native request should arrive");
        let approval = parse_ui_request(&request).unwrap();
        assert_eq!(approval.options[0].label, "Allow once");
        assert_eq!(approval.options[1].label, "Deny");
        let answer = build_ui_answer(&approval, "opt_1", &[]).unwrap();
        assert_eq!(answer["value"], "Deny");
        rpc.respond_ui(&request.id, answer).unwrap();
        let terminal = (0..200)
            .find_map(|_| {
                let value = rpc.try_event();
                if value.is_none() {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                value
            })
            .expect("fixture must report terminal event");
        assert_eq!(terminal["type"], "agent_settled");
        let _ = rpc.close();
    }

    #[test]
    fn extension_ui_becomes_typed_approval_without_native_ids() {
        let request = UiRequest {
            id: "native-ui-1".into(),
            method: "select".into(),
            value: json!({"id":"native-ui-1","method":"select","title":"Allow?","options":["Allow","Deny"]}),
        };
        let approval = parse_ui_request(&request).unwrap();
        assert_eq!(approval.options.len(), 3);
        assert!(!format!("{:?}", approval.options).contains("native-ui-1"));
        let answer = build_ui_answer(&approval, "opt_0", &[]).unwrap();
        assert_eq!(answer["value"], "Allow");
        assert!(build_ui_answer(&approval, "opt_99", &[]).is_err());
    }
}

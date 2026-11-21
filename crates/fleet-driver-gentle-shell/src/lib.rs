//! Proveedor Gentle Shell para Gentle Fleet.
//!
//! Usa Pi RPC para el chat y `gentle-shell.desktop-rpc/v1` sobre Extension UI
//! para capacidades de escritorio. No se usa GPUI, Herdr ni los modos de
//! permiso/Desktop RPC como referencia de arquitectura. Los subagentes quedan
//! fuera de alcance (P4-F05).

mod desktop;
mod events;

use fleet_rpc_jsonl as rpc;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use fleet_agent::{
    redact_provider_error, AgentDriver, AgentError, AgentInput, DriverFactory, LaunchContext,
    NativeHistoryMessage, NativeSessionInfo, ProviderRegistry, RecoveryEvidence, SendReceipt,
};
use fleet_domain::{
    ApprovalArgument, ControlState, DomainEvent, NativeHistoryTool, ProviderAvailability,
    ProviderCapabilities, ProviderDescriptor, ProviderVersion, RunnerMode, SessionState,
    TurnStatus, VersionRange,
};
use fleet_rpc_jsonl::{JsonlRpc, UiRequest};
use serde_json::{json, Value};

use desktop::DesktopClient;
use events::{build_ui_answer, parse_ui_request, EventMapper, NativeApproval};

const PROGRAM: &str = "gentle-shell";
const COMPATIBILITY: VersionRange = VersionRange {
    min_major: 4,
    min_minor: 8,
    max_major_exclusive: 5,
};
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
const EMBEDDED_PI_VERSION: &str = "1.5.2";

fn map_rpc(error: fleet_rpc_jsonl::RpcError) -> AgentError {
    AgentError {
        code: error.code,
        message: redact_provider_error(&error.message),
    }
}

fn detect_version() -> Result<(ProviderVersion, String), AgentError> {
    let output = Command::new(PROGRAM)
        .arg("--version")
        .output()
        .map_err(|error| AgentError {
            code: "provider_unavailable".into(),
            message: format!("cannot execute gentle-shell: {error}"),
        })?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let shell_version = stdout
        .lines()
        .find_map(|line| {
            line.split_whitespace()
                .find(|part| part.chars().next().is_some_and(|c| c.is_ascii_digit()))
        })
        .unwrap_or("0.0.0")
        .to_string();
    let pi_version = stdout
        .lines()
        .find(|line| line.contains("pi"))
        .and_then(|line| {
            line.split_whitespace()
                .find(|part| part.chars().next().is_some_and(|c| c.is_ascii_digit()))
        })
        .unwrap_or(EMBEDDED_PI_VERSION)
        .to_string();
    Ok((ProviderVersion::parse(shell_version), pi_version))
}

fn metadata(version: ProviderVersion, availability: ProviderAvailability) -> ProviderDescriptor {
    ProviderDescriptor {
        id: "gentle-shell".into(),
        version,
        capabilities: ProviderCapabilities {
            streaming_text: true,
            tool_events: true,
            session_history: true,
            native_turn_ids: false,
            cancel_turn: true,
            approvals: true,
            can_own_process: true,
            can_attach_service: false,
        },
        mode: RunnerMode::Owned,
        availability,
        controls: Vec::new(),
    }
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn parse_controls(
    models: &Value,
    levels: &Value,
    state: &Value,
    permission: Option<&str>,
) -> Vec<ControlState> {
    let mut available = Vec::new();
    let mut current = None;
    for model in models
        .get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (Some(id), Some(provider)) = (text(model, "id"), text(model, "provider")) else {
            continue;
        };
        let value = format!("{provider}:{id}");
        if state
            .get("model")
            .and_then(|m| m.get("id"))
            .and_then(Value::as_str)
            == Some(id.as_str())
        {
            current = Some(value.clone());
        }
        available.push(value);
    }
    let efforts: Vec<String> = levels
        .get("levels")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let mut controls = vec![
        ControlState {
            name: "model".into(),
            current,
            available,
            mutable: true,
            status: "applied".into(),
        },
        ControlState {
            name: "effort".into(),
            current: text(state, "thinkingLevel"),
            available: efforts,
            mutable: true,
            status: "applied".into(),
        },
    ];
    if let Some(mode) = permission {
        controls.push(ControlState {
            name: "permission_mode".into(),
            current: Some(mode.to_string()),
            available: vec![
                "normal".into(),
                "auto".into(),
                "yolo".into(),
                "super-yolo".into(),
            ],
            mutable: std::env::var("FLEET_GS_ALLOW_MODE_WRITE").is_ok(),
            status: "applied".into(),
        });
    }
    controls
}

fn native_ref(value: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in value.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("gs-{hash:016x}")
}

fn discovered_sessions() -> &'static std::sync::Mutex<Vec<NativeSessionInfo>> {
    static DISCOVERED: std::sync::OnceLock<std::sync::Mutex<Vec<NativeSessionInfo>>> =
        std::sync::OnceLock::new();
    DISCOVERED.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

fn session_refs() -> &'static std::sync::Mutex<HashMap<String, String>> {
    static REFS: std::sync::OnceLock<std::sync::Mutex<HashMap<String, String>>> =
        std::sync::OnceLock::new();
    REFS.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// Sondeo aislado de capacidades: abre un proceso Gentle Shell efímero,
/// negocia Desktop RPC y lee modelo/esfuerzo/modo sin modificar nada.
fn discover_controls() -> Vec<ControlState> {
    let args = vec!["--mode".into(), "rpc".into(), "--no-session".into()];
    let environment = [("GENTLE_SHELL_INTERACTIVE_HOST", "1")];
    let dir = std::env::temp_dir();
    let result = (|| -> Result<Vec<ControlState>, AgentError> {
        let mut rpc = JsonlRpc::spawn_with_widgets(&dir, PROGRAM, &args, &environment, true)
            .map_err(map_rpc)?;
        let mut desktop = DesktopClient::new();
        desktop.negotiate(&mut rpc).map_err(AgentError::from)?;
        let state = rpc
            .call("get_state", json!({}), CALL_TIMEOUT)
            .map_err(map_rpc)?;
        let models = rpc
            .call("get_available_models", json!({}), CALL_TIMEOUT)
            .map_err(map_rpc)
            .unwrap_or(json!({}));
        let levels = rpc
            .call("get_available_thinking_levels", json!({}), CALL_TIMEOUT)
            .map_err(map_rpc)
            .unwrap_or(json!({}));
        let permission = if desktop.capabilities.supports("get_permission_mode") {
            desktop
                .call(&mut rpc, "get_permission_mode", &json!({}), CALL_TIMEOUT)
                .ok()
                .and_then(|value| text(&value, "mode"))
        } else {
            None
        };
        let _ = rpc.close();
        Ok(parse_controls(
            &models,
            &levels,
            &state,
            permission.as_deref(),
        ))
    })();
    result.unwrap_or_default()
}

pub struct GentleShellDriver {
    descriptor: ProviderDescriptor,
    workspace: Option<PathBuf>,
    rpc: Option<JsonlRpc>,
    desktop: DesktopClient,
    mapper: EventMapper,
    pending: HashMap<String, NativeApproval>,
    pending_events: VecDeque<fleet_domain::AgentEvent>,
    permission_mode: Option<String>,
    closed: bool,
}

impl Default for GentleShellDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl GentleShellDriver {
    pub fn new() -> Self {
        Self {
            descriptor: metadata(
                ProviderVersion::parse("0.0.0"),
                ProviderAvailability::Unknown,
            ),
            workspace: None,
            rpc: None,
            desktop: DesktopClient::new(),
            mapper: EventMapper::default(),
            pending: HashMap::new(),
            pending_events: VecDeque::new(),
            permission_mode: None,
            closed: false,
        }
    }

    pub fn probe(&mut self) -> ProviderDescriptor {
        match detect_version() {
            Ok((version, embedded_pi)) => {
                self.descriptor = metadata(
                    version.clone(),
                    if COMPATIBILITY.compatible(&version) {
                        ProviderAvailability::Available
                    } else {
                        ProviderAvailability::Unavailable
                    },
                );
                self.descriptor.controls = vec![ControlState {
                    name: "embedded_pi".into(),
                    current: Some(embedded_pi.clone()),
                    available: vec![embedded_pi],
                    mutable: false,
                    status: "applied".into(),
                }];
            }
            Err(_) => self.descriptor.availability = ProviderAvailability::Unavailable,
        }
        self.descriptor.clone()
    }

    fn rpc_mut(&mut self) -> Result<&mut JsonlRpc, AgentError> {
        self.rpc.as_mut().ok_or_else(|| AgentError {
            code: "not_open".into(),
            message: "Gentle Shell RPC is not running".into(),
        })
    }

    fn emit(&mut self, kind: DomainEvent) {
        self.pending_events
            .push_back(fleet_domain::AgentEvent { kind });
    }

    fn refresh_permissions(&mut self) {
        if !self.desktop.capabilities.supports("get_permission_mode") {
            return;
        }
        if let Ok(data) = self.desktop.call(
            self.rpc.as_mut().expect("rpc checked by caller"),
            "get_permission_mode",
            &json!({}),
            CALL_TIMEOUT,
        ) {
            self.permission_mode = text(&data, "mode");
        }
    }

    fn handle_dialog(&mut self, request: UiRequest) {
        let Some(approval) = parse_ui_request(&request) else {
            return;
        };
        let options = approval.options.clone();
        let allowed = options
            .iter()
            .map(|option| option.option_id.clone())
            .collect();
        let message = redact_provider_error(&approval.message);
        self.pending
            .insert(approval.request_id.clone(), approval.clone());
        self.emit(DomainEvent::ApprovalRequested {
            native_request_id: approval.request_id,
            kind: "gentle_shell_extension_ui".into(),
            message,
            resources: vec![],
            allowed_decisions: allowed,
            options,
        });
    }

    fn handle_desktop_events(&mut self) {
        let events: Vec<_> = self.desktop.events.drain(..).collect();
        for event in events {
            if event.event == "sequence_gap" {
                self.emit(DomainEvent::AgentNotice {
                    message: "Desktop RPC sequence gap; snapshot required".into(),
                });
                if self.desktop.capabilities.supports("get_snapshot") {
                    let result = self.desktop.call(
                        self.rpc.as_mut().expect("rpc owned by driver"),
                        "get_snapshot",
                        &json!({}),
                        CALL_TIMEOUT,
                    );
                    if let Ok(data) = result {
                        let seq = data.get("seq").and_then(Value::as_u64).unwrap_or(0);
                        if seq > 0 {
                            self.desktop.sequence.resync(seq);
                        }
                    }
                }
                continue;
            }
            if matches!(
                event.event.as_str(),
                "permission_mode_changed" | "config_changed" | "profile_applied"
            ) {
                self.emit(DomainEvent::AgentNotice {
                    message: format!("desktop event {}", event.event),
                });
            }
            // session_snapshot y widgets no se publican como texto del asistente.
        }
    }
}

impl AgentDriver for GentleShellDriver {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn open(&mut self, ctx: LaunchContext) -> Result<String, AgentError> {
        if !ctx.workspace.is_dir() {
            return Err(AgentError {
                code: "invalid_workspace".into(),
                message: "Gentle Shell workspace is not a directory".into(),
            });
        }
        let (version, embedded_pi) = detect_version()?;
        if !COMPATIBILITY.compatible(&version) {
            return Err(AgentError {
                code: "incompatible_version".into(),
                message: format!("Gentle Shell {} is outside >=4.8,<5", version.text),
            });
        }
        self.descriptor = metadata(version, ProviderAvailability::Available);
        let args = vec![
            "--mode".to_string(),
            "rpc".to_string(),
            "--no-session".to_string(),
        ];
        let environment = [("GENTLE_SHELL_INTERACTIVE_HOST", "1")];
        let mut rpc =
            JsonlRpc::spawn_with_widgets(&ctx.workspace, PROGRAM, &args, &environment, true)
                .map_err(map_rpc)?;
        let state = rpc
            .call("get_state", json!({}), CALL_TIMEOUT)
            .map_err(map_rpc)?;
        let native = text(&state, "sessionId").unwrap_or_default();
        self.workspace = Some(ctx.workspace);
        self.rpc = Some(rpc);
        self.desktop
            .negotiate(self.rpc.as_mut().expect("rpc just set"))
            .map_err(AgentError::from)?;
        self.refresh_permissions();
        let mut discovered: Vec<NativeSessionInfo> = Vec::new();
        if self.desktop.capabilities.supports("list_sessions") {
            if let Ok(data) = self.desktop.call(
                self.rpc.as_mut().expect("rpc just set"),
                "list_sessions",
                &json!({}),
                CALL_TIMEOUT,
            ) {
                for value in data
                    .get("sessions")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let Some(session_id) = text(value, "id").or_else(|| text(value, "sessionId"))
                    else {
                        continue;
                    };
                    let reference = native_ref(&session_id);
                    session_refs()
                        .lock()
                        .expect("session refs poisoned")
                        .insert(reference.clone(), session_id.clone());
                    discovered.push(NativeSessionInfo {
                        native_ref: reference,
                        workspace_hint: text(value, "cwd"),
                        state: text(value, "status").unwrap_or_else(|| "native".into()),
                        activity: text(value, "updatedAt").unwrap_or_else(|| "unknown".into()),
                        can_resume_history: true,
                        model: None,
                        effort: None,
                        externally_active: false,
                    });
                }
            }
        }
        if !discovered.is_empty() {
            let mut list = discovered_sessions().lock().expect("discovery poisoned");
            for item in discovered {
                if !list
                    .iter()
                    .any(|existing| existing.native_ref == item.native_ref)
                {
                    list.push(item);
                }
            }
        }
        let rpc = self.rpc.as_mut().expect("rpc just set");
        let models = rpc
            .call("get_available_models", json!({}), CALL_TIMEOUT)
            .map_err(map_rpc)
            .unwrap_or(json!({}));
        let levels = rpc
            .call("get_available_thinking_levels", json!({}), CALL_TIMEOUT)
            .map_err(map_rpc)
            .unwrap_or(json!({}));
        let state = rpc
            .call("get_state", json!({}), CALL_TIMEOUT)
            .map_err(map_rpc)
            .unwrap_or(json!({}));
        let permission = self.permission_mode.as_deref();
        self.descriptor.controls = parse_controls(&models, &levels, &state, permission);
        self.descriptor.controls.push(ControlState {
            name: "embedded_pi".into(),
            current: Some(embedded_pi.clone()),
            available: vec![embedded_pi],
            mutable: false,
            status: "applied".into(),
        });
        Ok(native)
    }

    fn send(&mut self, input: AgentInput) -> Result<SendReceipt, AgentError> {
        self.mapper.begin_turn(input.turn_id.clone());
        let params = json!({"message": input.text});
        let response = self
            .rpc_mut()?
            .call("prompt", params, CALL_TIMEOUT)
            .map_err(map_rpc)?;
        let disposition = text(&response, "disposition").unwrap_or_default();
        if disposition == "handled" {
            let turn = self.mapper.fleet_turn.take().unwrap_or(input.turn_id);
            self.emit(DomainEvent::TurnCompleted {
                turn_id: turn,
                status: TurnStatus::Completed,
            });
            self.mapper.end_turn();
        } else if disposition.is_empty() {
            self.mapper.end_turn();
            return Err(AgentError {
                code: "provider_protocol".into(),
                message: "prompt response did not report a disposition".into(),
            });
        }
        Ok(SendReceipt { accepted: true })
    }

    fn next_event(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<fleet_domain::AgentEvent>, AgentError> {
        if let Some(event) = self.pending_events.pop_front() {
            return Ok(Some(event));
        }
        let deadline = std::time::Instant::now() + timeout;
        loop {
            // Los mensajes Desktop RPC se drenan ANTES de mapear eventos de chat.
            let (dialogs, frame, alive) = {
                let Some(rpc) = self.rpc.as_mut() else {
                    return Err(AgentError {
                        code: "not_open".into(),
                        message: "Gentle Shell RPC is not running".into(),
                    });
                };
                let dialogs = self.desktop.drain_ui(rpc);
                let frame = if dialogs.is_empty() && self.desktop.events.is_empty() {
                    rpc.try_event()
                } else {
                    None
                };
                (dialogs, frame, rpc.alive())
            };
            if !dialogs.is_empty() {
                for dialog in dialogs {
                    self.handle_dialog(dialog);
                }
                return Ok(self.pending_events.pop_front());
            }
            if !self.desktop.events.is_empty() {
                self.handle_desktop_events();
                return Ok(self.pending_events.pop_front());
            }
            if let Some(frame) = frame {
                let events = self.mapper.map(&frame);
                for event in events {
                    self.pending_events.push_back(event);
                }
                return Ok(self.pending_events.pop_front());
            }
            if !alive && self.mapper.fleet_turn.is_some() {
                let turn = self.mapper.fleet_turn.take().unwrap_or_default();
                self.emit(DomainEvent::TurnFailed {
                    turn_id: turn,
                    message: "Gentle Shell process closed before agent_settled".into(),
                });
                self.mapper.end_turn();
                return Ok(self.pending_events.pop_front());
            }
            if std::time::Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn answer(&mut self, request_id: String, decision: String) -> Result<(), AgentError> {
        self.answer_with_args(request_id, decision, Vec::new())
    }

    fn answer_with_args(
        &mut self,
        request_id: String,
        decision: String,
        args: Vec<ApprovalArgument>,
    ) -> Result<(), AgentError> {
        let approval = self.pending.get(&request_id).ok_or_else(|| AgentError {
            code: "not_found".into(),
            message: "interactive request is no longer pending".into(),
        })?;
        let answer = build_ui_answer(approval, &decision, &args)?;
        self.rpc_mut()?
            .respond_ui(&request_id, answer)
            .map_err(map_rpc)?;
        self.pending.remove(&request_id);
        self.emit(DomainEvent::ApprovalResolved {
            native_request_id: request_id,
            decision,
        });
        Ok(())
    }

    fn interrupt(&mut self, _turn_id: String) -> Result<(), AgentError> {
        if self.mapper.fleet_turn.is_none() {
            return Ok(());
        }
        self.rpc_mut()?
            .call("abort", json!({}), CALL_TIMEOUT)
            .map(|_| ())
            .map_err(map_rpc)?;
        let _ = self.rpc_mut()?.call("clear_queue", json!({}), CALL_TIMEOUT);
        Ok(())
    }

    fn close(&mut self) -> Result<(), AgentError> {
        self.closed = true;
        if let Some(mut rpc) = self.rpc.take() {
            rpc.close().map_err(map_rpc)?;
        }
        Ok(())
    }

    fn inspect_recovery(&self) -> RecoveryEvidence {
        let alive = !self.closed && self.rpc.as_ref().is_some_and(|rpc| rpc.alive());
        RecoveryEvidence {
            session_alive: alive,
            detail: format!(
                "mode=owned version={} desktop_methods={} live_turn_recovery=false",
                self.descriptor.version.text,
                self.desktop.capabilities.methods.len()
            ),
        }
    }

    fn session_state(&self) -> SessionState {
        if self.closed {
            SessionState::Stopped
        } else if self.rpc.is_some() {
            SessionState::Running
        } else {
            SessionState::NeedsReconciliation
        }
    }

    fn list_controls(&self) -> Vec<ControlState> {
        self.descriptor.controls.clone()
    }

    fn get_control(&self, name: &str) -> Result<ControlState, AgentError> {
        self.list_controls()
            .into_iter()
            .find(|control| control.name == name)
            .ok_or_else(|| AgentError::unsupported(name))
    }

    fn set_control(
        &mut self,
        name: &str,
        value: &str,
        scope: &str,
    ) -> Result<ControlState, AgentError> {
        if name != "permission_mode" {
            return Err(AgentError::unsupported(name));
        }
        if std::env::var("FLEET_GS_ALLOW_MODE_WRITE").is_err() {
            return Err(AgentError::unsupported(
                "changing the native permission mode",
            ));
        }
        if !matches!(scope, "turn" | "session") {
            return Err(AgentError {
                code: "InvalidArgument".into(),
                message: "scope must be `turn` or `session`".into(),
            });
        }
        if !self.desktop.capabilities.supports("set_permission_mode") {
            return Err(AgentError::unsupported("set_permission_mode"));
        }
        self.desktop
            .call(
                self.rpc.as_mut().expect("runner owns rpc"),
                "set_permission_mode",
                &json!({"mode": value, "persist": false}),
                CALL_TIMEOUT,
            )
            .map_err(AgentError::from)?;
        self.refresh_permissions();
        let mut control = self.get_control(name)?;
        control.status = if self.permission_mode.as_deref() == Some(value) {
            "applied".into()
        } else {
            "rejected".into()
        };
        Ok(control)
    }
}

pub struct GentleShellFactory;

impl Default for GentleShellFactory {
    fn default() -> Self {
        Self
    }
}

impl DriverFactory for GentleShellFactory {
    fn id(&self) -> String {
        "gentle-shell".into()
    }

    fn compatibility(&self) -> VersionRange {
        COMPATIBILITY
    }

    fn create(&self) -> Box<dyn AgentDriver> {
        Box::new(GentleShellDriver::new())
    }

    fn metadata(&self) -> ProviderDescriptor {
        metadata(
            ProviderVersion::parse("0.0.0"),
            ProviderAvailability::Unknown,
        )
    }

    fn probe(&self) -> ProviderDescriptor {
        let mut descriptor = GentleShellDriver::new().probe();
        let mut controls = discover_controls();
        for control in descriptor.controls.drain(..) {
            if control.name == "embedded_pi"
                && !controls.iter().any(|item| item.name == "embedded_pi")
            {
                controls.push(control);
            }
        }
        descriptor.controls = controls;
        descriptor
    }

    fn list_native_sessions(&self) -> Result<Vec<NativeSessionInfo>, AgentError> {
        Ok(discovered_sessions()
            .lock()
            .expect("discovery poisoned")
            .clone())
    }

    fn import_native_history(
        &self,
        reference: &str,
    ) -> Result<Vec<NativeHistoryMessage>, AgentError> {
        let session_id = session_refs()
            .lock()
            .expect("session refs poisoned")
            .get(reference)
            .cloned()
            .ok_or_else(|| AgentError {
                code: "not_found".into(),
                message: "native session reference is no longer available; rediscover it".into(),
            })?;
        let mut driver = GentleShellDriver::new();
        let ctx = LaunchContext {
            workspace: std::env::temp_dir(),
            mode: RunnerMode::Owned,
        };
        driver.open(ctx)?;
        let mut messages = Vec::new();
        let response = driver
            .rpc
            .as_mut()
            .expect("opened")
            .call(
                "get_messages",
                json!({"sessionId": session_id}),
                CALL_TIMEOUT,
            )
            .map_err(map_rpc)?;
        for (index, value) in response
            .get("messages")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let role = text(value, "role").unwrap_or_else(|| "unknown".into());
            if role == "system" {
                continue;
            }
            let body = value
                .get("content")
                .map(|content| match content {
                    Value::String(text) => text.clone(),
                    Value::Array(parts) => parts
                        .iter()
                        .filter_map(|part| text(part, "text"))
                        .collect::<Vec<_>>()
                        .join(" "),
                    _ => String::new(),
                })
                .unwrap_or_default();
            if body.is_empty() {
                continue;
            }
            let tool = if role == "toolResult" {
                Some(NativeHistoryTool {
                    name: text(value, "toolName").unwrap_or_else(|| "tool".into()),
                    input: None,
                    output: Some(body.clone()),
                    status: None,
                })
            } else {
                None
            };
            messages.push(NativeHistoryMessage {
                role: if role == "toolResult" {
                    "tool".into()
                } else {
                    role
                },
                text: redact_provider_error(&body),
                source_ref: Some(native_ref(&format!("{}:{}", session_id, index))),
                tool,
            });
        }
        let _ = driver.close();
        Ok(messages)
    }
}

pub fn register_gentle_shell(registry: &mut ProviderRegistry) {
    registry.register(Box::new(GentleShellFactory));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_is_independent_and_declares_native_capabilities() {
        let factory = GentleShellFactory;
        assert_eq!(factory.id(), "gentle-shell");
        assert!(factory.supports(&ProviderVersion::parse("4.8.0")));
        assert!(!factory.supports(&ProviderVersion::parse("4.7.0")));
        assert!(!factory.supports(&ProviderVersion::parse("5.0.0")));
        let descriptor = factory.metadata();
        assert!(descriptor.capabilities.approvals);
        assert!(descriptor.capabilities.cancel_turn);
        assert!(!descriptor.capabilities.can_attach_service);
    }

    #[test]
    fn embedded_pi_version_is_discovered_not_hardcoded_in_descriptor() {
        let mut driver = GentleShellDriver::new();
        let descriptor = driver.probe();
        assert!(descriptor
            .controls
            .iter()
            .any(|control| control.name == "embedded_pi"));
    }
}

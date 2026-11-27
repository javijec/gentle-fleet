//! Proveedor Gentle Shell para Gentle Fleet.
//!
//! Usa Pi RPC para el chat y `gentle-shell.desktop-rpc/v1` sobre Extension UI
//! para capacidades de escritorio. No se usa GPUI, Herdr ni los modos de
//! permiso/Desktop RPC como referencia de arquitectura. Los subagentes quedan
//! fuera de alcance (P4-F05).

mod desktop;
mod events;
#[cfg(windows)]
mod windows_bundle;

use fleet_rpc_jsonl as rpc;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::Duration;

use fleet_agent::{
    redact_provider_error, AgentDriver, AgentError, AgentInput, DriverFactory, LaunchContext,
    NativeHistoryMessage, NativeSessionInfo, ProviderRegistry, RecoveryEvidence, SendReceipt,
};
use fleet_domain::{
    ApprovalArgument, ControlState, DomainEvent, ProviderAvailability, ProviderCapabilities,
    ProviderDescriptor, ProviderVersion, RunnerMode, SessionState, TurnStatus, VersionRange,
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
// 5.0.0 is a candidate, not part of the accepted 4.x range. Permit an
// explicit local experiment only; never admit arbitrary 5.x versions.
fn compatible_version(version: &ProviderVersion, experimental: bool) -> bool {
    COMPATIBILITY.compatible(version)
        || (experimental && version.text == "5.0.0")
}

fn experimental_5() -> bool {
    std::env::var("FLEET_GENTLE_SHELL_EXPERIMENTAL_5").as_deref() == Ok("1")
}

const CALL_TIMEOUT: Duration = Duration::from_secs(30);
const EMBEDDED_PI_VERSION: &str = "1.5.2";
const HISTORY_UNSUPPORTED: &str = "Gentle Shell native history requires verified session-bound read-only RPC; an ephemeral --no-session process cannot prove the selected session identity";

fn map_rpc(error: fleet_rpc_jsonl::RpcError) -> AgentError {
    AgentError {
        code: error.code,
        message: redact_provider_error(&error.message),
    }
}

fn shell_command() -> Result<std::process::Command, AgentError> {
    #[cfg(windows)]
    if let Some(command) = windows_bundle::discover()? {
        return Ok(command);
    }
    fleet_agent::process::provider_command(
        PROGRAM,
        "FLEET_GENTLE_SHELL_BIN",
        Some("FLEET_GENTLE_SHELL_JS"),
    )
}

fn detect_version(
    command: &std::process::Command,
) -> Result<(ProviderVersion, String), AgentError> {
    // Node startup can be slower on native Windows (cold cache, scanning).
    // Only the read-only version probe gets this bounded budget.
    #[cfg(windows)]
    let output =
        fleet_agent::process::version_stdout_with_timeout(command, Duration::from_secs(10))?;
    #[cfg(not(windows))]
    let output = fleet_agent::process::version_stdout(command)?;
    let stdout = String::from_utf8_lossy(&output);
    let shell_version = stdout
        .lines()
        .find_map(|line| {
            line.split_whitespace()
                .find(|part| part.chars().next().is_some_and(|c| c.is_ascii_digit()))
        })
        .ok_or_else(|| AgentError {
            code: "provider_unavailable".into(),
            message: "Gentle Shell version probe returned no version".into(),
        })?
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
            session_history: false,
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
    let state_id = state
        .get("model")
        .and_then(|model| model.get("id"))
        .and_then(Value::as_str);
    let state_provider = state
        .get("model")
        .and_then(|model| model.get("provider"))
        .and_then(Value::as_str);
    let candidates = models
        .get("models")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|item| item.get("id").and_then(Value::as_str) == state_id)
                .count()
        })
        .unwrap_or_default();
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
        if state_id == Some(id.as_str())
            && (state_provider == Some(provider.as_str())
                || (state_provider.is_none() && candidates == 1))
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
            // Un flag no garantiza que el home de Gentle Shell esté
            // aislado: P4.3 mantiene permisos de solo lectura.
            mutable: false,
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

/// Sondeo aislado de capacidades: abre un proceso Gentle Shell efímero,
/// negocia Desktop RPC y lee modelo/esfuerzo/modo sin modificar nada.
fn discover_controls() -> Vec<ControlState> {
    let args = vec!["--mode".into(), "rpc".into(), "--no-session".into()];
    let environment = [("GENTLE_SHELL_INTERACTIVE_HOST", "1")];
    let dir = std::env::temp_dir();
    let result = (|| -> Result<Vec<ControlState>, AgentError> {
        let mut rpc = JsonlRpc::spawn_command(&dir, shell_command()?, &args, &environment, true)
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
    native_session_id: Option<String>,
    last_desktop_snapshot: Option<Value>,
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
            native_session_id: None,
            last_desktop_snapshot: None,
            closed: false,
        }
    }

    pub fn probe(&mut self) -> ProviderDescriptor {
        match shell_command().and_then(|command| detect_version(&command)) {
            Ok((version, embedded_pi)) => {
                self.descriptor = metadata(
                    version.clone(),
                    if compatible_version(&version, experimental_5()) {
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
            Err(error) => {
                self.descriptor.availability = ProviderAvailability::Unavailable;
                if std::env::var_os("FLEET_W2_DIAGNOSTICS").is_some() {
                    // Fixed categories only: no native stdout/stderr or paths.
                    let reason = match error.message.as_str() {
                        "provider version probe timed out" => "timeout",
                        "provider version probe exited unsuccessfully" => "nonzero_exit",
                        "provider version probe left active descendants" => "active_descendants",
                        "cannot start contained provider version probe" => "contained_spawn_failed",
                        "Gentle Shell version probe returned no version" => "no_version",
                        "provider JS must be an existing absolute .js/.mjs/.cjs" => "invalid_js",
                        "native provider .exe not found in absolute PATH entries" => {
                            "exe_not_found"
                        }
                        "provider override must be an existing absolute .exe" => "invalid_exe",
                        "configure either provider EXE or JS, not both" => "ambiguous_launcher",
                        "Gentle Shell bundle metadata invalid" => "bundle_metadata_invalid",
                        "Gentle Shell bundle entrypoint or runtime missing" => "bundle_files_missing",
                        "Gentle Shell bundle path escaped root" => "bundle_path_escape",
                        _ => "other_probe_failure",
                    };
                    eprintln!("fleet-w2-discovery gentle-shell {reason}");
                }
            }
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

    /// Un snapshot puede restaurar la secuencia solo si identifica la
    /// sesión Fleet actual. Si faltan identidad o secuencia, no se publica
    /// reconciliación ficticia.
    fn reconcile_desktop_snapshot(&mut self, data: &Value) -> bool {
        let seq = data.get("seq").and_then(Value::as_u64).unwrap_or_default();
        let id = text(data, "sessionId").or_else(|| text(data, "session_id"));
        if seq == 0 || id.as_ref() != self.native_session_id.as_ref() {
            return false;
        }
        self.last_desktop_snapshot = Some(data.clone());
        self.desktop.sequence.resync(seq);
        if let Some(mode) = text(data, "permissionMode") {
            self.permission_mode = Some(mode);
        }
        true
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
                    match result {
                        Ok(data) if self.reconcile_desktop_snapshot(&data) => {
                            self.emit(DomainEvent::AgentNotice {
                                message: "Desktop RPC snapshot reconciled for the current session"
                                    .into(),
                            });
                        }
                        Ok(_) | Err(_) => {
                            self.emit(DomainEvent::AgentNotice {
                                message: "Desktop RPC snapshot missing valid session identity or sequence; reconciliation remains incomplete".into(),
                            });
                        }
                    }
                }
                continue;
            }
            if event.event == "session_snapshot" {
                if !self.reconcile_desktop_snapshot(&event.data) {
                    self.emit(DomainEvent::AgentNotice {
                        message:
                            "ignored Desktop RPC snapshot without matching native session identity"
                                .into(),
                    });
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
        let command = shell_command()?;
        let (version, embedded_pi) = detect_version(&command)?;
        if !compatible_version(&version, experimental_5()) {
            return Err(AgentError {
                code: "incompatible_version".into(),
                message: format!(
                    "Gentle Shell {} is outside >=4.8,<5 (5.0.0 requires explicit experimental opt-in)",
                    version.text
                ),
            });
        }
        self.descriptor = metadata(version, ProviderAvailability::Available);
        let args = vec![
            "--mode".to_string(),
            "rpc".to_string(),
            "--no-session".to_string(),
        ];
        let environment = [("GENTLE_SHELL_INTERACTIVE_HOST", "1")];
        let mut rpc = JsonlRpc::spawn_command(&ctx.workspace, command, &args, &environment, true)
            .map_err(map_rpc)?;
        let state = rpc
            .call("get_state", json!({}), CALL_TIMEOUT)
            .map_err(map_rpc)?;
        let native = text(&state, "sessionId").unwrap_or_default();
        self.native_session_id = Some(native.clone());
        self.workspace = Some(ctx.workspace);
        self.rpc = Some(rpc);
        self.desktop
            .negotiate(self.rpc.as_mut().expect("rpc just set"))
            .map_err(AgentError::from)?;
        self.refresh_permissions();
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
        // Fleet persiste controles diferidos; deben aplicarse al proceso
        // nativo antes de iniciar el turno, nunca ignorarse en silencio.
        for control in &input.controls {
            if let Err(error) = self.set_control(&control.name, &control.value, "turn") {
                let uncertain = matches!(
                    error.code.as_str(),
                    "rpc_timeout" | "rpc_backpressure" | "rpc_protocol" | "provider_gone"
                );
                if uncertain {
                    self.emit(DomainEvent::TurnUncertain {
                        turn_id: input.turn_id.clone(),
                        reason: format!(
                            "Gentle Shell native control outcome uncertain: {}",
                            error.code
                        ),
                    });
                } else {
                    self.emit(DomainEvent::TurnFailed {
                        turn_id: input.turn_id.clone(),
                        message: redact_provider_error(&error.message),
                    });
                }
                return Ok(SendReceipt { accepted: false });
            }
            if self.get_control(&control.name)?.current.as_deref() != Some(control.value.as_str()) {
                self.emit(DomainEvent::TurnFailed {
                    turn_id: input.turn_id.clone(),
                    message: "Gentle Shell did not confirm the requested native control".into(),
                });
                return Ok(SendReceipt { accepted: false });
            }
        }
        self.mapper.begin_turn(input.turn_id.clone());
        let params = json!({"message": input.text});
        let response = match self.rpc_mut()?.call("prompt", params, CALL_TIMEOUT) {
            Ok(response) => response,
            Err(error) => {
                // El prompt pudo haberse ejecutado aunque el ACK se perdiera.
                // No puede reintentarse automáticamente.
                self.emit(DomainEvent::TurnUncertain {
                    turn_id: input.turn_id,
                    reason: format!("Gentle Shell prompt outcome uncertain: {}", error.code),
                });
                self.mapper.end_turn();
                return Ok(SendReceipt { accepted: false });
            }
        };
        let disposition = text(&response, "disposition").unwrap_or_default();
        if disposition == "handled" {
            let turn = self.mapper.fleet_turn.take().unwrap_or(input.turn_id);
            self.emit(DomainEvent::TurnCompleted {
                turn_id: turn,
                status: TurnStatus::Completed,
            });
            self.mapper.end_turn();
        } else if disposition.is_empty() {
            self.emit(DomainEvent::TurnUncertain {
                turn_id: input.turn_id,
                reason: "Gentle Shell prompt response omitted disposition".into(),
            });
            self.mapper.end_turn();
            return Ok(SendReceipt { accepted: false });
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
                if frame.get("type").and_then(Value::as_str) == Some("rpc_fatal_error") {
                    let reason =
                        text(&frame, "message").unwrap_or_else(|| "RPC framing failed".into());
                    if let Some(turn_id) = self.mapper.fleet_turn.take() {
                        self.emit(DomainEvent::TurnUncertain {
                            turn_id,
                            reason: redact_provider_error(&reason),
                        });
                        self.mapper.end_turn();
                    } else {
                        self.emit(DomainEvent::AgentError {
                            message: redact_provider_error(&reason),
                        });
                    }
                    return Ok(self.pending_events.pop_front());
                }
                let events = self.mapper.map(&frame);
                for event in events {
                    self.pending_events.push_back(event);
                }
                return Ok(self.pending_events.pop_front());
            }
            if !alive && self.mapper.fleet_turn.is_some() {
                let turn = self.mapper.fleet_turn.take().unwrap_or_default();
                self.emit(DomainEvent::TurnUncertain {
                    turn_id: turn,
                    reason: "Gentle Shell transport closed before agent_settled".into(),
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
        } else if self.rpc.as_ref().is_some_and(|rpc| rpc.alive()) {
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
        if !matches!(scope, "turn" | "session") {
            return Err(AgentError {
                code: "InvalidArgument".into(),
                message: "scope must be turn or session".into(),
            });
        }
        if self.mapper.fleet_turn.is_some() {
            return Err(AgentError {
                code: "ControlDuringTurn".into(),
                message: "native controls cannot be changed during an active turn".into(),
            });
        }
        if name == "permission_mode" {
            // No existe evidencia de que un flag por sí solo aísle el
            // directorio global y el perfil activo del usuario.
            return Err(AgentError::unsupported(
                "permission_mode write requires verified isolated Gentle Shell home",
            ));
        }
        let control = self.get_control(name)?;
        if !control.available.iter().any(|option| option == value) {
            return Err(AgentError {
                code: "InvalidArgument".into(),
                message: "requested value is not an available native control".into(),
            });
        }
        let (command, params) = match name {
            "model" => {
                let (provider, model_id) = value.split_once(':').ok_or_else(|| AgentError {
                    code: "InvalidArgument".into(),
                    message: "model must be provider:modelId".into(),
                })?;
                ("set_model", json!({"provider":provider,"modelId":model_id}))
            }
            "effort" => ("set_thinking_level", json!({"level":value})),
            _ => return Err(AgentError::unsupported(name)),
        };
        self.rpc_mut()?
            .call(command, params, CALL_TIMEOUT)
            .map_err(map_rpc)?;
        let models = self
            .rpc_mut()?
            .call("get_available_models", json!({}), CALL_TIMEOUT)
            .map_err(map_rpc)?;
        let levels = self
            .rpc_mut()?
            .call("get_available_thinking_levels", json!({}), CALL_TIMEOUT)
            .map_err(map_rpc)?;
        let state = self
            .rpc_mut()?
            .call("get_state", json!({}), CALL_TIMEOUT)
            .map_err(map_rpc)?;
        let embedded = self
            .descriptor
            .controls
            .iter()
            .find(|item| item.name == "embedded_pi")
            .cloned();
        self.descriptor.controls =
            parse_controls(&models, &levels, &state, self.permission_mode.as_deref());
        if let Some(embedded) = embedded {
            self.descriptor.controls.push(embedded);
        }
        let mut effective = self.get_control(name)?;
        effective.status = if effective.current.as_deref() == Some(value) {
            "applied".into()
        } else {
            "rejected".into()
        };
        self.emit(DomainEvent::AgentNotice {
            message: format!("control {} {}: {}", name, effective.status, value),
        });
        if effective.status != "applied" {
            return Err(AgentError {
                code: "ControlNotApplied".into(),
                message: "Gentle Shell did not confirm the requested native control".into(),
            });
        }
        Ok(effective)
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

    fn supports(&self, version: &ProviderVersion) -> bool {
        compatible_version(version, experimental_5())
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
        if descriptor.availability != ProviderAvailability::Available {
            return descriptor;
        }
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
        // Las factorías viven en fleetd, distinto del runner. Nunca depender
        // del cache estático de una sesión abierta en otro proceso.
        let args = vec!["--mode".into(), "rpc".into(), "--no-session".into()];
        let environment = [("GENTLE_SHELL_INTERACTIVE_HOST", "1")];
        let command = shell_command()?;
        let (version, _) = detect_version(&command)?;
        if !compatible_version(&version, experimental_5()) {
            return Err(AgentError {
                code: "incompatible_version".into(),
                message: "Gentle Shell is outside the supported version range".into(),
            });
        }
        let mut rpc =
            JsonlRpc::spawn_command(&std::env::temp_dir(), command, &args, &environment, true)
                .map_err(map_rpc)?;
        let mut desktop = DesktopClient::new();
        let result = (|| -> Result<Vec<NativeSessionInfo>, AgentError> {
            desktop.negotiate(&mut rpc).map_err(AgentError::from)?;
            if !desktop.capabilities.supports("list_sessions") {
                return Err(AgentError::unsupported(
                    "Gentle Shell native session listing",
                ));
            }
            let data = desktop
                .call(&mut rpc, "list_sessions", &json!({}), CALL_TIMEOUT)
                .map_err(AgentError::from)?;
            let sessions = data
                .get("sessions")
                .and_then(Value::as_array)
                .ok_or_else(|| AgentError {
                    code: "NativeDiscoveryUnavailable".into(),
                    message: "Gentle Shell did not return a sessions array".into(),
                })?;
            let mut result = Vec::new();
            for item in sessions {
                let Some(id) = text(item, "id").or_else(|| text(item, "sessionId")) else {
                    continue;
                };
                result.push(NativeSessionInfo {
                    native_ref: native_ref(&id),
                    workspace_hint: text(item, "cwd"),
                    state: text(item, "status").unwrap_or_else(|| "unknown".into()),
                    activity: text(item, "updatedAt").unwrap_or_else(|| "unknown".into()),
                    can_resume_history: false,
                    model: None,
                    effort: None,
                    externally_active: false,
                });
            }
            Ok(result)
        })();
        let _ = rpc.close();
        result
    }

    fn import_native_history(
        &self,
        _reference: &str,
    ) -> Result<Vec<NativeHistoryMessage>, AgentError> {
        // get_messages sin switch_session verifica la sesión del proceso
        // efímero y NO demuestra que un sessionId arbitrario fue leído.
        // Fallar cerrado antes que importar datos de otra sesión.
        Err(AgentError::unsupported(HISTORY_UNSUPPORTED))
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
    fn snapshot_reconciliation_requires_native_session_identity() {
        let mut driver = GentleShellDriver::new();
        driver.native_session_id = Some("local-session".into());
        assert!(
            !driver.reconcile_desktop_snapshot(&json!({"seq": 24, "sessionId":"foreign-session"}))
        );
        assert!(!driver.reconcile_desktop_snapshot(&json!({"seq":24})));
        assert!(!driver.reconcile_desktop_snapshot(&json!({"seq":0, "sessionId":"local-session"})));
        assert!(driver.reconcile_desktop_snapshot(
            &json!({"seq":24, "sessionId":"local-session", "permissionMode":"normal"})
        ));
        assert_eq!(driver.permission_mode.as_deref(), Some("normal"));
        assert_eq!(driver.last_desktop_snapshot.as_ref().unwrap()["seq"], 24);
    }

    #[test]
    fn experimental_five_is_exact_and_opt_in() {
        assert!(compatible_version(&ProviderVersion::parse("4.8.0"), false));
        assert!(!compatible_version(&ProviderVersion::parse("5.0.0"), false));
        assert!(compatible_version(&ProviderVersion::parse("5.0.0"), true));
        assert!(!compatible_version(&ProviderVersion::parse("5.0.1"), true));
        assert!(!compatible_version(&ProviderVersion::parse("5.1.0"), true));
        assert!(!compatible_version(&ProviderVersion::parse("5.0.0-unknown"), true));
    }

    #[test]
    fn duplicate_native_model_ids_are_never_ambiguous() {
        let models = json!({"models":[
            {"id":"same","provider":"a"},
            {"id":"same","provider":"b"}
        ]});
        let state = json!({"model":{"id":"same"}});
        assert!(parse_controls(&models, &json!({}), &state, None)[0]
            .current
            .is_none());
        let exact = json!({"model":{"id":"same","provider":"b"}});
        assert_eq!(
            parse_controls(&models, &json!({}), &exact, None)[0]
                .current
                .as_deref(),
            Some("b:same")
        );
    }

    #[test]
    fn embedded_pi_version_is_not_hardcoded_in_static_metadata() {
        // La versión embebida se detecta en runtime: el test no debe depender
        // de que el binario esté instalado en el entorno de CI.
        let descriptor = GentleShellFactory.metadata();
        assert!(descriptor.controls.is_empty());
        assert_eq!(descriptor.version.text, "0.0.0");
        let parsed = parse_controls(
            &json!({"models":[{"id":"m","provider":"p"}]}),
            &json!({"levels":["low"]}),
            &json!({"model":{"id":"m"},"thinkingLevel":"low"}),
            Some("normal"),
        );
        assert!(parsed.iter().any(|control| control.name == "model"));
        assert!(parsed
            .iter()
            .any(|control| control.name == "permission_mode"));
    }
}

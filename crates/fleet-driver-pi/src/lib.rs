//! Driver de Pi estándar (Pi RPC) para Gentle Fleet.
//!
//! Proveedor independiente de Gentle Shell: no se usa Desktop RPC, modos de
//! permiso propios ni ningún componente de GPUI. El runner es dueño del
//! proceso Pi y de su directorio de sesiones aislado.

mod events;
use fleet_rpc_jsonl as rpc;

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
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
use serde_json::{json, Value};

use events::{build_ui_answer, parse_ui_request, EventMapper, NativeApproval};
use rpc::{JsonlRpc, RpcError, UiRequest};

/// Alias local para no reescribir el driver Pi tras extraer el transporte.
type PiRpc = JsonlRpc;

pub const COMPATIBILITY: VersionRange = VersionRange {
    min_major: 1,
    min_minor: 6,
    max_major_exclusive: 2,
};

const CALL_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_HISTORY_BYTES: u64 = 16 * 1024 * 1024;
const MAX_HISTORY_MESSAGES: usize = 10_000;

fn map_rpc(error: RpcError) -> AgentError {
    AgentError {
        code: error.code,
        message: redact_provider_error(&error.message),
    }
}

fn pi_command() -> Result<std::process::Command, AgentError> {
    fleet_agent::process::provider_command("pi", "FLEET_PI_BIN", Some("FLEET_PI_JS"))
}

fn detect_version(command: &std::process::Command) -> Result<ProviderVersion, AgentError> {
    let output = fleet_agent::process::version_stdout(command)?;
    let text = String::from_utf8_lossy(&output).trim().to_string();
    if text.is_empty() {
        return Err(AgentError {
            code: "provider_unavailable".into(),
            message: "pi --version returned no version".into(),
        });
    }
    Ok(ProviderVersion::parse(text))
}

fn metadata(version: ProviderVersion, availability: ProviderAvailability) -> ProviderDescriptor {
    ProviderDescriptor {
        id: "pi".into(),
        version,
        capabilities: ProviderCapabilities {
            streaming_text: true,
            tool_events: true,
            session_history: true,
            native_turn_ids: false,
            cancel_turn: true, // verificado con e2e_p4_pi_interrupt.sh sobre Pi 1.6.0
            approvals: false,  // solo Extension UI si una extensión la usa
            can_own_process: true,
            can_attach_service: false,
        },
        mode: RunnerMode::Owned,
        availability,
        controls: Vec::new(),
    }
}

fn native_ref(value: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in value.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("pi-{hash:016x}")
}

fn session_refs() -> &'static std::sync::Mutex<HashMap<String, PathBuf>> {
    static REFS: std::sync::OnceLock<std::sync::Mutex<HashMap<String, PathBuf>>> =
        std::sync::OnceLock::new();
    REFS.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

fn native_sessions_dir() -> Result<PathBuf, AgentError> {
    let path = std::env::var_os("PI_SESSION_DIR")
        .map(PathBuf::from)
        .or_else(|| fleet_agent::process::user_home().map(|home| home.join(".pi/agent/sessions")));
    path.filter(|path| path.is_absolute()).ok_or_else(|| AgentError {
        code: "session_dir_unavailable".into(),
        message: "Pi session directory requires an absolute path or a user home".into(),
    })
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn parse_model_controls(models: &Value, levels: &Value, state: &Value) -> Vec<ControlState> {
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
    // Pi 1.6.0 puede omitir el provider en get_state. En ese caso
    // solo inferimos modelo actual si el ID es único en el catálogo.
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
    vec![
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
    ]
}

/// Consulta barata de capacidades: abre un Pi aislado, negocia modelos y
/// esfuerzos y lo cierra. No toma posesión de ninguna sesión externa.
fn discover_controls() -> Vec<ControlState> {
    let dir = std::env::temp_dir().join(format!("gf-pi-probe-{}", uuid::Uuid::new_v4()));
    if std::fs::create_dir_all(&dir).is_err() {
        return Vec::new();
    }
    let result = (|| -> Result<Vec<ControlState>, AgentError> {
        let args = vec![
            "--mode".to_string(),
            "rpc".to_string(),
            "--session-dir".to_string(),
            dir.to_string_lossy().into_owned(),
        ];
        let mut rpc = PiRpc::spawn_command(Path::new(&dir), pi_command()?, &args, &[], false)
            .map_err(map_rpc)?;
        let state = rpc
            .call("get_state", json!({}), Duration::from_secs(5))
            .map_err(map_rpc)?;
        let models = rpc
            .call("get_available_models", json!({}), Duration::from_secs(5))
            .map_err(map_rpc)?;
        let levels = rpc
            .call(
                "get_available_thinking_levels",
                json!({}),
                Duration::from_secs(5),
            )
            .map_err(map_rpc)?;
        let _ = rpc.close();
        Ok(parse_model_controls(&models, &levels, &state))
    })();
    let _ = std::fs::remove_dir_all(dir);
    result.unwrap_or_default()
}

pub struct PiDriver {
    descriptor: ProviderDescriptor,
    workspace: Option<PathBuf>,
    session_dir: Option<PathBuf>,
    rpc: Option<PiRpc>,
    mapper: EventMapper,
    controls: Vec<ControlState>,
    pending: HashMap<String, NativeApproval>,
    pending_events: VecDeque<fleet_domain::AgentEvent>,
    closed: bool,
}

impl Default for PiDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl PiDriver {
    pub fn new() -> Self {
        Self {
            descriptor: metadata(
                ProviderVersion::parse("0.0.0"),
                ProviderAvailability::Unknown,
            ),
            workspace: None,
            session_dir: None,
            rpc: None,
            mapper: EventMapper::default(),
            controls: Vec::new(),
            pending: HashMap::new(),
            pending_events: VecDeque::new(),
            closed: false,
        }
    }

    pub fn probe(&mut self) -> ProviderDescriptor {
        match pi_command().and_then(|command| detect_version(&command)) {
            Ok(version) => {
                self.descriptor = metadata(
                    version.clone(),
                    if COMPATIBILITY.compatible(&version) {
                        ProviderAvailability::Available
                    } else {
                        ProviderAvailability::Unavailable
                    },
                );
            }
            Err(_) => self.descriptor.availability = ProviderAvailability::Unavailable,
        }
        self.descriptor.clone()
    }

    fn rpc(&self) -> Result<&PiRpc, AgentError> {
        self.rpc.as_ref().ok_or_else(|| AgentError {
            code: "not_open".into(),
            message: "Pi RPC is not running".into(),
        })
    }

    fn rpc_mut(&mut self) -> Result<&mut PiRpc, AgentError> {
        self.rpc.as_mut().ok_or_else(|| AgentError {
            code: "not_open".into(),
            message: "Pi RPC is not running".into(),
        })
    }

    fn refresh_controls(&mut self) {
        let models = self.rpc_mut().and_then(|r| {
            r.call("get_available_models", json!({}), CALL_TIMEOUT)
                .map_err(map_rpc)
        });
        let levels = self.rpc_mut().and_then(|r| {
            r.call("get_available_thinking_levels", json!({}), CALL_TIMEOUT)
                .map_err(map_rpc)
        });
        let state = self.rpc_mut().and_then(|r| {
            r.call("get_state", json!({}), CALL_TIMEOUT)
                .map_err(map_rpc)
        });
        if let (Ok(models), Ok(levels), Ok(state)) = (models, levels, state) {
            self.controls = parse_model_controls(&models, &levels, &state);
            self.descriptor.controls = self.controls.clone();
        }
    }

    fn emit(&mut self, kind: DomainEvent) {
        self.pending_events
            .push_back(fleet_domain::AgentEvent { kind });
    }

    fn handle_ui(&mut self, request: UiRequest) {
        // Estas Extension UI son notificaciones, no solicitudes de decisión.
        // Enviar cancelled inventaría una respuesta que Pi no pidió.
        if matches!(
            request.method.as_str(),
            "notify" | "setStatus" | "setWidget"
        ) {
            return;
        }
        let Some(approval) = parse_ui_request(&request) else {
            let _ = self.rpc_mut().and_then(|r| {
                r.respond_ui(&request.id, json!({"cancelled": true}))
                    .map_err(map_rpc)
            });
            return;
        };
        let options = approval.options.clone();
        let allowed: Vec<String> = options.iter().map(|o| o.option_id.clone()).collect();
        let message = redact_provider_error(&approval.message);
        self.pending
            .insert(approval.request_id.clone(), approval.clone());
        self.emit(DomainEvent::ApprovalRequested {
            native_request_id: approval.request_id,
            kind: "pi_extension_ui".into(),
            message,
            resources: vec![],
            allowed_decisions: allowed,
            options,
        });
    }

    fn handle_internal_event(&mut self, event: &Value) -> bool {
        if event.get("type").and_then(Value::as_str) == Some("rpc_fatal_error") {
            let reason = text(event, "message").unwrap_or_else(|| "Pi RPC protocol failure".into());
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
            return true;
        }
        if event.get("type").and_then(Value::as_str) == Some("agent_error") {
            let message = text(event, "message").unwrap_or_else(|| "Pi protocol error".into());
            self.emit(DomainEvent::AgentError {
                message: redact_provider_error(&message),
            });
            return true;
        }
        false
    }
}

impl AgentDriver for PiDriver {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn open(&mut self, ctx: LaunchContext) -> Result<String, AgentError> {
        if !ctx.workspace.is_dir() {
            return Err(AgentError {
                code: "invalid_workspace".into(),
                message: "Pi workspace is not a directory".into(),
            });
        }
        let command = pi_command()?;
        let version = detect_version(&command)?;
        if !COMPATIBILITY.compatible(&version) {
            return Err(AgentError {
                code: "incompatible_version".into(),
                message: format!("Pi {} is outside >=1.6,<2", version.text),
            });
        }
        self.descriptor = metadata(version, ProviderAvailability::Available);
        let session_dir = std::env::temp_dir().join(format!("gf-pi-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&session_dir).map_err(|error| AgentError {
            code: "session_dir_unavailable".into(),
            message: error.to_string(),
        })?;
        let args = vec![
            "--mode".to_string(),
            "rpc".to_string(),
            "--session-dir".to_string(),
            session_dir.to_string_lossy().into_owned(),
        ];
        let mut rpc = match PiRpc::spawn_command(&ctx.workspace, command, &args, &[], false) {
            Ok(rpc) => rpc,
            Err(error) => {
                let _ = std::fs::remove_dir_all(&session_dir);
                return Err(map_rpc(error));
            }
        };
        let state = match rpc.call("get_state", json!({}), CALL_TIMEOUT) {
            Ok(state) => state,
            Err(error) => {
                // Keep data if termination is not confirmed.
                if rpc.close().is_ok() {
                    let _ = std::fs::remove_dir_all(&session_dir);
                }
                return Err(map_rpc(error));
            }
        };
        let native = text(&state, "sessionId").unwrap_or_default();
        self.workspace = Some(ctx.workspace);
        self.session_dir = Some(session_dir);
        self.rpc = Some(rpc);
        self.refresh_controls();
        Ok(native)
    }

    fn send(&mut self, input: AgentInput) -> Result<SendReceipt, AgentError> {
        // Los controles se aplican al proceso Pi ANTES del prompt: la API
        // estable de Pi solo permite cambiarlos cuando no hay turno activo.
        for control in &input.controls {
            if let Err(error) = self.set_control(&control.name, &control.value, "turn") {
                self.emit(DomainEvent::TurnFailed {
                    turn_id: input.turn_id.clone(),
                    message: redact_provider_error(&error.message),
                });
                return Ok(SendReceipt { accepted: false });
            }
        }
        self.mapper.begin_turn(input.turn_id.clone());
        let params = json!({"message": input.text});
        let response = match self.rpc_mut()?.call("prompt", params, CALL_TIMEOUT) {
            Ok(response) => response,
            Err(error) => {
                // El comando pudo ejecutarse sin que el ACK llegara. P3
                // prohíbe reintentar automáticamente operaciones ambiguas.
                self.emit(DomainEvent::TurnUncertain {
                    turn_id: input.turn_id,
                    reason: format!("Pi prompt outcome uncertain: {}", error.code),
                });
                self.mapper.end_turn();
                return Ok(SendReceipt { accepted: false });
            }
        };
        let disposition = text(&response, "disposition").unwrap_or_default();
        if disposition == "handled" {
            // Ninguna ejecución del modelo: el turno se cierra aquí y no se
            // espera `agent_settled`, que nunca llegará.
            let turn = self.mapper.fleet_turn.take().unwrap_or(input.turn_id);
            self.emit(DomainEvent::TurnCompleted {
                turn_id: turn,
                status: TurnStatus::Completed,
            });
            self.mapper.end_turn();
        } else if disposition.is_empty() {
            self.emit(DomainEvent::TurnUncertain {
                turn_id: input.turn_id,
                reason: "Pi prompt response did not report its disposition".into(),
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
            if let Some(request) = self.rpc_mut()?.try_ui_request() {
                self.handle_ui(request);
                return Ok(self.pending_events.pop_front());
            }
            if let Some(frame) = self.rpc_mut()?.try_event() {
                if !self.handle_internal_event(&frame) {
                    for event in self.mapper.map(&frame) {
                        self.pending_events.push_back(event);
                    }
                }
                return Ok(self.pending_events.pop_front());
            }
            if !self.rpc()?.alive() && self.mapper.fleet_turn.is_some() {
                let turn = self.mapper.fleet_turn.take().unwrap_or_default();
                self.emit(DomainEvent::TurnUncertain {
                    turn_id: turn,
                    reason: "Pi RPC reader closed before agent_settled".into(),
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
        if let Some(dir) = self.session_dir.take() {
            let _ = std::fs::remove_dir_all(dir);
        }
        Ok(())
    }

    fn inspect_recovery(&self) -> RecoveryEvidence {
        let alive = !self.closed && self.rpc.as_ref().is_some_and(|rpc| rpc.alive());
        let dropped = self
            .rpc
            .as_ref()
            .map(|rpc| rpc.dropped_frames())
            .unwrap_or(0);
        let stderr_lines = self
            .rpc
            .as_ref()
            .map(|rpc| rpc.stderr_tail().len())
            .unwrap_or(0);
        RecoveryEvidence {
            session_alive: alive,
            detail: format!(
                "mode=owned version={} live_turn_recovery=false dropped_frames={} stderr_lines={}",
                self.descriptor.version.text, dropped, stderr_lines
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
        self.controls.clone()
    }

    fn get_control(&self, name: &str) -> Result<ControlState, AgentError> {
        self.controls
            .iter()
            .find(|control| control.name == name)
            .cloned()
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
                message: "scope must be `turn` or `session`".into(),
            });
        }
        let mut control = self.get_control(name)?;
        if !control.available.iter().any(|item| item == value) {
            return Err(AgentError {
                code: "InvalidArgument".into(),
                message: format!("value `{value}` is not available for `{name}`"),
            });
        }
        if self.mapper.fleet_turn.is_some() {
            return Err(AgentError {
                code: "ControlDuringTurn".into(),
                message: "Pi native controls cannot be changed during an active turn".into(),
            });
        }
        let (command, params) = match name {
            "model" => {
                let (provider, model_id) = value.split_once(':').ok_or_else(|| AgentError {
                    code: "InvalidArgument".into(),
                    message: "model value must be `provider:modelId`".into(),
                })?;
                (
                    "set_model",
                    json!({"provider": provider, "modelId": model_id}),
                )
            }
            "effort" => ("set_thinking_level", json!({"level": value})),
            _ => return Err(AgentError::unsupported(name)),
        };
        self.rpc_mut()?
            .call(command, params, CALL_TIMEOUT)
            .map_err(map_rpc)?;
        self.refresh_controls();
        let effective = self.get_control(name)?;
        control.current = effective.current.clone();
        control.status = if effective.current.as_deref() == Some(value) {
            "applied".into()
        } else {
            "rejected".into()
        };
        // El estado efectivo confirmado por Pi se publica como aviso durable:
        // el cliente no debe confundir la preferencia Fleet con su aplicación.
        self.emit(DomainEvent::AgentNotice {
            message: format!("control {} {}: {}", name, control.status, value),
        });
        Ok(control)
    }
}

pub struct PiFactory;

impl Default for PiFactory {
    fn default() -> Self {
        Self
    }
}

impl DriverFactory for PiFactory {
    fn id(&self) -> String {
        "pi".into()
    }

    fn compatibility(&self) -> VersionRange {
        COMPATIBILITY
    }

    fn create(&self) -> Box<dyn AgentDriver> {
        Box::new(PiDriver::new())
    }

    fn metadata(&self) -> ProviderDescriptor {
        metadata(
            ProviderVersion::parse("0.0.0"),
            ProviderAvailability::Unknown,
        )
    }

    fn probe(&self) -> ProviderDescriptor {
        let mut descriptor = PiDriver::new().probe();
        if descriptor.availability == ProviderAvailability::Available && descriptor.controls.is_empty()
        {
            descriptor.controls = discover_controls();
        }
        descriptor
    }

    /// Descubrimiento no intrusivo de archivos de sesión nativos. Nunca crea
    /// runner ni adopta procesos; la adopción con escritura sigue bloqueada.
    fn list_native_sessions(&self) -> Result<Vec<NativeSessionInfo>, AgentError> {
        let root = native_sessions_dir()?;
        let mut result = Vec::new();
        collect_session_files(&root, &root, &mut result, 0)?;
        Ok(result)
    }

    fn import_native_history(
        &self,
        reference: &str,
    ) -> Result<Vec<NativeHistoryMessage>, AgentError> {
        let path = session_refs()
            .lock()
            .expect("session refs poisoned")
            .get(reference)
            .cloned()
            .ok_or_else(|| AgentError {
                code: "not_found".into(),
                message: "native session reference is no longer available; rediscover it".into(),
            })?;
        let metadata = std::fs::metadata(&path).map_err(|error| AgentError {
            code: "history_unreadable".into(),
            message: error.to_string(),
        })?;
        if metadata.len() > MAX_HISTORY_BYTES {
            return Err(AgentError {
                code: "HistoryResultLimit".into(),
                message: "native session file exceeds the safe import limit".into(),
            });
        }
        let contents = std::fs::read_to_string(&path).map_err(|error| AgentError {
            code: "history_unreadable".into(),
            message: error.to_string(),
        })?;
        if contents.len() as u64 > MAX_HISTORY_BYTES {
            return Err(AgentError {
                code: "HistoryResultLimit".into(),
                message: "native session grew beyond the safe import limit".into(),
            });
        }
        let header = contents.lines().next().ok_or_else(|| AgentError {
            code: "HistoryMalformed".into(),
            message: "Pi session file is empty".into(),
        })?;
        let header: Value = serde_json::from_str(header).map_err(|_| AgentError {
            code: "HistoryMalformed".into(),
            message: "Pi session header is malformed".into(),
        })?;
        let session_id = text(&header, "id").ok_or_else(|| AgentError {
            code: "HistoryMalformed".into(),
            message: "Pi session header lacks identity".into(),
        })?;
        if header.get("type").and_then(Value::as_str) != Some("session")
            || native_ref(&format!("{}:{}", path.display(), session_id)) != reference
        {
            return Err(AgentError {
                code: "HistoryIdentityMismatch".into(),
                message: "native session file changed since discovery; rediscover it".into(),
            });
        }
        parse_native_history(&contents, reference)
    }
}

/// Convierte una captura JSONL de Pi sin perder ni modificar el contenido
/// original. Los bloques no textuales se conservan como JSON estructurado.
fn parse_native_history(
    contents: &str,
    reference: &str,
) -> Result<Vec<NativeHistoryMessage>, AgentError> {
    let mut messages = Vec::new();
    for (index, line) in contents.lines().enumerate() {
        let entry: Value = serde_json::from_str(line).map_err(|_| AgentError {
            code: "HistoryMalformed".into(),
            message: format!(
                "Pi session JSONL contains malformed record at line {}",
                index + 1
            ),
        })?;
        if entry.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let entry_id = text(&entry, "id").ok_or_else(|| AgentError {
            code: "HistoryMalformed".into(),
            message: format!("Pi session message at line {} has no stable ID", index + 1),
        })?;
        let message = entry.get("message").ok_or_else(|| AgentError {
            code: "HistoryMalformed".into(),
            message: format!("Pi session message at line {} has no payload", index + 1),
        })?;
        let role = text(message, "role").ok_or_else(|| AgentError {
            code: "HistoryMalformed".into(),
            message: format!("Pi session message at line {} has no role", index + 1),
        })?;
        // System messages are intentionally excluded from imported user chat.
        if role == "system" {
            continue;
        }
        let content = message.get("content").ok_or_else(|| AgentError {
            code: "HistoryMissingContent".into(),
            message: format!("Pi session message at line {} has no content", index + 1),
        })?;
        let body = match content {
            Value::String(value) => value.clone(),
            Value::Array(parts)
                if parts.iter().all(|part| {
                    part.get("type").and_then(Value::as_str) == Some("text")
                        && part.get("text").and_then(Value::as_str).is_some()
                }) =>
            {
                parts
                    .iter()
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("")
            }
            Value::Array(_) | Value::Object(_) => content.to_string(),
            _ => {
                return Err(AgentError {
                    code: "HistoryMissingContent".into(),
                    message: format!(
                        "Pi session content at line {} cannot be represented",
                        index + 1
                    ),
                });
            }
        };
        if body.is_empty() {
            return Err(AgentError {
                code: "HistoryMissingContent".into(),
                message: format!("Pi session message at line {} has empty content", index + 1),
            });
        }
        if messages.len() >= MAX_HISTORY_MESSAGES {
            return Err(AgentError {
                code: "HistoryResultLimit".into(),
                message: "Pi native history exceeds the maximum number of imported messages".into(),
            });
        }
        let tool = if role == "toolResult" {
            Some(NativeHistoryTool {
                name: text(message, "toolName").unwrap_or_else(|| "tool".into()),
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
            text: body,
            // Incluye la identidad de la sesión en la referencia, porque
            // diferentes sesiones Pi reutilizan IDs de entradas.
            source_ref: Some(native_ref(&format!("{reference}\\0{entry_id}"))),
            tool,
        });
    }
    Ok(messages)
}

fn collect_session_files(
    root: &Path,
    dir: &Path,
    output: &mut Vec<NativeSessionInfo>,
    depth: usize,
) -> Result<(), AgentError> {
    if depth > 6 {
        return Ok(());
    }
    let entries = std::fs::read_dir(dir).map_err(|error| AgentError {
        code: "discovery_unavailable".into(),
        message: error.to_string(),
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_session_files(root, &path, output, depth + 1)?;
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
            continue;
        }
        // Descubrir una sesión requiere solo la cabecera: nunca cargar a
        // memoria todo el historial nativo durante una simple enumeración.
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        let mut reader = std::io::BufReader::new(file.take(64 * 1024));
        let mut header_line = String::new();
        if reader.read_line(&mut header_line).is_err() {
            continue;
        }
        let Ok(header) = serde_json::from_str::<Value>(&header_line) else {
            continue;
        };
        if header.get("type").and_then(Value::as_str) != Some("session") {
            continue;
        }
        let Some(session_id) = text(&header, "id") else {
            continue;
        };
        let reference = native_ref(&format!("{}:{}", path.display(), session_id));
        session_refs()
            .lock()
            .expect("session refs poisoned")
            .insert(reference.clone(), path.clone());
        output.push(NativeSessionInfo {
            native_ref: reference,
            workspace_hint: text(&header, "cwd"),
            state: "native_session_file".into(),
            activity: text(&header, "timestamp").unwrap_or_else(|| "unknown".into()),
            can_resume_history: true,
            model: None,
            effort: None,
            externally_active: false,
        });
    }
    let _ = root;
    Ok(())
}

pub fn register_pi(registry: &mut ProviderRegistry) {
    registry.register(Box::new(PiFactory));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_declares_pi_independently() {
        let factory = PiFactory;
        assert_eq!(factory.id(), "pi");
        assert!(factory.supports(&ProviderVersion::parse("1.6.0")));
        assert!(factory.supports(&ProviderVersion::parse("1.9.0")));
        assert!(!factory.supports(&ProviderVersion::parse("1.5.0")));
        assert!(!factory.supports(&ProviderVersion::parse("2.0.0")));
        let descriptor = factory.metadata();
        assert!(!descriptor.capabilities.can_attach_service);
        assert!(descriptor.capabilities.cancel_turn);
    }

    #[test]
    fn active_model_requires_provider_and_id_match() {
        let models = json!({"models":[
            {"id":"shared","provider":"alpha"},
            {"id":"shared","provider":"beta"}
        ]});
        let levels = json!({"levels":["off"]});
        let state = json!({"model":{"id":"shared","provider":"beta"}});
        let controls = parse_model_controls(&models, &levels, &state);
        assert_eq!(controls[0].current.as_deref(), Some("beta:shared"));
        let unknown = json!({"model":{"id":"shared"}});
        assert!(parse_model_controls(&models, &levels, &unknown)[0]
            .current
            .is_none());
    }

    #[test]
    fn notification_methods_do_not_create_pending_approvals() {
        let mut driver = PiDriver::new();
        for method in ["notify", "setStatus", "setWidget"] {
            driver.handle_ui(UiRequest {
                id: String::new(),
                method: method.into(),
                value: json!({"type":"extension_ui_request","method":method}),
            });
        }
        assert!(driver.pending.is_empty());
        assert!(driver.pending_events.is_empty());
    }

    #[test]
    fn native_history_rejects_malformed_and_truncated_imports() {
        let malformed = concat!("{\"type\":\"session\",\"id\":\"s\"}\n", "{invalid json}\n");
        let error = parse_native_history(malformed, "pi-session-1").unwrap_err();
        assert_eq!(error.code, "HistoryMalformed");
        let mut many = String::new();
        for index in 0..=MAX_HISTORY_MESSAGES {
            many.push_str(&format!(
                "{{\"type\":\"message\",\"id\":\"e{index}\",\"message\":{{\"role\":\"assistant\",\"content\":\"x\"}}}}\n"
            ));
        }
        let error = parse_native_history(&many, "pi-session-1").unwrap_err();
        assert_eq!(error.code, "HistoryResultLimit");
    }

    #[test]
    fn native_history_preserves_original_content_and_session_scoped_ids() {
        let contents = concat!(
            "{\"type\":\"message\",\"id\":\"e1\",\"message\":{\"role\":\"user\",\"content\":\"https://example.invalid/path\",\"toolCallId\":\"abc\"}}\n",
            "{\"type\":\"message\",\"id\":\"e2\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"image\",\"mimeType\":\"image/png\",\"data\":\"abc\"}]}}\n"
        );
        let first = parse_native_history(contents, "pi-first-session").unwrap();
        let second = parse_native_history(contents, "pi-second-session").unwrap();
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].text, "https://example.invalid/path");
        assert!(first[1].text.contains("image/png"));
        assert_ne!(first[0].source_ref, second[0].source_ref);
    }

    #[test]
    fn corrupt_frame_makes_in_flight_pi_turn_uncertain() {
        let mut driver = PiDriver::new();
        driver.mapper.begin_turn("fleet-turn".into());
        assert!(driver.handle_internal_event(
            &json!({"type":"rpc_fatal_error","message":"malformed Pi RPC frame"})
        ));
        assert!(driver.mapper.fleet_turn.is_none());
        let event = driver.pending_events.pop_front().unwrap();
        assert!(matches!(
            event.kind,
            DomainEvent::TurnUncertain { turn_id, .. } if turn_id == "fleet-turn"
        ));
    }

    #[test]
    fn controls_are_discovered_from_native_payloads() {
        let models = json!({"models":[{"id":"m1","provider":"p1"},{"id":"m2","provider":"p2"}]});
        let levels = json!({"levels":["off","medium"]});
        let state = json!({"model":{"id":"m2"},"thinkingLevel":"medium"});
        let controls = parse_model_controls(&models, &levels, &state);
        assert_eq!(controls[0].current.as_deref(), Some("p2:m2"));
        assert_eq!(controls[0].available, vec!["p1:m1", "p2:m2"]);
        assert_eq!(controls[1].current.as_deref(), Some("medium"));
    }
}

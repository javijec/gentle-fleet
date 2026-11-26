//! Driver Codex basado en App Server (JSON-RPC 2.0 sobre stdio).
//!
//! El driver es dueño de un proceso `codex app-server` por sesión Fleet. No
//! analiza ANSI, no lee la TUI y no publica ningún ID nativo en la API Fleet.

mod events;
mod rpc;

use std::collections::{HashMap, VecDeque};
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use fleet_agent::{
    redact_provider_error, AgentDriver, AgentError, AgentInput, DriverFactory, LaunchContext,
    NativeHistoryMessage, NativeSessionInfo, ProviderRegistry, RecoveryEvidence, SendReceipt,
};
use fleet_domain::{
    AgentEvent, ControlState, DomainEvent, NativeHistoryTool, ProviderAvailability,
    ProviderCapabilities, ProviderDescriptor, ProviderVersion, RunnerMode, SessionState,
    VersionRange,
};
use serde_json::{json, Value};

use events::{build_native_answer, parse_approval_request, EventMapper, NativeApproval};
use rpc::{RpcClient, RpcError};

pub const COMPATIBILITY: VersionRange = VersionRange {
    min_major: 0,
    min_minor: 171,
    max_major_exclusive: 1,
};

const CALL_TIMEOUT: Duration = Duration::from_secs(30);
const INIT_TIMEOUT: Duration = Duration::from_secs(15);

fn to_agent_error(error: RpcError) -> AgentError {
    AgentError {
        code: error.code,
        message: redact_provider_error(&error.message),
    }
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

#[cfg(windows)]
fn resolve_windows_codex(
    override_path: Option<&OsStr>,
    path: Option<&OsStr>,
) -> Result<OsString, AgentError> {
    fn is_native(path: &std::path::Path) -> bool {
        path.is_absolute()
            && path.extension().and_then(OsStr::to_str).is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
            && path.is_file()
    }
    if let Some(value) = override_path {
        let candidate = std::path::Path::new(value);
        if is_native(candidate) {
            return Ok(candidate.as_os_str().to_owned());
        }
        return Err(AgentError {
            code: "provider_unavailable".into(),
            message: "FLEET_CODEX_BIN must be an existing absolute native .exe path".into(),
        });
    }
    if let Some(path) = path {
        for directory in std::env::split_paths(path) {
            let candidate = directory.join("codex.exe");
            if is_native(&candidate) {
                return Ok(candidate.into_os_string());
            }
        }
    }
    Err(AgentError {
        code: "provider_unavailable".into(),
        message: "native codex.exe not found; set FLEET_CODEX_BIN to the installed executable (not a .cmd shim)".into(),
    })
}

fn codex_program() -> Result<OsString, AgentError> {
    #[cfg(windows)]
    {
        resolve_windows_codex(
            std::env::var_os("FLEET_CODEX_BIN").as_deref(),
            std::env::var_os("PATH").as_deref(),
        )
    }
    #[cfg(not(windows))]
    {
        Ok(OsString::from("codex"))
    }
}

fn detect_version(program: &OsStr) -> Result<ProviderVersion, AgentError> {
    let output = Command::new(program)
        .arg("--version")
        .output()
        .map_err(|error| AgentError {
            code: "provider_unavailable".into(),
            message: format!("cannot execute codex: {error}"),
        })?;
    if !output.status.success() {
        return Err(AgentError {
            code: "provider_unavailable".into(),
            message: "codex --version failed".into(),
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let version = stdout
        .split_whitespace()
        .find(|part| part.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .unwrap_or("0.0.0")
        .to_string();
    Ok(ProviderVersion::parse(version))
}

fn metadata_with(
    version: ProviderVersion,
    availability: ProviderAvailability,
) -> ProviderDescriptor {
    ProviderDescriptor {
        id: "codex".into(),
        version,
        capabilities: ProviderCapabilities {
            streaming_text: true,
            tool_events: true,
            session_history: true,
            native_turn_ids: true,
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

/// Parsea `model/list`: los IDs de modelo y los esfuerzos se descubren, nunca
/// se hardcodean (P4.1-E).
pub fn controls_from_model_list(result: &Value) -> (Vec<ControlState>, Vec<String>, Vec<String>) {
    let models = result
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut ids = Vec::new();
    let mut efforts = Vec::new();
    let mut default_model = None;
    let mut default_effort = None;
    for model in &models {
        if model.get("hidden").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(id) = text(model, "id").or_else(|| text(model, "model")) else {
            continue;
        };
        if model.get("isDefault").and_then(Value::as_bool) == Some(true) {
            default_model = Some(id.clone());
        }
        for item in model
            .get("supportedReasoningEfforts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(effort) = text(item, "reasoningEffort") {
                if !efforts.contains(&effort) {
                    efforts.push(effort);
                }
            }
        }
        if default_effort.is_none() {
            default_effort = text(model, "defaultReasoningEffort");
        }
        ids.push(id);
    }
    let model_default = default_model.or_else(|| ids.first().cloned());
    let controls = vec![
        ControlState {
            name: "model".into(),
            current: model_default.clone(),
            available: ids.clone(),
            mutable: true,
            status: "applied".into(),
        },
        ControlState {
            name: "effort".into(),
            current: default_effort.clone(),
            available: efforts.clone(),
            mutable: true,
            status: "applied".into(),
        },
    ];
    (controls, ids, efforts)
}

fn discover_controls() -> Vec<ControlState> {
    let dir = std::env::temp_dir();
    let Ok(mut rpc) = RpcClient::spawn(&dir) else {
        return Vec::new();
    };
    let initialized = rpc
        .call(
            "initialize",
            json!({
                "clientInfo": {"name": "gentle-fleet-probe", "version": "0.1.0"},
                "capabilities": {"experimentalApi": false}
            }),
            Duration::from_secs(5),
        )
        .is_ok();
    if !initialized {
        let _ = rpc.close();
        return Vec::new();
    }
    let _ = rpc.notify("initialized", json!({}));
    let result = rpc.call("model/list", json!({}), Duration::from_secs(5));
    let _ = rpc.close();
    result
        .map(|value| controls_from_model_list(&value).0)
        .unwrap_or_default()
}

struct PendingApproval {
    request_id: Value,
    method: String,
    params: Value,
    options: Vec<fleet_domain::ApprovalOptionSpec>,
}

pub struct CodexDriver {
    descriptor: ProviderDescriptor,
    workspace: Option<PathBuf>,
    rpc: Option<RpcClient>,
    mapper: EventMapper,
    pending: HashMap<String, PendingApproval>,
    answered: HashMap<String, String>,
    local_events: VecDeque<AgentEvent>,
    native_thread: Option<String>,
    native_turn: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    controls: Vec<ControlState>,
    announced_dropped_notifications: bool,
    closed: bool,
}

impl Default for CodexDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl CodexDriver {
    pub fn new() -> Self {
        Self {
            descriptor: metadata_with(
                ProviderVersion::parse("0.0.0"),
                ProviderAvailability::Unknown,
            ),
            workspace: None,
            rpc: None,
            mapper: EventMapper::default(),
            pending: HashMap::new(),
            answered: HashMap::new(),
            local_events: VecDeque::new(),
            native_thread: None,
            native_turn: None,
            model: None,
            effort: None,
            controls: Vec::new(),
            announced_dropped_notifications: false,
            closed: false,
        }
    }

    /// Comprobación barata para `ListAgents`: detecta versión sin abrir sesión.
    pub fn probe(&mut self) -> ProviderDescriptor {
        match codex_program().and_then(|program| detect_version(&program)) {
            Ok(version) => {
                self.descriptor = metadata_with(
                    version.clone(),
                    if COMPATIBILITY.compatible(&version) {
                        ProviderAvailability::Available
                    } else {
                        ProviderAvailability::Unavailable
                    },
                );
            }
            Err(_) => {
                self.descriptor.availability = ProviderAvailability::Unavailable;
            }
        }
        if self.descriptor.availability == ProviderAvailability::Available && self.descriptor.controls.is_empty() {
            self.descriptor.controls = discover_controls();
        }
        self.descriptor.clone()
    }

    fn rpc(&self) -> Result<&RpcClient, AgentError> {
        self.rpc.as_ref().ok_or_else(|| AgentError {
            code: "not_open".into(),
            message: "Codex App Server is not running".into(),
        })
    }

    fn rpc_mut(&mut self) -> Result<&mut RpcClient, AgentError> {
        self.rpc.as_mut().ok_or_else(|| AgentError {
            code: "not_open".into(),
            message: "Codex App Server is not running".into(),
        })
    }

    fn emit(&mut self, kind: DomainEvent) {
        self.local_events.push_back(AgentEvent { kind });
    }

    fn handle_server_request(&mut self, request: rpc::ServerRequest) {
        match parse_approval_request(&request) {
            Some(approval) => {
                let NativeApproval {
                    native_request_id,
                    request_id,
                    method,
                    params,
                    message,
                    resources,
                    options,
                } = approval;
                let allowed_decisions: Vec<String> = options
                    .iter()
                    .map(|option| option.option_id.clone())
                    .collect();
                self.pending.insert(
                    native_request_id.clone(),
                    PendingApproval {
                        request_id,
                        method,
                        params,
                        options: options.clone(),
                    },
                );
                self.emit(DomainEvent::ApprovalRequested {
                    native_request_id,
                    kind: "codex_native_approval".into(),
                    message,
                    resources,
                    allowed_decisions,
                    options,
                });
            }
            None => {
                self.emit(DomainEvent::AgentNotice {
                    message: format!(
                        "Codex interactive request unsupported and rejected: {}",
                        redact_provider_error(&request.method)
                    ),
                });
                // Request nativo que no sabemos traducir: se rechaza con error
                // JSON-RPC. Nunca se autoaprueba y nunca se deja sin respuesta
                // al App Server (evita deadlock y aprobaciones huérfanas).
                let id = request.id.clone();
                let _ = self.rpc_mut().and_then(|rpc| {
                    rpc.respond_error(&id, -32601, "unsupported approval by Fleet driver")
                        .map_err(to_agent_error)
                });
            }
        }
    }

    fn handle_notification(&mut self, frame: Value) {
        if let Some(method) = frame.get("method").and_then(Value::as_str) {
            if method == "serverRequest/resolved" {
                let request_id = frame
                    .get("params")
                    .and_then(|params| params.get("requestId"));
                let native_id = match request_id {
                    Some(Value::Number(number)) => number.to_string(),
                    Some(Value::String(text)) => text.clone(),
                    _ => String::new(),
                };
                if let Some(decision) = self.answered.remove(&native_id) {
                    self.emit(DomainEvent::ApprovalResolved {
                        native_request_id: native_id,
                        decision,
                    });
                }
                return;
            }
        }
        let events = self.mapper.map(&frame);
        for event in events {
            self.local_events.push_back(event);
        }
    }
}

impl AgentDriver for CodexDriver {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn open(&mut self, ctx: LaunchContext) -> Result<String, AgentError> {
        if !ctx.workspace.is_dir() {
            return Err(AgentError {
                code: "invalid_workspace".into(),
                message: "Codex workspace is not a directory".into(),
            });
        }
        let program = codex_program()?;
        let version = detect_version(&program)?;
        if !COMPATIBILITY.compatible(&version) {
            return Err(AgentError {
                code: "incompatible_version".into(),
                message: format!(
                    "Codex {} is outside compatible range >=0.171,<1",
                    version.text
                ),
            });
        }
        self.descriptor = metadata_with(version.clone(), ProviderAvailability::Available);
        self.workspace = Some(ctx.workspace.clone());
        let mut rpc = RpcClient::spawn_with_program(&ctx.workspace, &program).map_err(to_agent_error)?;
        let initialize = rpc
            .call(
                "initialize",
                json!({
                    "clientInfo": {"name": "gentle-fleet", "version": "0.1.0"},
                    "capabilities": {"experimentalApi": false}
                }),
                INIT_TIMEOUT,
            )
            .map_err(to_agent_error)?;
        if let Some(user_agent) = text(&initialize, "userAgent") {
            self.descriptor.version = version;
            self.descriptor.availability = ProviderAvailability::Available;
            let _ = user_agent;
        }
        rpc.notify("initialized", json!({}))
            .map_err(to_agent_error)?;
        if let Ok(models) = rpc.call("model/list", json!({}), CALL_TIMEOUT) {
            let (controls, ids, efforts) = controls_from_model_list(&models);
            self.controls = controls.clone();
            self.descriptor.controls = controls;
            self.model = std::env::var("FLEET_CODEX_MODEL")
                .ok()
                .filter(|v| ids.contains(v));
            self.effort = std::env::var("FLEET_CODEX_EFFORT")
                .ok()
                .filter(|v| efforts.contains(v));
        }
        // Por defecto Codex debe usar sus políticas nativas. Fleet solo
        // anula sandbox/aprobación ante un override explícito del usuario.
        let mut thread_params = json!({"cwd": ctx.workspace.to_string_lossy()});
        if let Ok(value) = std::env::var("FLEET_CODEX_APPROVAL_POLICY") {
            thread_params["approvalPolicy"] = json!(value);
        }
        if let Ok(value) = std::env::var("FLEET_CODEX_SANDBOX") {
            thread_params["sandbox"] = json!(value);
        }
        let started = rpc
            .call("thread/start", thread_params, CALL_TIMEOUT)
            .map_err(to_agent_error)?;
        let thread = started.get("thread").cloned().unwrap_or(started);
        let native_thread = text(&thread, "id").ok_or_else(|| AgentError {
            code: "provider_protocol".into(),
            message: "thread/start did not return a thread id".into(),
        })?;
        self.mapper.set_native_thread(native_thread.clone());
        self.native_thread = Some(native_thread.clone());
        self.rpc = Some(rpc);
        Ok(native_thread)
    }

    fn send(&mut self, input: AgentInput) -> Result<SendReceipt, AgentError> {
        let native_thread = self.native_thread.clone().ok_or_else(|| AgentError {
            code: "not_open".into(),
            message: "Codex thread is not open".into(),
        })?;
        let native_turn = format!("turn-{}", input.turn_id);
        for requested in &input.controls {
            match requested.name.as_str() {
                "model" | "effort" => {
                    let available = self
                        .controls
                        .iter()
                        .find(|control| control.name == requested.name)
                        .map(|control| control.available.clone())
                        .unwrap_or_default();
                    if available.contains(&requested.value) {
                        if requested.name == "model" {
                            self.model = Some(requested.value.clone());
                        } else {
                            self.effort = Some(requested.value.clone());
                        }
                    } else {
                        self.emit(DomainEvent::TurnFailed {
                            turn_id: input.turn_id.clone(),
                            message: format!(
                                "control value `{}` is not available for `{}`",
                                requested.value, requested.name
                            ),
                        });
                        return Ok(SendReceipt { accepted: false });
                    }
                }
                other => {
                    self.emit(DomainEvent::TurnFailed {
                        turn_id: input.turn_id.clone(),
                        message: format!("unsupported turn control: {other}"),
                    });
                    return Ok(SendReceipt { accepted: false });
                }
            }
        }
        let mut params = json!({
            "threadId": native_thread,
            "input": [{"type": "text", "text": input.text}]
        });
        if let Some(model) = &self.model {
            params["model"] = json!(model);
        }
        if let Some(effort) = &self.effort {
            params["effort"] = json!(effort);
        }
        let result = match self.rpc_mut()?.call("turn/start", params, CALL_TIMEOUT) {
            Ok(value) => value,
            Err(error) => {
                // Codex pudo aceptar la solicitud antes de perder el ACK.
                // Nunca reintentar automáticamente ni dejar el turno Running.
                self.emit(DomainEvent::TurnUncertain {
                    turn_id: input.turn_id,
                    reason: format!("native turn/start outcome uncertain: {}", error.code),
                });
                return Ok(SendReceipt { accepted: false });
            }
        };
        let turn = result.get("turn").cloned().unwrap_or(result);
        let returned_native_turn = text(&turn, "id").unwrap_or_else(|| native_turn.clone());
        self.native_turn = Some(returned_native_turn.clone());
        self.mapper.begin_turn(returned_native_turn, input.turn_id);
        // El ACK de turn/start NO es la finalización del turno.
        Ok(SendReceipt { accepted: true })
    }

    fn next_event(&mut self, timeout: Duration) -> Result<Option<AgentEvent>, AgentError> {
        if let Some(event) = self.local_events.pop_front() {
            return Ok(Some(event));
        }
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(request) = self
                .rpc_mut()?
                .recv_server_request(Duration::from_millis(0))
            {
                self.handle_server_request(request);
                return Ok(self.local_events.pop_front());
            }
            if let Some(notification) = self.rpc_mut()?.try_notification() {
                self.handle_notification(notification);
                return Ok(self.local_events.pop_front());
            }
            if !self.announced_dropped_notifications && self.rpc()?.dropped_notifications() > 0 {
                self.announced_dropped_notifications = true;
                self.emit(DomainEvent::AgentNotice {
                    message: "Codex notification buffer saturated; snapshots required".into(),
                });
                return Ok(self.local_events.pop_front());
            }
            if !self.rpc()?.alive() && self.mapper.fleet_turn.is_some() {
                let turn = self.mapper.fleet_turn.clone().unwrap_or_default();
                self.mapper.end_turn();
                self.emit(DomainEvent::TurnFailed {
                    turn_id: turn,
                    message: "Codex App Server closed before a terminal turn result".into(),
                });
                return Ok(self.local_events.pop_front());
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
        args: Vec<fleet_domain::ApprovalArgument>,
    ) -> Result<(), AgentError> {
        let pending = self.pending.get(&request_id).ok_or_else(|| AgentError {
            code: "not_found".into(),
            message: "approval request is no longer pending".into(),
        })?;
        let answer = build_native_answer(
            &pending.method,
            &decision,
            &pending.options,
            &args,
            &pending.params,
        )?;
        let response_id = pending.request_id.clone();
        let method = pending.method.clone();
        self.rpc_mut()?
            .respond(&response_id, answer)
            .map_err(to_agent_error)?;
        self.pending.remove(&request_id);
        // La confirmación llega como `serverRequest/resolved`; no se publica
        // una resolución optimista que el core no aceptaría como nativa.
        let _ = method;
        self.answered.insert(request_id, decision);
        Ok(())
    }

    fn interrupt(&mut self, _turn_id: String) -> Result<(), AgentError> {
        let Some(thread) = self.native_thread.clone() else {
            return Ok(());
        };
        let Some(native_turn) = self.native_turn.clone() else {
            return Ok(());
        };
        self.rpc_mut()?
            .call(
                "turn/interrupt",
                json!({"threadId": thread, "turnId": native_turn}),
                CALL_TIMEOUT,
            )
            .map(|_| ())
            .map_err(to_agent_error)
    }

    fn close(&mut self) -> Result<(), AgentError> {
        self.closed = true;
        match self.rpc.take() {
            Some(mut rpc) => rpc.close().map_err(to_agent_error),
            None => Ok(()),
        }
    }

    fn inspect_recovery(&self) -> RecoveryEvidence {
        let alive = !self.closed && self.rpc.as_ref().is_some_and(|rpc| rpc.alive());
        let stderr_lines = self
            .rpc
            .as_ref()
            .map(|rpc| rpc.stderr_tail().len())
            .unwrap_or(0);
        RecoveryEvidence {
            session_alive: alive,
            detail: format!(
                "mode=owned version={} thread_open={} live_turn_recovery=false stderr_lines={}",
                self.descriptor.version.text,
                self.native_thread.is_some(),
                stderr_lines
            ),
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
        if name == "model" {
            self.model = Some(value.to_string());
        } else {
            self.effort = Some(value.to_string());
        }
        control.current = Some(value.to_string());
        // Codex solo ofrece cambios por turno de forma estable; los cambios
        // dentro de un turno requieren experimentalApi y no se habilitan.
        control.status = if self.mapper.fleet_turn.is_some() {
            "deferred".into()
        } else {
            "applied".into()
        };
        Ok(control)
    }

    fn session_state(&self) -> SessionState {
        if self.closed {
            SessionState::Stopped
        } else if self.native_thread.is_some() {
            SessionState::Running
        } else {
            SessionState::NeedsReconciliation
        }
    }
}

fn native_ref(native_id: &str) -> String {
    // FNV-1a de 64 bits: correlación opaca, no reversible en la API.
    let mut hash = 0xcbf29ce484222325u64;
    for byte in native_id.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("cx-{hash:016x}")
}

fn native_refs() -> &'static std::sync::Mutex<HashMap<String, String>> {
    static REFS: std::sync::OnceLock<std::sync::Mutex<HashMap<String, String>>> =
        std::sync::OnceLock::new();
    REFS.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

fn connect_probe() -> Result<RpcClient, AgentError> {
    let mut rpc = RpcClient::spawn(&std::env::temp_dir()).map_err(to_agent_error)?;
    rpc.call(
        "initialize",
        json!({
            "clientInfo": {"name": "gentle-fleet-discovery", "version": "0.1.0"},
            "capabilities": {"experimentalApi": false}
        }),
        INIT_TIMEOUT,
    )
    .map_err(to_agent_error)?;
    rpc.notify("initialized", json!({}))
        .map_err(to_agent_error)?;
    Ok(rpc)
}

pub struct CodexFactory;

impl Default for CodexFactory {
    fn default() -> Self {
        Self
    }
}

impl DriverFactory for CodexFactory {
    fn id(&self) -> String {
        "codex".into()
    }

    fn compatibility(&self) -> VersionRange {
        COMPATIBILITY
    }

    fn create(&self) -> Box<dyn AgentDriver> {
        Box::new(CodexDriver::new())
    }

    fn metadata(&self) -> ProviderDescriptor {
        metadata_with(
            ProviderVersion::parse("0.0.0"),
            ProviderAvailability::Unknown,
        )
    }

    fn probe(&self) -> ProviderDescriptor {
        CodexDriver::new().probe()
    }

    /// Descubrimiento global de solo lectura. Se lista de threads del
    /// App Server con la autenticación nativa del usuario, sin crear runner
    /// ni sesión Fleet. No se declara exclusividad: la adopción con escritura
    ///permanece bloqueada (F11/F12).
    fn list_native_sessions(&self) -> Result<Vec<NativeSessionInfo>, AgentError> {
        let mut rpc = connect_probe()?;
        let loaded = rpc
            .call("thread/loaded/list", json!({}), CALL_TIMEOUT)
            .map(|value| {
                value
                    .get("data")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        let loaded_ids: Vec<String> = loaded
            .iter()
            .filter_map(|item| text(item, "id").or_else(|| item.as_str().map(str::to_string)))
            .collect();
        let mut result = Vec::new();
        let mut cursor: Option<String> = None;
        let mut seen_cursors = std::collections::HashSet::new();
        loop {
            let mut params = json!({"limit": 100});
            if let Some(value) = &cursor {
                params["cursor"] = json!(value);
            }
            let response = rpc
                .call("thread/list", params, CALL_TIMEOUT)
                .map_err(to_agent_error)?;
            let data = response
                .get("data")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let empty_page = data.is_empty();
            for thread in data {
                let Some(thread_id) = text(&thread, "id") else {
                    continue;
                };
                let reference = native_ref(&thread_id);
                native_refs()
                    .lock()
                    .expect("native refs poisoned")
                    .insert(reference.clone(), thread_id.clone());
                let status = thread
                    .get("status")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        thread
                            .get("status")
                            .and_then(|status| status.get("type"))
                            .and_then(Value::as_str)
                    })
                    .unwrap_or("unknown")
                    .to_string();
                result.push(NativeSessionInfo {
                    native_ref: reference,
                    workspace_hint: text(&thread, "cwd").or_else(|| text(&thread, "path")),
                    state: status.clone(),
                    activity: text(&thread, "updatedAt").unwrap_or_else(|| "unknown".into()),
                    can_resume_history: true,
                    model: text(&thread, "model"),
                    effort: text(&thread, "reasoningEffort"),
                    externally_active: loaded_ids.contains(&thread_id)
                        || status == "active"
                        || thread
                            .get("status")
                            .and_then(|value| value.get("activeFlags"))
                            .and_then(Value::as_array)
                            .is_some_and(|flags| !flags.is_empty()),
                });
            }
            cursor = text(&response, "nextCursor");
            if cursor.is_none() || empty_page {
                break;
            }
            if !seen_cursors.insert(cursor.clone().unwrap_or_default()) {
                return Err(AgentError {
                    code: "PaginationCycle".into(),
                    message: "Codex returned a repeated session cursor".into(),
                });
            }
            if result.len() > 100_000 {
                return Err(AgentError {
                    code: "ResultLimit".into(),
                    message: "Codex session listing exceeds the safe Fleet response limit".into(),
                });
            }
        }
        let _ = rpc.close();
        Ok(result)
    }

    fn import_native_history(
        &self,
        reference: &str,
    ) -> Result<Vec<NativeHistoryMessage>, AgentError> {
        let thread_id = native_refs()
            .lock()
            .expect("native refs poisoned")
            .get(reference)
            .cloned()
            .ok_or_else(|| AgentError {
                code: "not_found".into(),
                message: "native session reference is no longer available; rediscover it".into(),
            })?;
        let mut rpc = connect_probe()?;
        let mut messages = Vec::new();
        let mut cursor: Option<String> = None;
        let mut seen_cursors = std::collections::HashSet::new();
        loop {
            let mut params = json!({
                "threadId": thread_id,
                "limit": 100,
                "itemsView": "full",
                "sortDirection": "asc"
            });
            if let Some(value) = &cursor {
                params["cursor"] = json!(value);
            }
            let turns = rpc
                .call("thread/turns/list", params, CALL_TIMEOUT)
                .map_err(to_agent_error)?;
            let page = turns
                .get("data")
                .and_then(Value::as_array)
                .ok_or_else(|| AgentError {
                    code: "HistoryMalformed".into(),
                    message: "Codex history response has no turn array".into(),
                })?;
            for turn in page {
                let items =
                    turn.get("items")
                        .and_then(Value::as_array)
                        .ok_or_else(|| AgentError {
                            code: "HistoryMalformed".into(),
                            message: "Codex turn history is missing items".into(),
                        })?;
                let turn_id = text(turn, "id").ok_or_else(|| AgentError {
                    code: "HistoryMalformed".into(),
                    message: "Codex history turn has no stable native identity".into(),
                })?;
                for (item_index, item) in items.iter().enumerate() {
                    let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
                    let role = match kind {
                        "userMessage" | "userInput" => "user",
                        "agentMessage" => "assistant",
                        "commandExecution" | "fileChange" | "mcpToolCall" | "dynamicToolCall" => {
                            "tool"
                        }
                        "reasoning" => "reasoning",
                        _ => {
                            return Err(AgentError {
                                code: "HistoryUnsupportedItem".into(),
                                message: "Codex history contains a structured item type Fleet cannot yet import".into(),
                            });
                        }
                    };
                    let content_text = || {
                        item.get("content")
                            .and_then(Value::as_array)
                            .map(|parts| {
                                parts
                                    .iter()
                                    .filter_map(|part| text(part, "text"))
                                    .collect::<Vec<_>>()
                                    .join(" ")
                            })
                            .filter(|value| !value.is_empty())
                    };
                    let body = if role == "tool" {
                        let mut fields = Vec::new();
                        fields.push(format!("tool_type: {kind}"));
                        if let Some(command) = text(item, "command").or_else(|| text(item, "query"))
                        {
                            fields.push(format!("input: {command}"));
                        }
                        if let Some(output) = text(item, "aggregatedOutput")
                            .or_else(|| text(item, "output"))
                            .or_else(|| text(item, "result"))
                        {
                            fields.push(format!("output: {output}"));
                        }
                        if let Some(status) = text(item, "status") {
                            fields.push(format!("status: {status}"));
                        }
                        fields.join("\n")
                    } else {
                        text(item, "text").or_else(content_text).unwrap_or_default()
                    };
                    if body.is_empty() {
                        return Err(AgentError {
                            code: "HistoryMissingContent".into(),
                            message: "Codex history contains an item without recoverable content"
                                .into(),
                        });
                    }
                    let source_ref =
                        native_ref(&format!("{}\0{}\0{}", thread_id, turn_id, item_index));
                    let tool = if role == "tool" {
                        Some(NativeHistoryTool {
                            name: kind.to_string(),
                            input: text(item, "command").or_else(|| text(item, "query")),
                            output: text(item, "aggregatedOutput")
                                .or_else(|| text(item, "output"))
                                .or_else(|| text(item, "result")),
                            status: text(item, "status"),
                        })
                    } else {
                        None
                    };
                    messages.push(NativeHistoryMessage {
                        role: role.into(),
                        text: body,
                        source_ref: Some(source_ref),
                        tool,
                    });
                }
            }
            cursor = text(&turns, "nextCursor");
            if cursor.is_none() {
                break;
            }
            if !seen_cursors.insert(cursor.clone().unwrap_or_default()) {
                return Err(AgentError {
                    code: "HistoryPaginationCycle".into(),
                    message: "Codex returned a repeated history cursor".into(),
                });
            }
            if messages.len() > 100_000 {
                return Err(AgentError {
                    code: "HistoryResultLimit".into(),
                    message: "history exceeds Fleet's safe in-memory page limit".into(),
                });
            }
        }
        let _ = rpc.close();
        Ok(messages)
    }
}

/// Registro de utilidad para tests de contrato que no levantan un servidor.
pub fn register_codex(registry: &mut ProviderRegistry) {
    registry.register(Box::new(CodexFactory));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_codex_resolves_native_exe_without_shell_or_relative_lookup() {
        let dir = std::env::temp_dir().join(format!(
            "fleet codex resolver & {}",
            std::process::id()
        ));
        std::fs::create_dir(&dir).unwrap();
        let executable = dir.join("codex.exe");
        std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
        let shim = dir.join("codex.cmd");
        std::fs::write(&shim, b"@exit /b 1").unwrap();
        let paths = std::env::join_paths([&dir]).unwrap();
        assert_eq!(
            resolve_windows_codex(None, Some(&paths)).unwrap(),
            executable.as_os_str()
        );
        assert_eq!(
            resolve_windows_codex(Some(executable.as_os_str()), None).unwrap(),
            executable.as_os_str()
        );
        assert!(resolve_windows_codex(Some(shim.as_os_str()), Some(&paths)).is_err());
        assert!(resolve_windows_codex(Some(OsStr::new("codex.exe")), Some(&paths)).is_err());
        assert!(resolve_windows_codex(None, Some(OsStr::new("."))).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn factory_declares_only_supported_capabilities() {
        let factory = CodexFactory;
        assert_eq!(factory.id(), "codex");
        assert!(factory.supports(&ProviderVersion::parse("0.171.0")));
        assert!(factory.supports(&ProviderVersion::parse("0.199.0")));
        assert!(!factory.supports(&ProviderVersion::parse("0.170.0")));
        assert!(!factory.supports(&ProviderVersion::parse("1.0.0")));
        let metadata = factory.metadata();
        assert!(metadata.capabilities.approvals);
        assert!(metadata.capabilities.cancel_turn);
        assert!(metadata.capabilities.tool_events);
        assert!(!metadata.capabilities.can_attach_service);
    }

    #[test]
    fn model_and_effort_are_discovered_not_hardcoded() {
        let models = json!({"data":[
            {"id":"model-a","isDefault":true,"supportedReasoningEfforts":[
                {"reasoningEffort":"low"},{"reasoningEffort":"high"}],
             "defaultReasoningEffort":"low"},
            {"id":"model-b","supportedReasoningEfforts":[{"reasoningEffort":"max"}]}
        ]});
        let (controls, ids, efforts) = controls_from_model_list(&models);
        assert_eq!(ids, vec!["model-a", "model-b"]);
        assert_eq!(efforts, vec!["low", "high", "max"]);
        assert_eq!(controls[0].current.as_deref(), Some("model-a"));
        assert_eq!(controls[1].current.as_deref(), Some("low"));
    }

    #[test]
    fn native_rpc_approval_is_forwarded_to_fleet_without_autoapproval() {
        let mut driver = CodexDriver::new();
        driver.handle_server_request(rpc::ServerRequest {
            id: json!(42),
            method: "item/commandExecution/requestApproval".into(),
            params: json!({
                "command": "printf FLEET_APPROVAL_PROBE",
                "availableDecisions": ["accept", "decline"],
            }),
        });
        assert!(driver.pending.contains_key("42"));
        assert!(driver.answered.is_empty());
        let event = driver
            .local_events
            .pop_front()
            .expect("approval event expected");
        match event.kind {
            DomainEvent::ApprovalRequested {
                native_request_id,
                allowed_decisions,
                ..
            } => {
                assert_eq!(native_request_id, "42");
                assert_eq!(allowed_decisions, vec!["accept", "decline"]);
            }
            _ => panic!("native request was not forwarded as an approval"),
        }
    }

    #[test]
    fn driver_without_open_has_no_native_ids() {
        let driver = CodexDriver::new();
        let evidence = driver.inspect_recovery();
        assert!(!evidence.session_alive);
        assert_eq!(driver.session_state(), SessionState::NeedsReconciliation);
    }
}

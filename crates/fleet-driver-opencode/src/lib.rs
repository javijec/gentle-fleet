//! Driver OpenCode real contra la API HTTP/SSE investigada en P0.
//!
//! El `sessionID`, `messageID`, `assistantMessageID` y demases IDs nativos se
//! conservan aquí: nunca aparecen en `AgentEvent` ni en el wire Fleet. El
//! SessionManager no importa este crate.

use std::collections::{HashSet, VecDeque};
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use fleet_agent::{
    AgentDriver, AgentError, AgentEvent, AgentInput, DomainEvent, DriverFactory, LaunchContext,
    RecoveryEvidence, SendReceipt,
};
use fleet_domain::{
    ProviderAvailability, ProviderCapabilities, ProviderDescriptor, ProviderVersion, RunnerMode,
    SessionState, TurnStatus, VersionRange,
};
use reqwest::blocking::Client;
use serde_json::{json, Value};

/// 2.3.x y versiones 2.y posteriores son compatibles; 3.x exige nueva fixture.
pub const COMPATIBILITY: VersionRange = VersionRange {
    min_major: 2,
    min_minor: 3,
    max_major_exclusive: 3,
};

pub struct OpenCodeDriver {
    descriptor: ProviderDescriptor,
    mode: RunnerMode,
    workspace: Option<PathBuf>,
    base_url: Option<String>,
    password: Option<String>,
    native_session_id: Option<String>,
    child: Option<Child>,
    stop: Arc<AtomicBool>,
    events_rx: Option<Receiver<Value>>,
    local_events: VecDeque<AgentEvent>,
    stream_pending: VecDeque<AgentEvent>,
    current_turn: Option<String>,
    last_tool: Option<String>,
    turn_finished: bool,
    // Evidencia nativa por ejecución: evita asignar eventos viejos al turno
    // nuevo basándose únicamente en un current_turn mutable.
    execution_started: bool,
    execution_start_seq: Option<u64>,
    last_terminal_seq: Option<u64>,
    active_assistant_messages: HashSet<String>,
    ask_shell: bool,
}

impl Default for OpenCodeDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenCodeDriver {
    pub fn new() -> Self {
        let capabilities = ProviderCapabilities {
            streaming_text: true,
            tool_events: true,
            session_history: true,
            native_turn_ids: false,
            cancel_turn: false,
            approvals: true,
            can_own_process: true,
            can_attach_service: true,
        };
        Self {
            descriptor: ProviderDescriptor {
                id: "opencode".into(),
                version: ProviderVersion::parse("0.0.0"),
                capabilities,
                mode: RunnerMode::Auto,
                availability: ProviderAvailability::Unknown,
                controls: vec![],
            },
            mode: RunnerMode::Auto,
            workspace: None,
            base_url: None,
            password: None,
            native_session_id: None,
            child: None,
            stop: Arc::new(AtomicBool::new(false)),
            events_rx: None,
            local_events: VecDeque::new(),
            stream_pending: VecDeque::new(),
            current_turn: None,
            last_tool: None,
            turn_finished: false,
            execution_started: false,
            execution_start_seq: None,
            last_terminal_seq: None,
            active_assistant_messages: HashSet::new(),
            ask_shell: false,
        }
    }

    /// Configura la regla de permisos `shell: ask` usada en los smokes reales.
    pub fn with_ask_shell(mut self, ask_shell: bool) -> Self {
        self.ask_shell = ask_shell;
        self
    }

    /// Construye un driver que levanta su propio `opencode serve` (útil para
    /// pruebas aisladas). Por defecto usa `Auto`: attach si hay servicio.
    pub fn with_mode(mode: RunnerMode) -> Self {
        let mut driver = Self::new();
        driver.mode = mode;
        driver
    }

    fn service_state_path() -> Option<PathBuf> {
        let state = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| fleet_agent::process::user_home().map(|home| home.join(".local/state")))?;
        state
            .is_absolute()
            .then(|| state.join("opencode").join("service.json"))
    }

    fn auth(&self) -> Result<String, AgentError> {
        let password = self.password.as_ref().ok_or_else(|| AgentError {
            code: "not_open".into(),
            message: "OpenCode driver has no server password".into(),
        })?;
        let token = base64::engine::general_purpose::STANDARD
            .encode(format!("opencode:{password}").as_bytes());
        Ok(format!("Basic {token}"))
    }

    fn attach_to_service(&mut self) -> Result<bool, AgentError> {
        let Some(path) = Self::service_state_path() else {
            return Ok(false);
        };
        if !path.is_file() {
            return Ok(false);
        }
        let raw = std::fs::read_to_string(&path).map_err(|e| AgentError {
            code: "service_state_unreadable".into(),
            message: format!("cannot read {:?}: {e}", path.file_name()),
        })?;
        let state: Value = serde_json::from_str(&raw).map_err(|e| AgentError {
            code: "service_state_invalid".into(),
            message: format!("service.json is not JSON: {e}"),
        })?;
        let url = state
            .get("url")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AgentError {
                code: "service_state_invalid".into(),
                message: "service.json has no url".into(),
            })?;
        let password = state
            .get("password")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AgentError {
                code: "service_state_invalid".into(),
                message: "service.json has no password".into(),
            })?;
        self.base_url = Some(url.trim_end_matches('/').to_string());
        self.password = Some(password.to_string());
        self.descriptor.mode = RunnerMode::Attached;
        Ok(true)
    }

    fn spawn_owned_server(&mut self) -> Result<(), AgentError> {
        let workspace = self.workspace.clone().ok_or_else(|| AgentError {
            code: "invalid_workspace".into(),
            message: "OpenCode driver was opened without a workspace".into(),
        })?;
        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| AgentError {
            code: "bind_failed".into(),
            message: format!("cannot allocate loopback port: {e}"),
        })?;
        let port = listener
            .local_addr()
            .map_err(|e| AgentError {
                code: "bind_failed".into(),
                message: format!("cannot read loopback port: {e}"),
            })?
            .port();
        drop(listener);

        let mut command =
            fleet_agent::process::provider_command("opencode", "FLEET_OPENCODE_BIN", None)?;
        let mut child = command
            .args([
                "serve",
                "--hostname",
                "127.0.0.1",
                "--port",
                &port.to_string(),
            ])
            .current_dir(&workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| AgentError {
                code: "provider_unavailable".into(),
                message: format!("cannot start `opencode serve`: {e}"),
            })?;
        let stdout = child.stdout.take().ok_or_else(|| AgentError {
            code: "provider_unavailable".into(),
            message: "opencode serve stdout is missing".into(),
        })?;
        let (tx, rx) = mpsc::sync_channel(32);
        std::thread::spawn(move || {
            // After readiness, keep draining stdout instead of closing the
            // provider's pipe. Do not retain or publish server output.
            let mut startup = true;
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if startup && tx.send(line).is_err() {
                    startup = false;
                }
            }
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(25);
        let mut password = None;
        let mut listening = false;
        while std::time::Instant::now() < deadline && !(listening && password.is_some()) {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(line) => {
                    if let Some(rest) = line.strip_prefix("server password ") {
                        password = Some(rest.trim().to_string());
                    }
                    if line.starts_with("server listening on") {
                        listening = true;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        let Some(password) = password.filter(|value| !value.is_empty() && listening) else {
            drop(rx);
            let _ = child.kill();
            let _ = child.wait();
            return Err(AgentError {
                code: "provider_unavailable".into(),
                message: "opencode serve did not announce readiness and password".into(),
            });
        };
        self.base_url = Some(format!("http://127.0.0.1:{port}"));
        self.password = Some(password);
        self.child = Some(child);
        self.descriptor.mode = RunnerMode::Owned;
        Ok(())
    }

    fn client(&self) -> Client {
        Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_else(|_| Client::new())
    }

    fn check_version(&mut self) -> Result<(), AgentError> {
        let base = self.base_url.clone().expect("endpoint resolved");
        let auth = self.auth()?;
        let response = self
            .client()
            .get(format!("{base}/api/info"))
            .header("Authorization", &auth)
            .send()
            .map_err(|e| AgentError {
                code: "provider_unavailable".into(),
                message: format!("GET /api/info failed: {e}"),
            })?;
        if !response.status().is_success() {
            return Err(AgentError {
                code: "unauthorized".into(),
                message: format!("GET /api/info returned {}", response.status()),
            });
        }
        let info: Value = response.json().map_err(|e| AgentError {
            code: "provider_protocol".into(),
            message: format!("invalid /api/info JSON: {e}"),
        })?;
        let version = info
            .get("version")
            .and_then(Value::as_str)
            .ok_or_else(|| AgentError {
                code: "provider_protocol".into(),
                message: "/api/info has no version".into(),
            })?;
        self.descriptor.version = ProviderVersion::parse(version);
        if !COMPATIBILITY.compatible(&self.descriptor.version) {
            self.descriptor.availability = ProviderAvailability::Unavailable;
            return Err(AgentError {
                code: "incompatible_version".into(),
                message: format!(
                    "OpenCode {} is outside compatible range >=2.3,<3",
                    self.descriptor.version.text
                ),
            });
        }
        self.descriptor.availability = ProviderAvailability::Available;
        Ok(())
    }

    fn create_native_session(&mut self) -> Result<String, AgentError> {
        let base = self.base_url.clone().expect("endpoint resolved");
        let auth = self.auth()?;
        let workspace = self.workspace.clone().expect("workspace set");
        let response = self
            .client()
            .post(format!("{base}/api/session"))
            .header("Authorization", &auth)
            .json(&json!({
                "title": "gentle-fleet-p2",
                "location": { "directory": workspace.to_string_lossy() },
                "permissions": if self.ask_shell {
                    json!([{"action":"shell","resource":"*","effect":"ask"}])
                } else {
                    json!([])
                }
            }))
            .send()
            .map_err(|e| AgentError {
                code: "provider_unavailable".into(),
                message: format!("POST /api/session failed: {e}"),
            })?;
        if !response.status().is_success() {
            return Err(AgentError {
                code: "provider_protocol".into(),
                message: format!("POST /api/session returned {}", response.status()),
            });
        }
        let body: Value = response.json().map_err(|e| AgentError {
            code: "provider_protocol".into(),
            message: format!("invalid /api/session JSON: {e}"),
        })?;
        let id = body
            .get("data")
            .and_then(|d| d.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| AgentError {
                code: "provider_protocol".into(),
                message: "/api/session response has no data.id".into(),
            })?
            .to_string();
        Ok(id)
    }

    fn start_event_stream(&mut self) -> Result<(), AgentError> {
        let base = self.base_url.clone().expect("endpoint resolved");
        let auth = self.auth()?;
        let stop = Arc::clone(&self.stop);
        let (tx, rx): (Sender<Value>, Receiver<Value>) = mpsc::channel();
        std::thread::spawn(move || {
            let client = Client::builder().build().unwrap_or_else(|_| Client::new());
            let response = match client
                .get(format!("{base}/api/event"))
                .header("Authorization", &auth)
                .header("Accept", "text/event-stream")
                .send()
            {
                Ok(response) => response,
                Err(error) => {
                    let _ = tx.send(json!({
                        "type": "fleet.stream_error",
                        "data": { "message": format!("SSE connection failed: {error}") }
                    }));
                    return;
                }
            };
            for line in BufReader::new(response).lines().map_while(Result::ok) {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let Some(payload) = line.strip_prefix("data: ") else {
                    continue;
                };
                let Ok(value) = serde_json::from_str::<Value>(payload) else {
                    continue;
                };
                if tx.send(value).is_err() {
                    break;
                }
            }
        });
        self.events_rx = Some(rx);
        Ok(())
    }
}

/// Traduce un evento SSE a cero o más eventos de dominio Fleet.
/// Los IDs nativos se descartan aquí; el turno Fleet se inyecta después.
fn map_stream_event(value: &Value, native_session_id: &str) -> Option<Vec<AgentEvent>> {
    let kind = value.get("type")?.as_str()?;
    let data = value.get("data")?;
    if kind == "fleet.stream_error" {
        return Some(vec![AgentEvent {
            kind: DomainEvent::TurnFailed {
                turn_id: "unknown".into(),
                message: data
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("stream error")
                    .to_string(),
            },
        }]);
    }
    if data.get("sessionID").and_then(Value::as_str) != Some(native_session_id) {
        return None;
    }
    let turn = "unknown".to_string();
    let text_field = |name: &str| {
        data.get(name)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let events = match kind {
        "session.text.delta" => vec![DomainEvent::AssistantDelta {
            turn_id: turn,
            text: text_field("delta"),
        }],
        "session.text.ended" => vec![DomainEvent::AssistantMessage {
            turn_id: turn,
            text: text_field("text"),
        }],
        // `session.tool.input.started` es el único evento que trae `name`;
        // `session.tool.called` solo trae `input`, por eso no se usa aquí.
        "session.tool.input.started" => vec![DomainEvent::ToolStarted {
            turn_id: turn,
            name: text_field("name"),
            input: String::new(),
        }],
        "session.tool.called" => return None,
        "session.tool.success" => {
            let output = data
                .get("content")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            vec![
                DomainEvent::ToolOutput {
                    turn_id: turn.clone(),
                    name: String::new(),
                    output,
                },
                DomainEvent::ToolCompleted {
                    turn_id: turn,
                    name: String::new(),
                    success: true,
                },
            ]
        }
        "session.tool.failed" => {
            let message = data
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("tool failed")
                .to_string();
            vec![
                DomainEvent::ToolOutput {
                    turn_id: turn.clone(),
                    name: String::new(),
                    output: message,
                },
                DomainEvent::ToolCompleted {
                    turn_id: turn,
                    name: String::new(),
                    success: false,
                },
            ]
        }
        "session.execution.succeeded" => vec![DomainEvent::TurnCompleted {
            turn_id: turn,
            status: TurnStatus::Completed,
        }],
        "session.execution.failed" => vec![DomainEvent::TurnCompleted {
            turn_id: turn,
            status: TurnStatus::Failed,
        }],
        "session.execution.interrupted" => vec![DomainEvent::TurnCompleted {
            turn_id: turn,
            status: TurnStatus::Interrupted,
        }],
        "permission.asked" => vec![DomainEvent::ApprovalRequested {
            native_request_id: text_field("id"),
            kind: text_field("action"),
            message: text_field("message"),
            resources: data
                .get("resources")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            allowed_decisions: vec!["once".into(), "always".into(), "reject".into()],
            options: vec![
                fleet_domain::ApprovalOptionSpec {
                    option_id: "once".into(),
                    label: "Allow once".into(),
                    args: vec![],
                },
                fleet_domain::ApprovalOptionSpec {
                    option_id: "always".into(),
                    label: "Allow for this session".into(),
                    args: vec![],
                },
                fleet_domain::ApprovalOptionSpec {
                    option_id: "reject".into(),
                    label: "Deny".into(),
                    args: vec![],
                },
            ],
        }],
        "permission.replied" => vec![DomainEvent::ApprovalResolved {
            native_request_id: text_field("requestID"),
            decision: text_field("reply"),
        }],
        _ => return None,
    };
    Some(events.into_iter().map(|kind| AgentEvent { kind }).collect())
}

impl OpenCodeDriver {
    /// Comprobación barata para `ListAgents`: no crea sesión ni levanta un
    /// servidor. Detecta versión real cuando el servicio de usuario responde.
    pub fn probe(&mut self) -> ProviderDescriptor {
        match self.attach_to_service() {
            Ok(true) => {
                if self.check_version().is_err() {
                    self.descriptor.availability = ProviderAvailability::Unavailable;
                }
            }
            Ok(false) => {
                self.descriptor.mode = RunnerMode::Auto;
                self.descriptor.availability = ProviderAvailability::Unavailable;
                // Installation evidence is not proof of a compatible service.
                #[cfg(windows)]
                if let Ok(stdout) =
                    fleet_agent::process::provider_command("opencode", "FLEET_OPENCODE_BIN", None)
                        .and_then(|command| fleet_agent::process::version_stdout(&command))
                {
                    let version = String::from_utf8_lossy(&stdout);
                    if let Some(part) = version
                        .split_whitespace()
                        .find(|part| part.chars().next().is_some_and(|c| c.is_ascii_digit()))
                    {
                        self.descriptor.version = ProviderVersion::parse(part.to_owned());
                    }
                }
            }
            Err(_) => {
                self.descriptor.availability = ProviderAvailability::Unavailable;
            }
        }
        self.descriptor.clone()
    }
}

impl AgentDriver for OpenCodeDriver {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn open(&mut self, ctx: LaunchContext) -> Result<String, AgentError> {
        self.workspace = Some(ctx.workspace.clone());
        self.mode = ctx.mode;
        let attached = match ctx.mode {
            RunnerMode::Attached => self.attach_to_service()?,
            RunnerMode::Owned => false,
            RunnerMode::Auto => self.attach_to_service()?,
        };
        if !attached {
            self.spawn_owned_server()?;
        }
        let result = (|| {
            self.check_version()?;
            let native_id = self.create_native_session()?;
            self.native_session_id = Some(native_id.clone());
            self.start_event_stream()?;
            Ok(native_id)
        })();
        if result.is_err() && self.child.is_some() {
            // No attached service is terminated by startup cleanup.
            let _ = self.close();
        }
        result
    }

    fn send(&mut self, input: AgentInput) -> Result<SendReceipt, AgentError> {
        let base = self.base_url.clone().ok_or_else(|| AgentError {
            code: "not_open".into(),
            message: "OpenCode driver is not open".into(),
        })?;
        let auth = self.auth()?;
        let native_id = self.native_session_id.clone().ok_or_else(|| AgentError {
            code: "not_open".into(),
            message: "OpenCode driver has no native session".into(),
        })?;
        let response = self
            .client()
            .post(format!("{base}/api/session/{native_id}/prompt"))
            .header("Authorization", &auth)
            .json(&json!({ "text": input.text }))
            .send()
            .map_err(|e| AgentError {
                code: "provider_unavailable".into(),
                message: format!("POST /prompt failed: {e}"),
            })?;
        if !response.status().is_success() {
            return Err(AgentError {
                code: "provider_protocol".into(),
                message: format!("POST /prompt returned {}", response.status()),
            });
        }
        self.current_turn = Some(input.turn_id.clone());
        self.turn_finished = false;
        self.execution_started = false;
        self.execution_start_seq = None;
        self.active_assistant_messages.clear();
        self.last_tool = None;
        Ok(SendReceipt { accepted: true })
    }

    fn answer(&mut self, native_request_id: String, decision: String) -> Result<(), AgentError> {
        let base = self.base_url.clone().ok_or_else(|| AgentError {
            code: "not_open".into(),
            message: "OpenCode driver is not open".into(),
        })?;
        let auth = self.auth()?;
        let native_session = self.native_session_id.clone().ok_or_else(|| AgentError {
            code: "not_open".into(),
            message: "OpenCode driver has no native session".into(),
        })?;
        let response = self
            .client()
            .post(format!(
                "{base}/api/session/{native_session}/permission/{native_request_id}/reply"
            ))
            .header("Authorization", &auth)
            .json(&json!({ "decision": decision }))
            .send()
            .map_err(|e| AgentError {
                code: "approval_delivery_failed".into(),
                message: format!("permission reply failed: {e}"),
            })?;
        if !response.status().is_success() {
            return Err(AgentError {
                code: "approval_delivery_failed".into(),
                message: format!("permission reply returned {}", response.status()),
            });
        }
        self.local_events.push_back(AgentEvent {
            kind: DomainEvent::ApprovalResolved {
                native_request_id,
                decision,
            },
        });
        Ok(())
    }

    fn next_event(&mut self, timeout: Duration) -> Result<Option<AgentEvent>, AgentError> {
        if let Some(event) = self.local_events.pop_front() {
            return Ok(Some(event));
        }
        if self.stream_pending.is_empty() {
            let Some(rx) = self.events_rx.as_ref() else {
                return Ok(None);
            };
            let raw: Value = match rx.recv_timeout(timeout) {
                Ok(value) => value,
                Err(mpsc::RecvTimeoutError::Timeout) => return Ok(None),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if let Some(turn) = self.current_turn.clone() {
                        self.current_turn = None;
                        return Ok(Some(AgentEvent {
                            kind: DomainEvent::TurnFailed {
                                turn_id: turn,
                                message: "OpenCode SSE stream closed before a terminal result"
                                    .into(),
                            },
                        }));
                    }
                    return Ok(None);
                }
            };
            let native = self.native_session_id.clone().unwrap_or_default();
            let kind = raw.get("type").and_then(Value::as_str).unwrap_or("");
            let data = raw.get("data").unwrap_or(&Value::Null);
            let native_seq = raw
                .get("durable")
                .and_then(|durable| durable.get("seq"))
                .and_then(Value::as_u64);

            // Para esta sesión, cada ejecución debe comenzar después del
            // último terminal nativo conocido. No usar un step como inicio.
            if kind == "session.execution.started" {
                if self.current_turn.is_some()
                    && !self.execution_started
                    && native_seq
                        .zip(self.last_terminal_seq)
                        .is_none_or(|(seq, previous)| seq > previous)
                    && data.get("sessionID").and_then(Value::as_str) == Some(native.as_str())
                {
                    self.execution_started = true;
                    self.execution_start_seq = native_seq;
                }
                return Ok(None);
            }
            if kind == "session.step.started" {
                if self.execution_started
                    && data.get("sessionID").and_then(Value::as_str) == Some(native.as_str())
                {
                    if let Some(message_id) = data.get("assistantMessageID").and_then(Value::as_str)
                    {
                        self.active_assistant_messages
                            .insert(message_id.to_string());
                    }
                }
                return Ok(None);
            }

            if kind != "fleet.stream_error" {
                if !self.execution_started || self.current_turn.is_none() {
                    return Ok(None);
                }
                if let Some(seq) = native_seq {
                    if self
                        .last_terminal_seq
                        .is_some_and(|previous| seq <= previous)
                        || self.execution_start_seq.is_some_and(|start| seq < start)
                    {
                        return Ok(None);
                    }
                }
                // Deltas y herramientas llevan assistantMessageID. No atribuir
                // a este turno los mensajes de una ejecución anterior.
                if kind.starts_with("session.text.") || kind.starts_with("session.tool.") {
                    let belongs = data
                        .get("assistantMessageID")
                        .and_then(Value::as_str)
                        .is_some_and(|id| self.active_assistant_messages.contains(id));
                    if !belongs {
                        return Ok(None);
                    }
                }
                if matches!(
                    kind,
                    "session.execution.succeeded"
                        | "session.execution.failed"
                        | "session.execution.interrupted"
                ) {
                    // Sin frontera nativa verificable no fingimos correlación.
                    if self
                        .execution_start_seq
                        .zip(native_seq)
                        .is_none_or(|(start, end)| end <= start)
                    {
                        return Ok(None);
                    }
                    self.last_terminal_seq = native_seq;
                }
            }
            if let Some(events) = map_stream_event(&raw, &native) {
                self.stream_pending.extend(events);
            }
        }
        let Some(mut event) = self.stream_pending.pop_front() else {
            return Ok(None);
        };
        let turn_scoped = matches!(
            event.kind,
            DomainEvent::TurnStarted { .. }
                | DomainEvent::AssistantDelta { .. }
                | DomainEvent::AssistantMessage { .. }
                | DomainEvent::ToolStarted { .. }
                | DomainEvent::ToolOutput { .. }
                | DomainEvent::ToolCompleted { .. }
                | DomainEvent::TurnCompleted { .. }
                | DomainEvent::TurnFailed { .. }
        );
        if turn_scoped && self.current_turn.is_none() {
            return Ok(None); // evento tardío de un turno anterior
        }
        let terminal = matches!(
            event.kind,
            DomainEvent::TurnCompleted { .. } | DomainEvent::TurnFailed { .. }
        );
        if terminal {
            if self.turn_finished {
                return Ok(None);
            }
            self.turn_finished = true;
        }
        if let Some(turn) = self.current_turn.clone() {
            match &mut event.kind {
                DomainEvent::AssistantDelta { turn_id, .. }
                | DomainEvent::AssistantMessage { turn_id, .. }
                | DomainEvent::ToolStarted { turn_id, .. }
                | DomainEvent::ToolOutput { turn_id, .. }
                | DomainEvent::ToolCompleted { turn_id, .. }
                | DomainEvent::TurnCompleted { turn_id, .. }
                | DomainEvent::TurnFailed { turn_id, .. }
                | DomainEvent::TurnStarted { turn_id, .. } => *turn_id = turn,
                _ => {}
            }
        }
        if let Some(tool) = self.last_tool.clone() {
            match &mut event.kind {
                DomainEvent::ToolStarted { name, .. } => *name = tool.clone(),
                DomainEvent::ToolOutput { name, .. } | DomainEvent::ToolCompleted { name, .. }
                    if name.is_empty() =>
                {
                    *name = tool.clone();
                }
                _ => {}
            }
        }
        if let DomainEvent::ToolStarted { name, .. } = &event.kind {
            self.last_tool = Some(name.clone());
        }
        if terminal {
            self.current_turn = None;
            self.execution_started = false;
            self.execution_start_seq = None;
            self.active_assistant_messages.clear();
        }
        Ok(Some(event))
    }

    fn close(&mut self) -> Result<(), AgentError> {
        self.stop.store(true, Ordering::Relaxed);
        let mut failure = None;
        if let (Some(base), Some(native)) = (&self.base_url, &self.native_session_id) {
            match self.auth() {
                Ok(auth) => match self
                    .client()
                    .delete(format!("{base}/api/session/{native}"))
                    .header("Authorization", &auth)
                    .send()
                {
                    Ok(response)
                        if response.status().is_success()
                            || response.status() == reqwest::StatusCode::NOT_FOUND => {}
                    Ok(response) => {
                        failure = Some(format!("DELETE session returned {}", response.status()))
                    }
                    Err(error) => failure = Some(format!("DELETE session failed: {error}")),
                },
                Err(error) => failure = Some(error.to_string()),
            }
        } else {
            failure = Some("native OpenCode session was not known".into());
        }

        // Owned y Attached son distintos: nunca matar el servicio compartido.
        if let Some(mut child) = self.child.take() {
            if let Err(error) = child.kill() {
                failure.get_or_insert(format!("owned server kill failed: {error}"));
            }
            if let Err(error) = child.wait() {
                failure.get_or_insert(format!("owned server wait failed: {error}"));
            }
        }
        if let Some(message) = failure {
            return Err(AgentError {
                code: "StopUnconfirmed".into(),
                message,
            });
        }
        self.native_session_id = None;
        Ok(())
    }

    fn inspect_recovery(&self) -> RecoveryEvidence {
        // A native session ID alone does not prove the service is alive.
        let session_alive = match (&self.base_url, &self.native_session_id, self.auth()) {
            (Some(base), Some(native), Ok(auth)) => self
                .client()
                .get(format!("{base}/api/session/{native}"))
                .header("Authorization", auth)
                .timeout(Duration::from_secs(2))
                .send()
                .is_ok_and(|response| response.status().is_success()),
            _ => false,
        };
        RecoveryEvidence {
            session_alive,
            detail: format!(
                "mode={:?} version={} native_session_reachable={session_alive}",
                self.descriptor.mode, self.descriptor.version.text,
            ),
        }
    }

    fn session_state(&self) -> SessionState {
        if self.native_session_id.is_some() {
            SessionState::Running
        } else {
            SessionState::Stopped
        }
    }
}

pub struct OpenCodeFactory {
    ask_shell: bool,
}

impl OpenCodeFactory {
    pub fn new() -> Self {
        Self { ask_shell: false }
    }

    pub fn with_ask_shell(ask_shell: bool) -> Self {
        Self { ask_shell }
    }
}

impl Default for OpenCodeFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl DriverFactory for OpenCodeFactory {
    fn id(&self) -> String {
        "opencode".into()
    }
    fn compatibility(&self) -> VersionRange {
        COMPATIBILITY
    }
    fn create(&self) -> Box<dyn AgentDriver> {
        Box::new(OpenCodeDriver::new().with_ask_shell(self.ask_shell))
    }

    fn metadata(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            id: "opencode".into(),
            version: ProviderVersion::parse("0.0.0"),
            capabilities: ProviderCapabilities {
                streaming_text: true,
                tool_events: true,
                session_history: true,
                native_turn_ids: false,
                cancel_turn: false,
                approvals: true,
                can_own_process: true,
                can_attach_service: true,
            },
            mode: RunnerMode::Auto,
            availability: ProviderAvailability::Unknown,
            controls: vec![],
        }
    }

    fn probe(&self) -> ProviderDescriptor {
        OpenCodeDriver::new().with_ask_shell(self.ask_shell).probe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn late_events_do_not_switch_to_the_next_turn() {
        let mut driver = OpenCodeDriver::new();
        driver.native_session_id = Some("ses_one".into());
        driver.current_turn = Some("turn-new".into());
        driver.last_terminal_seq = Some(20);
        let (tx, rx) = mpsc::channel();
        driver.events_rx = Some(rx);
        let event = |kind: &str, seq: u64, data: Value| json!({"type": kind, "data": data, "durable": {"seq": seq}});
        // Replay de un comienzo anterior, anterior al terminal conocido.
        tx.send(event(
            "session.execution.started",
            10,
            json!({"sessionID":"ses_one"}),
        ))
        .unwrap();
        assert!(driver
            .next_event(Duration::from_millis(5))
            .unwrap()
            .is_none());
        assert!(!driver.execution_started);
        tx.send(event(
            "session.execution.started",
            21,
            json!({"sessionID":"ses_one"}),
        ))
        .unwrap();
        assert!(driver
            .next_event(Duration::from_millis(5))
            .unwrap()
            .is_none());
        assert!(driver.execution_started);
        tx.send(event(
            "session.step.started",
            22,
            json!({
                "sessionID":"ses_one", "assistantMessageID":"msg_new"
            }),
        ))
        .unwrap();
        assert!(driver
            .next_event(Duration::from_millis(5))
            .unwrap()
            .is_none());
        tx.send(event(
            "session.text.delta",
            23,
            json!({
                "sessionID":"ses_one", "assistantMessageID":"msg_old", "delta":"wrong"
            }),
        ))
        .unwrap();
        assert!(driver
            .next_event(Duration::from_millis(5))
            .unwrap()
            .is_none());
        tx.send(event(
            "session.text.delta",
            24,
            json!({
                "sessionID":"ses_one", "assistantMessageID":"msg_new", "delta":"right"
            }),
        ))
        .unwrap();
        let right = driver
            .next_event(Duration::from_millis(5))
            .unwrap()
            .unwrap();
        assert!(
            matches!(right.kind, DomainEvent::AssistantDelta { ref turn_id, ref text }
            if turn_id == "turn-new" && text == "right")
        );
        tx.send(event(
            "session.execution.succeeded",
            25,
            json!({"sessionID":"ses_one"}),
        ))
        .unwrap();
        let done = driver
            .next_event(Duration::from_millis(5))
            .unwrap()
            .unwrap();
        assert!(
            matches!(done.kind, DomainEvent::TurnCompleted { ref turn_id, .. }
            if turn_id == "turn-new")
        );
        assert!(driver.current_turn.is_none());
    }

    #[test]
    fn maps_stream_events_and_keeps_native_ids_private() {
        let native = "ses_native_secret";
        let delta = json!({
            "type": "session.text.delta",
            "data": {"sessionID": native, "delta": "hello"}
        });
        let events = map_stream_event(&delta, native).unwrap();
        assert!(matches!(events[0].kind, DomainEvent::AssistantDelta { .. }));
        let other = json!({
            "type": "session.text.delta",
            "data": {"sessionID": "ses_other", "delta": "hidden"}
        });
        assert!(map_stream_event(&other, native).is_none());
        let factory = OpenCodeFactory::new();
        assert!(factory.supports(&ProviderVersion::parse("2.4.0")));
        assert!(!factory.supports(&ProviderVersion::parse("3.0.0")));
    }

    #[test]
    fn maps_permission_ask_and_native_confirmation() {
        let native = "ses_permission_session";
        let asked = json!({
            "type": "permission.asked",
            "data": {
                "sessionID": native,
                "id": "per_native",
                "action": "shell",
                "message": "allow?",
                "resources": ["echo hi"],
                "save": ["echo *"]
            }
        });
        let events = map_stream_event(&asked, native).unwrap();
        match &events[0].kind {
            DomainEvent::ApprovalRequested {
                native_request_id,
                kind,
                resources,
                allowed_decisions,
                ..
            } => {
                assert_eq!(native_request_id, "per_native");
                assert_eq!(kind, "shell");
                assert_eq!(resources, &vec!["echo hi".to_string()]);
                assert!(allowed_decisions.contains(&"once".to_string()));
                assert!(allowed_decisions.contains(&"reject".to_string()));
            }
            other => panic!("unexpected event: {other:?}"),
        }
        let replied = json!({
            "type": "permission.replied",
            "data": {
                "sessionID": native,
                "requestID": "per_native",
                "reply": "reject"
            }
        });
        let events = map_stream_event(&replied, native).unwrap();
        match &events[0].kind {
            DomainEvent::ApprovalResolved {
                native_request_id,
                decision,
            } => {
                assert_eq!(native_request_id, "per_native");
                assert_eq!(decision, "reject");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
}

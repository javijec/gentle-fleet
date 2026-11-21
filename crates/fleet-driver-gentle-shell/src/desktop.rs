//! Cliente `gentle-shell.desktop-rpc/v1` sobre Extension UI.
//!
//! La respuesta de dominio NO es la respuesta del comando `prompt` Pi: llega
//! como `extension_ui_request{method:"setWidget", widgetKey:"gentle-desktop-rpc"}`
//! y se correlaciona por `requestId` del envelope Desktop RPC.

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

use fleet_agent::AgentError;
use fleet_rpc_jsonl::{JsonlRpc, UiRequest};
use serde_json::{json, Value};

pub const SCHEMA: &str = "gentle-shell.desktop-rpc/v1";
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
pub const MAX_REQUEST_ID: usize = 128;
pub const MAX_METHOD: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopError {
    pub code: String,
    pub message: String,
}

impl DesktopError {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl From<DesktopError> for AgentError {
    fn from(value: DesktopError) -> Self {
        AgentError {
            code: value.code,
            message: value.message,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DesktopCapabilities {
    pub methods: Vec<String>,
    pub events: Vec<String>,
}

impl DesktopCapabilities {
    pub fn supports(&self, method: &str) -> bool {
        self.methods.iter().any(|item| item == method)
    }

    pub fn from_response(data: &Value) -> Result<Self, DesktopError> {
        let methods = data
            .get("methods")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .ok_or_else(|| DesktopError::new("CapabilitiesMalformed", "methods array missing"))?;
        let events = data
            .get("events")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self { methods, events })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopEvent {
    pub seq: u64,
    pub event: String,
    pub data: Value,
}

/// Estado de la secuencia nativa de eventos Desktop RPC.
#[derive(Debug, Clone, Default)]
pub struct SequenceTracker {
    expected: Option<u64>,
    reconciliations: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SequenceOutcome {
    Event,
    Duplicate,
    Gap,
    Reset,
}

impl SequenceTracker {
    /// El contador por sí solo no demuestra un reset: seq=1 también puede
    /// ser un evento retrasado. Solo un snapshot nativo explícito autoriza
    /// reiniciar la época; en otros casos se pide reconciliación.
    pub fn observe_with_reset_marker(
        &mut self,
        seq: u64,
        session_snapshot: bool,
    ) -> SequenceOutcome {
        let Some(expected) = self.expected else {
            self.expected = Some(seq.saturating_add(1));
            return SequenceOutcome::Event;
        };
        if seq == expected {
            self.expected = Some(seq.saturating_add(1));
            SequenceOutcome::Event
        } else if seq == 1 && expected > 2 {
            if session_snapshot {
                self.expected = Some(2);
                self.reconciliations += 1;
                SequenceOutcome::Reset
            } else {
                SequenceOutcome::Gap
            }
        } else if seq < expected {
            SequenceOutcome::Duplicate
        } else {
            SequenceOutcome::Gap
        }
    }

    /// Tras `get_snapshot` se acepta la secuencia observada como nueva base.
    pub fn resync(&mut self, seq: u64) {
        self.expected = Some(seq.saturating_add(1));
        self.reconciliations += 1;
    }
}

pub struct DesktopClient {
    next_request: u64,
    pending: HashMap<String, mpsc::SyncSender<Result<Value, DesktopError>>>,
    pub capabilities: DesktopCapabilities,
    pub sequence: SequenceTracker,
    pub events: VecDeque<DesktopEvent>,
    deferred_dialogs: VecDeque<UiRequest>,
}

use std::collections::VecDeque;

impl Default for DesktopClient {
    fn default() -> Self {
        Self::new()
    }
}

impl DesktopClient {
    pub fn new() -> Self {
        Self {
            next_request: 1,
            pending: HashMap::new(),
            capabilities: DesktopCapabilities::default(),
            sequence: SequenceTracker::default(),
            events: VecDeque::new(),
            deferred_dialogs: VecDeque::new(),
        }
    }

    fn new_request_id(&mut self) -> String {
        let id = format!("fleet-desktop-{}", self.next_request);
        self.next_request += 1;
        id
    }

    fn encode_request(
        method: &str,
        request_id: &str,
        params: &Value,
    ) -> Result<String, DesktopError> {
        if method.len() > MAX_METHOD {
            return Err(DesktopError::new(
                "InvalidRequest",
                "method exceeds 64 characters",
            ));
        }
        if request_id.len() > MAX_REQUEST_ID {
            return Err(DesktopError::new(
                "InvalidRequest",
                "requestId exceeds 128 characters",
            ));
        }
        let envelope = json!({
            "schema": SCHEMA,
            "requestId": request_id,
            "method": method,
            "params": params,
        });
        let text = serde_json::to_string(&envelope)
            .map_err(|error| DesktopError::new("SerializationFailed", error.to_string()))?;
        if text.len() > MAX_REQUEST_BYTES {
            return Err(DesktopError::new(
                "InvalidRequest",
                "request exceeds 64 KiB",
            ));
        }
        Ok(text)
    }

    /// Negocia capacidades. Es la primera operación del adaptador.
    pub fn negotiate(&mut self, rpc: &mut JsonlRpc) -> Result<DesktopCapabilities, DesktopError> {
        let data = self.call(rpc, "capabilities", &json!({}), Duration::from_secs(20))?;
        self.capabilities = DesktopCapabilities::from_response(&data)?;
        Ok(self.capabilities.clone())
    }

    pub fn call(
        &mut self,
        rpc: &mut JsonlRpc,
        method: &str,
        params: &Value,
        timeout: Duration,
    ) -> Result<Value, DesktopError> {
        if !self.capabilities.methods.is_empty() && !self.capabilities.supports(method) {
            return Err(DesktopError::new(
                "UnsupportedMethod",
                format!("method `{method}` was not negotiated"),
            ));
        }
        let request_id = self.new_request_id();
        let payload = Self::encode_request(method, &request_id, params)?;
        let (tx, rx) = mpsc::sync_channel(1);
        self.pending.insert(request_id.clone(), tx);
        let message = format!("/gentle:desktop {payload}");
        let prompt = rpc
            .call("prompt", json!({"message": message}), timeout)
            .map_err(|error| DesktopError::new(error.code, error.message));
        if let Err(error) = prompt {
            self.pending.remove(&request_id);
            return Err(error);
        }
        // La respuesta de dominio llega como Extension UI mientras esperamos;
        // se bombea el transporte para no interbloquear el hilo lector.
        let deadline = std::time::Instant::now() + timeout;
        loop {
            // La cola ordinaria también puede llenarse durante una petición
            // Desktop RPC. Mantenerla drenada en el buffer acotado de JSONL
            // sin publicar eventos fuera de orden ni perder terminales.
            if let Err(error) = rpc.buffer_events() {
                self.pending.remove(&request_id);
                return Err(DesktopError::new(error.code, error.message));
            }
            while let Some(ui) = rpc.try_ui_request() {
                if ui.method == "setWidget"
                    && ui.value.get("widgetKey").and_then(Value::as_str)
                        == Some("gentle-desktop-rpc")
                {
                    self.handle_desktop_frame(&ui.value);
                } else if matches!(
                    ui.method.as_str(),
                    "select" | "confirm" | "input" | "editor"
                ) {
                    self.deferred_dialogs.push_back(ui);
                }
            }
            match rx.try_recv() {
                Ok(result) => return result,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending.remove(&request_id);
                    return Err(DesktopError::new(
                        "Transport",
                        "desktop response channel closed",
                    ));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            if std::time::Instant::now() >= deadline {
                self.pending.remove(&request_id);
                return Err(DesktopError::new(
                    "DesktopTimeout",
                    format!("no desktop response for `{method}`"),
                ));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Drena mensajes Extension UI. Devuelve los request de diálogo que Fleet
    /// debe traducir a aprobaciones; nunca responde a ellos automáticamente.
    pub fn drain_ui(&mut self, rpc: &mut JsonlRpc) -> Vec<UiRequest> {
        let mut dialogs: Vec<UiRequest> = self.deferred_dialogs.drain(..).collect();
        while let Some(request) = rpc.try_ui_request() {
            if request.method == "setWidget"
                && request.value.get("widgetKey").and_then(Value::as_str)
                    == Some("gentle-desktop-rpc")
            {
                self.handle_desktop_frame(&request.value);
                continue;
            }
            if matches!(
                request.method.as_str(),
                "select" | "confirm" | "input" | "editor"
            ) {
                dialogs.push(request);
            }
            // notify/setStatus/otros widgets son fire-and-forget: sin respuesta.
        }
        dialogs
    }

    fn handle_desktop_frame(&mut self, ui: &Value) {
        let Some(line) = ui
            .get("widgetLines")
            .and_then(Value::as_array)
            .and_then(|lines| lines.first())
            .and_then(Value::as_str)
        else {
            return;
        };
        let Ok(frame) = serde_json::from_str::<Value>(line) else {
            return;
        };
        if frame.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
            return;
        }
        let request_id = frame.get("requestId").and_then(Value::as_str).unwrap_or("");
        match frame.get("kind").and_then(Value::as_str) {
            Some("response") => {
                let result = if frame.get("ok").and_then(Value::as_bool) == Some(false) {
                    let error = frame.get("error").cloned().unwrap_or(json!({}));
                    Err(DesktopError::new(
                        error
                            .get("code")
                            .and_then(Value::as_str)
                            .unwrap_or("NativeError"),
                        error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("native desktop error"),
                    ))
                } else {
                    Ok(frame.get("data").cloned().unwrap_or(Value::Null))
                };
                if let Some(sender) = self.pending.remove(request_id) {
                    let _ = sender.send(result);
                }
            }
            Some("event") => {
                let Some(seq) = frame.get("seq").and_then(Value::as_u64) else {
                    return;
                };
                let event = frame
                    .get("event")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                match self
                    .sequence
                    .observe_with_reset_marker(seq, event == "session_snapshot")
                {
                    SequenceOutcome::Event | SequenceOutcome::Reset => {
                        self.events.push_back(DesktopEvent {
                            seq,
                            event,
                            data: frame.get("data").cloned().unwrap_or(Value::Null),
                        });
                    }
                    SequenceOutcome::Duplicate => {}
                    SequenceOutcome::Gap => {
                        // No se inventa el evento ausente; se registra el hueco
                        // y el driver debe pedir get_snapshot.
                        self.events.push_back(DesktopEvent {
                            seq,
                            event: "sequence_gap".into(),
                            data: json!({"expected_next": self.sequence.expected}),
                        });
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_limits_are_enforced() {
        assert!(DesktopClient::encode_request("m", "id", &json!({})).is_ok());
        assert!(DesktopClient::encode_request(&"m".repeat(65), "id", &json!({})).is_err());
        assert!(DesktopClient::encode_request("m", &"i".repeat(129), &json!({})).is_err());
        let big = json!({"x": "a".repeat(MAX_REQUEST_BYTES)});
        assert!(DesktopClient::encode_request("m", "id", &big).is_err());
    }

    #[test]
    fn capabilities_are_typed() {
        let data = json!({"methods":["capabilities","get_config"],"events":["config_changed"]});
        let caps = DesktopCapabilities::from_response(&data).unwrap();
        assert!(caps.supports("capabilities"));
        assert!(!caps.supports("list_subagents"));
    }

    #[test]
    fn sequence_tracker_detects_duplicates_gaps_and_resets() {
        let mut tracker = SequenceTracker::default();
        assert_eq!(tracker.observe_with_reset_marker(1, false), SequenceOutcome::Event);
        assert_eq!(tracker.observe_with_reset_marker(1, false), SequenceOutcome::Duplicate);
        assert_eq!(tracker.observe_with_reset_marker(3, false), SequenceOutcome::Gap);
        tracker.resync(3);
        assert_eq!(tracker.observe_with_reset_marker(4, false), SequenceOutcome::Event);
    }

    #[test]
    fn seq_one_requires_native_snapshot_before_reset() {
        let mut tracker = SequenceTracker::default();
        assert_eq!(tracker.observe_with_reset_marker(8, false), SequenceOutcome::Event);
        assert_eq!(tracker.observe_with_reset_marker(1, false), SequenceOutcome::Gap);
        assert_eq!(
            tracker.observe_with_reset_marker(1, true),
            SequenceOutcome::Reset
        );
        assert_eq!(tracker.observe_with_reset_marker(2, false), SequenceOutcome::Event);
        assert_eq!(tracker.observe_with_reset_marker(1, false), SequenceOutcome::Duplicate);
    }

    #[test]
    fn desktop_response_correlates_by_request_id_not_prompt_id() {
        let mut client = DesktopClient::new();
        let (tx, rx) = mpsc::sync_channel(1);
        client.pending.insert("p4-cap".into(), tx);
        let ui = json!({
            "type":"extension_ui_request",
            "method":"setWidget",
            "widgetKey":"gentle-desktop-rpc",
            "widgetLines":[ serde_json::to_string(&json!({
                "schema":SCHEMA,"kind":"response","requestId":"p4-cap","ok":true,
                "data":{"methods":["capabilities"],"events":[]}
            })).unwrap() ]
        });
        client.handle_desktop_frame(&ui);
        let data = rx
            .recv_timeout(Duration::from_millis(100))
            .unwrap()
            .unwrap();
        assert_eq!(data["methods"][0], "capabilities");
    }

    #[test]
    fn desktop_events_never_look_like_chat_text() {
        let mut client = DesktopClient::new();
        let ui = json!({
            "type":"extension_ui_request",
            "method":"setWidget",
            "widgetKey":"gentle-desktop-rpc",
            "widgetLines":[ serde_json::to_string(&json!({
                "schema":SCHEMA,"kind":"event","seq":1,"event":"session_snapshot","data":{}
            })).unwrap() ]
        });
        client.handle_desktop_frame(&ui);
        assert_eq!(client.events[0].event, "session_snapshot");
        assert!(client.events[0].data.is_object());
    }
}

#[cfg(test)]
mod real_probe {
    use super::*;
    use std::path::Path;

    #[test]
    #[ignore = "requires installed gentle-shell; opt-in diagnostic"]
    fn real_capabilities_roundtrip() {
        let args = vec!["--mode".into(), "rpc".into(), "--no-session".into()];
        let env = [("GENTLE_SHELL_INTERACTIVE_HOST", "1")];
        let mut rpc =
            JsonlRpc::spawn_with_widgets(Path::new("/tmp"), "gentle-shell", &args, &env, true)
                .unwrap();
        let mut client = DesktopClient::new();
        let caps = client.negotiate(&mut rpc).unwrap();
        assert!(!caps.methods.is_empty());
        eprintln!("methods={} events={:?}", caps.methods.len(), caps.events);
        let _ = rpc.close();
    }
}

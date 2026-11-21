//! Transporte Pi RPC: JSONL estricto sobre stdin/stdout de un proceso hijo.
//!
//! Diferencias respecto al App Server de Codex que impiden reutilizar su
//! cliente sin análisis: Pi usa `{"id","type":"<command>"}` en lugar de
//! JSON-RPC 2.0, separa eventos de respuestas por `type:"response"` y dispone
//! de un subprotocolo `extension_ui_request` iniciado por el proceso.

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

pub const MAX_LINE_BYTES: usize = 1024 * 1024;
const EVENT_QUEUE: usize = 256;
const UI_QUEUE: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcError {
    pub code: String,
    pub message: String,
}

impl RpcError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiRequest {
    pub id: String,
    pub method: String,
    pub value: Value,
}

type Pending = Arc<Mutex<HashMap<String, mpsc::SyncSender<Result<Value, RpcError>>>>>;

/// Lee una línea terminada **solo** en LF. Nunca trata U+2028/U+2029 como
/// separadores (son válidos dentro de strings JSON). Si el frame excede el
/// límite se descarta hasta LF y se informa, sin truncarlo ni entregarlo.
fn read_lf_line<R: BufRead>(reader: &mut R) -> io::Result<(Option<Vec<u8>>, bool)> {
    let mut bytes = Vec::with_capacity(1024);
    let mut one = [0u8; 1];
    let mut oversized = false;
    loop {
        let read = reader.read(&mut one)?;
        if read == 0 {
            return Ok((if bytes.is_empty() { None } else { Some(bytes) }, oversized));
        }
        if one[0] == b'\n' {
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
            return Ok((Some(bytes), oversized));
        }
        if bytes.len() < MAX_LINE_BYTES {
            bytes.push(one[0]);
        } else {
            oversized = true;
        }
    }
}

fn classify(
    value: Value,
    pending: &Pending,
    events: &SyncSender<Value>,
    ui: &SyncSender<UiRequest>,
) {
    let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
    if kind == "response" {
        let Some(id) = value.get("id").and_then(Value::as_str) else {
            return;
        };
        let result = if value.get("success").and_then(Value::as_bool) == Some(false) {
            Err(RpcError::new(
                value
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("command_failed"),
                value
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("Pi command failed"),
            ))
        } else {
            Ok(value.get("data").cloned().unwrap_or(Value::Null))
        };
        if let Some(sender) = pending.lock().expect("rpc pending poisoned").remove(id) {
            let _ = sender.send(result);
        }
        return;
    }
    if kind == "extension_ui_request" {
        let request = UiRequest {
            id: value.get("id").and_then(Value::as_str).unwrap_or("").into(),
            method: value
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into(),
            value,
        };
        // Las aprobaciones nunca se descartan: backpressure bloquea al lector.
        let _ = ui.send(request);
        return;
    }
    if kind.is_empty() {
        return;
    }
    // Sin pérdida silenciosa: si la cola está llena el hilo lector se bloquea
    // (backpressure) hasta que el driver drena los eventos pendientes.
    let _ = events.send(value);
}

pub struct PiRpc {
    child: Child,
    stdin: Option<ChildStdin>,
    next_id: AtomicU64,
    pending: Pending,
    events: Receiver<Value>,
    ui: Receiver<UiRequest>,
    alive: Arc<AtomicBool>,
    dropped: Arc<AtomicU64>,
    stderr_tail: Arc<Mutex<Vec<String>>>,
}

impl PiRpc {
    /// Arranca Pi en modo RPC con un directorio de sesiones propio y aislado.
    /// No hereda ni modifica la configuración personal del usuario.
    pub fn spawn(workspace: &Path, session_dir: &Path) -> Result<Self, RpcError> {
        let mut child = Command::new("pi")
            .arg("--mode")
            .arg("rpc")
            .arg("--session-dir")
            .arg(session_dir)
            .current_dir(workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| RpcError::new("provider_unavailable", error.to_string()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| RpcError::new("provider_unavailable", "no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| RpcError::new("provider_unavailable", "no stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| RpcError::new("provider_unavailable", "no stderr"))?;

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (event_tx, events) = mpsc::sync_channel(EVENT_QUEUE);
        let (ui_tx, ui) = mpsc::sync_channel(UI_QUEUE);
        let alive = Arc::new(AtomicBool::new(true));
        let dropped = Arc::new(AtomicU64::new(0));
        let stderr_tail = Arc::new(Mutex::new(Vec::new()));

        let reader_alive = alive.clone();
        let reader_pending = pending.clone();
        let reader_dropped = dropped.clone();
        std::thread::spawn(move || {
            let mut reader = io::BufReader::new(stdout);
            loop {
                if !reader_alive.load(Ordering::Acquire) {
                    break;
                }
                match read_lf_line(&mut reader) {
                    Ok((Some(bytes), oversized)) => {
                        if oversized {
                            reader_dropped.fetch_add(1, Ordering::Relaxed);
                        }
                        if oversized {
                            let error = json!({
                                "type":"agent_error",
                                "message":"Pi frame exceeded the configured limit"
                            });
                            let _ = event_tx.send(error);
                            continue;
                        }
                        if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                            classify(value, &reader_pending, &event_tx, &ui_tx);
                        } else {
                            let error = json!({
                                "type":"agent_error",
                                "message":"malformed Pi RPC frame"
                            });
                            let _ = event_tx.send(error);
                        }
                    }
                    Ok((None, _)) | Err(_) => break,
                }
            }
            reader_alive.store(false, Ordering::Release);
        });

        let stderr_alive = alive.clone();
        let stderr_log = stderr_tail.clone();
        std::thread::spawn(move || {
            let mut reader = io::BufReader::new(stderr);
            while stderr_alive.load(Ordering::Acquire) {
                match read_lf_line(&mut reader) {
                    Ok((Some(bytes), _)) => {
                        let line = String::from_utf8_lossy(&bytes).into_owned();
                        let mut log = stderr_log.lock().expect("stderr log poisoned");
                        if log.len() == 16 {
                            log.remove(0);
                        }
                        log.push(line);
                    }
                    _ => break,
                }
            }
        });

        Ok(Self {
            child,
            stdin: Some(stdin),
            next_id: AtomicU64::new(1),
            pending,
            events,
            ui,
            alive,
            dropped,
            stderr_tail,
        })
    }

    fn write_record(&mut self, record: &Value) -> Result<(), RpcError> {
        let line = serde_json::to_vec(record)
            .map_err(|error| RpcError::new("serialization_failed", error.to_string()))?;
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| RpcError::new("rpc_transport", "Pi stdin is closed"))?;
        stdin
            .write_all(&line)
            .and_then(|_| stdin.write_all(b"\n"))
            .and_then(|_| stdin.flush())
            .map_err(|error| RpcError::new("rpc_transport", error.to_string()))
    }

    /// Envía un comando y espera su respuesta con plazo acotado. Los eventos
    /// del proceso siguen siendo entregados por el hilo lector mientras tanto.
    pub fn call(
        &mut self,
        command: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, RpcError> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(RpcError::new("provider_gone", "Pi closed its stdout"));
        }
        let id = format!("fleet-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = mpsc::sync_channel(1);
        self.pending
            .lock()
            .expect("rpc pending poisoned")
            .insert(id.clone(), tx);
        let mut record = params;
        record["id"] = json!(id);
        record["type"] = json!(command);
        if let Err(error) = self.write_record(&record) {
            self.pending
                .lock()
                .expect("rpc pending poisoned")
                .remove(&id);
            return Err(error);
        }
        match rx.recv_timeout(timeout) {
            Ok(result) => result,
            Err(_) => {
                self.pending
                    .lock()
                    .expect("rpc pending poisoned")
                    .remove(&id);
                Err(RpcError::new(
                    "rpc_timeout",
                    format!(
                        "no response for {command} within {} ms",
                        timeout.as_millis()
                    ),
                ))
            }
        }
    }

    pub fn respond_ui(&mut self, id: &str, answer: Value) -> Result<(), RpcError> {
        let mut record = answer;
        record["id"] = json!(id);
        record["type"] = json!("extension_ui_response");
        self.write_record(&record)
    }

    pub fn try_event(&self) -> Option<Value> {
        self.events.try_recv().ok()
    }

    pub fn try_ui_request(&self) -> Option<UiRequest> {
        self.ui.try_recv().ok()
    }

    pub fn dropped_frames(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    pub fn stderr_tail(&self) -> Vec<String> {
        self.stderr_tail
            .lock()
            .expect("stderr log poisoned")
            .clone()
    }

    /// Cierre ordenado: EOF en stdin, espera acotada y SIGKILL solo si hace falta.
    pub fn close(&mut self) -> Result<(), RpcError> {
        self.alive.store(false, Ordering::Release);
        drop(self.stdin.take());
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => return Ok(()),
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(error) => return Err(RpcError::new("StopUnconfirmed", error.to_string())),
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        Err(RpcError::new(
            "StopUnconfirmed",
            "Pi required SIGKILL after close",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lf_reader_accepts_unicode_separators_inside_payload() {
        let payload = "a\u{2028}b\u{2029}c";
        let framed = format!("{payload}\n");
        let mut input = io::BufReader::new(framed.as_bytes());
        let (line, oversized) = read_lf_line(&mut input).unwrap();
        assert!(!oversized);
        assert_eq!(std::str::from_utf8(&line.unwrap()).unwrap(), payload);
    }

    #[test]
    fn lf_reader_rejects_crlf_and_split_lines() {
        let mut input = io::BufReader::new("abc\r\n".as_bytes());
        let (line, _) = read_lf_line(&mut input).unwrap();
        assert_eq!(line.unwrap(), b"abc".to_vec());
    }

    #[test]
    fn oversized_frame_is_reported_without_truncation() {
        let mut big = "x".repeat(MAX_LINE_BYTES + 100);
        big.push('\n');
        let mut input = io::BufReader::new(big.as_bytes());
        let (line, oversized) = read_lf_line(&mut input).unwrap();
        assert!(oversized);
        assert_eq!(line.unwrap().len(), MAX_LINE_BYTES);
    }

    #[test]
    fn response_correlates_by_id_and_events_are_separate() {
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (tx, rx) = mpsc::sync_channel(1);
        pending.lock().unwrap().insert("fleet-1".into(), tx);
        let (event_tx, event_rx) = mpsc::sync_channel(8);
        let (ui_tx, _ui_rx) = mpsc::sync_channel(8);
        classify(
            json!({"type":"response","id":"fleet-1","command":"get_state","success":true,"data":{"ok":1}}),
            &pending,
            &event_tx,
            &ui_tx,
        );
        assert_eq!(
            rx.recv_timeout(Duration::from_millis(100))
                .unwrap()
                .unwrap()["ok"],
            1
        );
        classify(
            json!({"type":"agent_settled","aborted":false}),
            &pending,
            &event_tx,
            &ui_tx,
        );
        assert_eq!(event_rx.try_recv().unwrap()["type"], "agent_settled");
        classify(
            json!({"type":"extension_ui_request","id":"u1","method":"select"}),
            &pending,
            &event_tx,
            &ui_tx,
        );
        assert!(event_rx.try_recv().is_err());
    }
}

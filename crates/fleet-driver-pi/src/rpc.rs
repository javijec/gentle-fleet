//! Transporte Pi RPC: JSONL estricto sobre stdin/stdout de un proceso hijo.
//!
//! Diferencias respecto al App Server de Codex que impiden reutilizar su
//! cliente sin análisis: Pi usa `{"id","type":"<command>"}` en lugar de
//! JSON-RPC 2.0, separa eventos de respuestas por `type:"response"` y dispone
//! de un subprotocolo `extension_ui_request` iniciado por el proceso.

use std::collections::{HashMap, VecDeque};
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

pub const MAX_LINE_BYTES: usize = 1024 * 1024;
const EVENT_QUEUE: usize = 256;
const UI_QUEUE: usize = 64;
// Los mensajes recibidos mientras se espera un ACK no se descartan: se
// retienen en una cola ordenada para entregarlos al runner después de call().
const MAX_DEFERRED_EVENTS: usize = 512;
const MAX_DEFERRED_BYTES: usize = 8 * 1024 * 1024;

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
        // Estos métodos son notificaciones de Extension UI. No esperan
        // respuesta y no deben ocupar la cola de aprobaciones tipadas.
        if matches!(request.method.as_str(), "notify" | "setStatus" | "setWidget") {
            return;
        }
        // Las solicitudes interactivas nunca se descartan silenciosamente.
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
    deferred_events: VecDeque<Value>,
    deferred_ui: VecDeque<UiRequest>,
    deferred_bytes: usize,
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
                            let _ = event_tx.send(json!({
                                "type":"rpc_fatal_error",
                                "message":"Pi frame exceeded the configured limit"
                            }));
                            // Un frame perdido podría ser agent_settled o una
                            // respuesta. El resultado es incierto: fail closed.
                            break;
                        }
                        match serde_json::from_slice::<Value>(&bytes) {
                            Ok(value) => classify(value, &reader_pending, &event_tx, &ui_tx),
                            Err(_) => {
                                let _ = event_tx.send(json!({
                                    "type":"rpc_fatal_error",
                                    "message":"malformed Pi RPC frame"
                                }));
                                break;
                            }
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
            deferred_events: VecDeque::new(),
            deferred_ui: VecDeque::new(),
            deferred_bytes: 0,
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

    fn retain_event(&mut self, event: Value) -> Result<(), RpcError> {
        let bytes = serde_json::to_vec(&event)
            .map_err(|error| RpcError::new("serialization_failed", error.to_string()))?
            .len();
        self.deferred_bytes = self.deferred_bytes.saturating_add(bytes);
        self.deferred_events.push_back(event);
        if self.deferred_events.len() > MAX_DEFERRED_EVENTS
            || self.deferred_bytes > MAX_DEFERRED_BYTES
        {
            // Conservar también el evento que alcanzó el límite. El caller
            // recibe error explícito y nunca un falso ACK de Pi.
            return Err(RpcError::new(
                "rpc_backpressure",
                "Pi emitted more events than the bounded synchronous-call buffer; outcome uncertain",
            ));
        }
        Ok(())
    }

    fn retain_ui(&mut self, request: UiRequest) -> Result<(), RpcError> {
        self.deferred_ui.push_back(request);
        if self.deferred_ui.len() > UI_QUEUE {
            return Err(RpcError::new(
                "rpc_backpressure",
                "Pi emitted too many interactive requests during a synchronous call; outcome uncertain",
            ));
        }
        Ok(())
    }

    /// Mientras espera un ACK, drena las colas del lector RPC y retiene los
    /// eventos para next_event(). Evita un interbloqueo entre backpressure y
    /// una respuesta cuyo frame aún no pudo leer el mismo hilo.
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

        let deadline = std::time::Instant::now() + timeout;
        let outcome = loop {
            // Un fallo de protocolo invalida la correlación incluso si Pi
            // alcanzó a entregar una respuesta previa a la corrupción.
            let mut fatal = None;
            while let Ok(event) = self.events.try_recv() {
                if event.get("type").and_then(Value::as_str) == Some("rpc_fatal_error") {
                    fatal = Some(RpcError::new(
                        "rpc_protocol",
                        "Pi RPC reader encountered a malformed or oversized frame",
                    ));
                }
                if let Err(error) = self.retain_event(event) {
                    fatal = Some(error);
                    break;
                }
            }
            if fatal.is_none() {
                while let Ok(request) = self.ui.try_recv() {
                    if let Err(error) = self.retain_ui(request) {
                        fatal = Some(error);
                        break;
                    }
                }
            }
            if let Some(error) = fatal {
                break Err(error);
            }
            match rx.try_recv() {
                Ok(reply) => break reply,
                Err(TryRecvError::Disconnected) => {
                    break Err(RpcError::new(
                        "provider_gone",
                        "Pi RPC response channel disconnected",
                    ));
                }
                Err(TryRecvError::Empty) => {}
            }
            if !self.alive.load(Ordering::Acquire) {
                break Err(RpcError::new(
                    "provider_gone",
                    "Pi reader closed before the command response",
                ));
            }
            if std::time::Instant::now() >= deadline {
                break Err(RpcError::new(
                    "rpc_timeout",
                    format!(
                        "no response for {command} within {} ms",
                        timeout.as_millis()
                    ),
                ));
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        self.pending
            .lock()
            .expect("rpc pending poisoned")
            .remove(&id);
        outcome
    }

    pub fn respond_ui(&mut self, id: &str, answer: Value) -> Result<(), RpcError> {
        let mut record = answer;
        record["id"] = json!(id);
        record["type"] = json!("extension_ui_response");
        self.write_record(&record)
    }

    pub fn try_event(&mut self) -> Option<Value> {
        if let Some(event) = self.deferred_events.pop_front() {
            self.deferred_bytes = self.deferred_bytes.saturating_sub(
                serde_json::to_vec(&event)
                    .map(|bytes| bytes.len())
                    .unwrap_or_default(),
            );
            return Some(event);
        }
        self.events.try_recv().ok()
    }

    pub fn try_ui_request(&mut self) -> Option<UiRequest> {
        self.deferred_ui
            .pop_front()
            .or_else(|| self.ui.try_recv().ok())
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
    fn synchronous_wait_can_drain_full_event_queue_without_discarding_terminal() {
        let (tx, rx) = mpsc::sync_channel(1);
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (ui_tx, _ui_rx) = mpsc::sync_channel(1);
        let writer = std::thread::spawn(move || {
            for index in 0..4 {
                classify(
                    json!({"type":"message_update","index":index}),
                    &pending,
                    &tx,
                    &ui_tx,
                );
            }
            classify(
                json!({"type":"agent_settled","aborted":false}),
                &pending,
                &tx,
                &ui_tx,
            );
        });
        let mut received = Vec::new();
        for _ in 0..5 {
            received.push(rx.recv_timeout(Duration::from_secs(1)).unwrap());
        }
        writer.join().unwrap();
        assert_eq!(received[4]["type"], "agent_settled");
        assert_eq!(received.len(), 5);
    }

    #[cfg(unix)]
    #[test]
    fn pi_call_drains_saturated_pipe_before_receiving_ack() {
        // Test de extremo a extremo del propio transporte, sin un modelo Pi.
        // El proceso envía más eventos que la capacidad de la cola acotada
        // antes de emitir el ACK de "prompt".
        let script = r#"IFS= read -r line
i=0
while [ "$i" -lt 300 ]; do
  printf '{"type":"message_update","index":%s}\n' "$i"
  i=$((i+1))
done
printf '{"type":"response","id":"fleet-1","success":true,"data":{"ok":true}}\n'
"#;
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let pending_reader = pending.clone();
        let (event_tx, events) = mpsc::sync_channel(1);
        let (ui_tx, ui) = mpsc::sync_channel(1);
        let alive = Arc::new(AtomicBool::new(true));
        let reader_alive = alive.clone();
        let reader = std::thread::spawn(move || {
            let mut stdout = io::BufReader::new(stdout);
            while let Ok((Some(bytes), false)) = read_lf_line(&mut stdout) {
                let value: Value = serde_json::from_slice(&bytes).unwrap();
                classify(value, &pending_reader, &event_tx, &ui_tx);
            }
            reader_alive.store(false, Ordering::Release);
        });
        let mut rpc = PiRpc {
            child,
            stdin: Some(stdin),
            next_id: AtomicU64::new(1),
            pending,
            events,
            ui,
            alive,
            dropped: Arc::new(AtomicU64::new(0)),
            stderr_tail: Arc::new(Mutex::new(Vec::new())),
            deferred_events: VecDeque::new(),
            deferred_ui: VecDeque::new(),
            deferred_bytes: 0,
        };
        let reply = rpc
            .call("prompt", json!({"message":"test"}), Duration::from_secs(5))
            .unwrap();
        assert_eq!(reply["ok"], true);
        for index in 0..300 {
            let event = rpc.try_event().expect("missing or discarded event");
            assert_eq!(event["index"], index);
        }
        assert!(rpc.try_event().is_none());
        let _ = rpc.close();
        reader.join().unwrap();
    }

    #[test]
    fn extension_ui_notifications_never_block_approval_queue() {
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (events_tx, _events) = mpsc::sync_channel(1);
        let (ui_tx, ui_rx) = mpsc::sync_channel(1);
        for method in ["notify", "setStatus", "setWidget"] {
            classify(
                json!({"type":"extension_ui_request","id":"u1","method":method}),
                &pending,
                &events_tx,
                &ui_tx,
            );
        }
        assert!(ui_rx.try_recv().is_err());
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

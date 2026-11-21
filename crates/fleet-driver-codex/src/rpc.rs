//! Cliente JSON-RPC 2.0 sobre stdio para Codex App Server.
//!
//! El bucle de lectura corre en su propio hilo y **nunca bloquea esperando la
//! respuesta de una petición propia**: mientras tanto sigue entregando
//! notificaciones y requests iniciados por el servidor (aprobaciones). Es el
//! requisito que evita el deadlock descrito en `P4_RPC_RESEARCH.md` §1.2.

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
const NOTIFICATION_QUEUE: usize = 256;
const SERVER_REQUEST_QUEUE: usize = 64;
// Buffer para mensajes que llegan mientras una llamada síncrona espera su ACK.
// Nunca descartar eventos: al llegar al límite, fallar explícitamente.
const MAX_DEFERRED_FRAMES: usize = 512;
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

impl std::error::Error for RpcError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerRequest {
    /// ID JSON-RPC original. Se conserva como `Value` para devolverlo
    /// exactamente igual (Codex usa IDs numéricos).
    pub id: Value,
    pub method: String,
    pub params: Value,
}

type Pending = Arc<Mutex<HashMap<u64, mpsc::SyncSender<Result<Value, RpcError>>>>>;

pub struct RpcClient {
    child: Child,
    stdin: Option<ChildStdin>,
    next_id: AtomicU64,
    pending: Pending,
    notifications: Receiver<Value>,
    server_requests: Receiver<ServerRequest>,
    reader_alive: Arc<AtomicBool>,
    dropped_notifications: Arc<AtomicU64>,
    last_stderr: Arc<Mutex<Vec<String>>>,
    deferred_notifications: VecDeque<Value>,
    deferred_requests: VecDeque<ServerRequest>,
    deferred_bytes: usize,
}

fn read_line_bounded<R: BufRead>(reader: &mut R) -> io::Result<Option<String>> {
    let mut bytes = Vec::with_capacity(1024);
    let mut one = [0u8; 1];
    loop {
        let read = reader.read(&mut one)?;
        if read == 0 {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
            };
        }
        if one[0] == b'\n' {
            return Ok(Some(String::from_utf8_lossy(&bytes).into_owned()));
        }
        if bytes.len() >= MAX_LINE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Codex JSON-RPC frame exceeded maximum length",
            ));
        }
        bytes.push(one[0]);
    }
}

fn classify(
    value: Value,
    pending: &Pending,
    notifications: &SyncSender<Value>,
    server_requests: &SyncSender<ServerRequest>,
    dropped: &AtomicU64,
) {
    let has_method = value.get("method").is_some();
    let has_result = value.get("result").is_some() || value.get("error").is_some();
    if has_method {
        let method = value
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if value.get("id").is_some() && !has_result {
            let request = ServerRequest {
                id: value.get("id").cloned().unwrap_or(Value::Null),
                method,
                params: value.get("params").cloned().unwrap_or(json!({})),
            };
            // Las aprobaciones NUNCA se descartan: el hilo se bloquea aquí
            // hasta que el driver drena la cola acotada.
            let _ = server_requests.send(request);
        } else {
            // Backpressure real: no eliminar mensajes al llenar la cola.
            // call() drena esta cola mientras espera respuestas propias,
            // evitando el deadlock con turn/start y requests del servidor.
            if notifications.send(value).is_err() {
                dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
        return;
    }
    if has_result {
        if let Some(id) = value.get("id").and_then(Value::as_u64) {
            if let Some(sender) = pending.lock().expect("rpc pending poisoned").remove(&id) {
                let result = match value.get("error") {
                    Some(error) => Err(RpcError::new(
                        error
                            .get("code")
                            .and_then(Value::as_i64)
                            .map(|code| code.to_string())
                            .unwrap_or_else(|| "jsonrpc_error".into()),
                        error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("JSON-RPC error")
                            .to_string(),
                    )),
                    None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = sender.send(result);
            }
        }
    }
}

impl RpcClient {
    /// Arranca `codex app-server` heredando el entorno (incluidas las
    /// credenciales nativas). No copia ni escribe `auth.json` ni `CODEX_HOME`.
    pub fn spawn(workspace: &Path) -> Result<Self, RpcError> {
        let mut child = Command::new("codex")
            .arg("app-server")
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
        let (notification_tx, notifications) = mpsc::sync_channel(NOTIFICATION_QUEUE);
        let (server_tx, server_requests) = mpsc::sync_channel(SERVER_REQUEST_QUEUE);
        let reader_alive = Arc::new(AtomicBool::new(true));
        let dropped_notifications = Arc::new(AtomicU64::new(0));
        let last_stderr = Arc::new(Mutex::new(Vec::new()));

        let reader_pending = pending.clone();
        let reader_alive_thread = reader_alive.clone();
        let reader_dropped = dropped_notifications.clone();
        std::thread::spawn(move || {
            let mut reader = io::BufReader::new(stdout);
            loop {
                if !reader_alive_thread.load(Ordering::Acquire) {
                    break;
                }
                match read_line_bounded(&mut reader) {
                    Ok(Some(line)) => {
                        if let Ok(value) = serde_json::from_str::<Value>(&line) {
                            classify(
                                value,
                                &reader_pending,
                                &notification_tx,
                                &server_tx,
                                &reader_dropped,
                            );
                        }
                    }
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
            reader_alive_thread.store(false, Ordering::Release);
        });

        let stderr_alive = reader_alive.clone();
        let stderr_log = last_stderr.clone();
        std::thread::spawn(move || {
            let mut reader = io::BufReader::new(stderr);
            while stderr_alive.load(Ordering::Acquire) {
                match read_line_bounded(&mut reader) {
                    Ok(Some(line)) => {
                        // Los logs de stderr pueden contener IDs nativos; se
                        // conservan solo en memoria y nunca se publican tal cual.
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
            notifications,
            server_requests,
            reader_alive,
            dropped_notifications,
            last_stderr,
            deferred_notifications: VecDeque::new(),
            deferred_requests: VecDeque::new(),
            deferred_bytes: 0,
        })
    }

    fn write_frame(&mut self, frame: &Value) -> Result<(), RpcError> {
        let line = serde_json::to_string(frame)
            .map_err(|error| RpcError::new("serialization_failed", error.to_string()))?;
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| RpcError::new("rpc_transport", "App Server stdin is closed"))?;
        stdin
            .write_all(line.as_bytes())
            .and_then(|_| stdin.write_all(b"\n"))
            .and_then(|_| stdin.flush())
            .map_err(|error| RpcError::new("rpc_transport", error.to_string()))
    }

    fn defer_notification(&mut self, frame: Value) -> Result<(), RpcError> {
        let bytes = serde_json::to_vec(&frame)
            .map_err(|error| RpcError::new("serialization_failed", error.to_string()))?
            .len();
        if self.deferred_notifications.len() >= MAX_DEFERRED_FRAMES
            || self.deferred_bytes.saturating_add(bytes) > MAX_DEFERRED_BYTES
        {
            return Err(RpcError::new(
                "rpc_backpressure",
                "Codex produced too many events during a synchronous call; result is uncertain",
            ));
        }
        self.deferred_bytes += bytes;
        self.deferred_notifications.push_back(frame);
        Ok(())
    }

    fn defer_request(&mut self, request: ServerRequest) -> Result<(), RpcError> {
        if self.deferred_requests.len() >= SERVER_REQUEST_QUEUE {
            return Err(RpcError::new(
                "rpc_backpressure",
                "Codex sent too many server requests during a synchronous call; result is uncertain",
            ));
        }
        self.deferred_requests.push_back(request);
        Ok(())
    }

    pub fn call(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, RpcError> {
        if !self.reader_alive.load(Ordering::Acquire) {
            return Err(RpcError::new(
                "provider_gone",
                "App Server closed its stdout",
            ));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::sync_channel(1);
        self.pending
            .lock()
            .expect("rpc pending poisoned")
            .insert(id, tx);
        let frame = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        if let Err(error) = self.write_frame(&frame) {
            self.pending
                .lock()
                .expect("rpc pending poisoned")
                .remove(&id);
            return Err(error);
        }
        let deadline = std::time::Instant::now() + timeout;
        let outcome = loop {
            match rx.try_recv() {
                Ok(response) => break response,
                Err(TryRecvError::Disconnected) => {
                    break Err(RpcError::new(
                        "provider_gone",
                        "RPC reply channel disconnected",
                    ));
                }
                Err(TryRecvError::Empty) => {}
            }
            // Mientras esperamos un ACK, el productor puede enviar deltas y
            // requests de aprobación. Liberar ambas colas mantiene al lector
            // capaz de recibir la respuesta sin descartar información.
            let mut deferred_result = Ok(());
            while let Ok(notification) = self.notifications.try_recv() {
                if let Err(error) = self.defer_notification(notification) {
                    deferred_result = Err(error);
                    break;
                }
            }
            if deferred_result.is_ok() {
                while let Ok(request) = self.server_requests.try_recv() {
                    if let Err(error) = self.defer_request(request) {
                        deferred_result = Err(error);
                        break;
                    }
                }
            }
            if let Err(error) = deferred_result {
                // No declarar éxito si el buffering no puede garantizar la
                // entrega. El runner debe reconciliar el estado del turno.
                break Err(error);
            }
            if std::time::Instant::now() >= deadline {
                break Err(RpcError::new(
                    "rpc_timeout",
                    format!("no response for {method} within {} ms", timeout.as_millis()),
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

    pub fn notify(&mut self, method: &str, params: Value) -> Result<(), RpcError> {
        self.write_frame(&json!({"jsonrpc":"2.0","method":method,"params":params}))
    }

    /// Responde a un request iniciado por el servidor (aprobación) con el ID
    /// original. Nunca se llama automáticamente sin una decisión del usuario.
    pub fn respond(&mut self, id: &Value, result: Value) -> Result<(), RpcError> {
        self.write_frame(&json!({"jsonrpc":"2.0","id":id,"result":result}))
    }

    pub fn respond_error(&mut self, id: &Value, code: i64, message: &str) -> Result<(), RpcError> {
        self.write_frame(&json!({
            "jsonrpc":"2.0",
            "id": id,
            "error": {"code": code, "message": message}
        }))
    }

    pub fn try_notification(&mut self) -> Option<Value> {
        if let Some(frame) = self.deferred_notifications.pop_front() {
            self.deferred_bytes = self.deferred_bytes.saturating_sub(
                serde_json::to_vec(&frame)
                    .map(|bytes| bytes.len())
                    .unwrap_or(0),
            );
            return Some(frame);
        }
        self.notifications.try_recv().ok()
    }

    pub fn recv_server_request(&mut self, timeout: Duration) -> Option<ServerRequest> {
        self.deferred_requests
            .pop_front()
            .or_else(|| self.server_requests.recv_timeout(timeout).ok())
    }

    pub fn dropped_notifications(&self) -> u64 {
        self.dropped_notifications.load(Ordering::Relaxed)
    }

    pub fn alive(&self) -> bool {
        self.reader_alive.load(Ordering::Acquire)
    }

    pub fn stderr_tail(&self) -> Vec<String> {
        self.last_stderr
            .lock()
            .expect("stderr log poisoned")
            .clone()
    }

    pub fn close(&mut self) -> Result<(), RpcError> {
        self.reader_alive.store(false, Ordering::Release);
        // Cerrar stdin es la señal de apagado ordenado del App Server.
        drop(self.stdin.take());
        match self.child.wait_timeout(Duration::from_secs(2)) {
            Ok(Some(_)) => Ok(()),
            Ok(None) => {
                let _ = self.child.kill();
                let _ = self.child.wait();
                Err(RpcError::new(
                    "StopUnconfirmed",
                    "App Server required SIGKILL after close",
                ))
            }
            Err(error) => Err(RpcError::new("StopUnconfirmed", error.to_string())),
        }
    }
}

/// `wait_timeout` no existe en `std::process::Child`; se implementa sondeando
/// `try_wait` con sleeps acotados para mantener el cierre determinista.
trait WaitTimeout {
    fn wait_timeout(&mut self, timeout: Duration) -> io::Result<Option<std::process::ExitStatus>>;
}

impl WaitTimeout for Child {
    fn wait_timeout(&mut self, timeout: Duration) -> io::Result<Option<std::process::ExitStatus>> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(Some(status));
            }
            if std::time::Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_rpc_frame_is_rejected_not_truncated() {
        let mut payload = vec![b'x'; MAX_LINE_BYTES + 1];
        payload.push(b'\n');
        let mut reader = io::BufReader::new(io::Cursor::new(payload));
        assert_eq!(
            read_line_bounded(&mut reader).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn full_notification_queue_applies_backpressure_without_loss() {
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (notifications_tx, notifications) = mpsc::sync_channel(1);
        let (requests_tx, _requests) = mpsc::sync_channel(1);
        let dropped = Arc::new(AtomicU64::new(0));
        classify(
            json!({"method":"item/agentMessage/delta","params":{"delta":"one"}}),
            &pending,
            &notifications_tx,
            &requests_tx,
            &dropped,
        );
        let thread_dropped = dropped.clone();
        let reader = std::thread::spawn(move || {
            classify(
                json!({"method":"turn/completed","params":{"turn":{"status":"completed"}}}),
                &pending,
                &notifications_tx,
                &requests_tx,
                &thread_dropped,
            );
        });
        assert_eq!(
            notifications.recv().unwrap()["method"],
            "item/agentMessage/delta"
        );
        reader.join().unwrap();
        assert_eq!(notifications.recv().unwrap()["method"], "turn/completed");
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
    }
}

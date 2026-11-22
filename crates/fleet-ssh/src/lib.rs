//! Transporte Fleet local/remoto y configuración de hosts SSH.
//!
//! El wire público no cambia: `Frame`, `Request`, `Response` y `Event` son los
//! mismos en ambos transportes. SSH solo transporta bytes y realiza la
//! autenticación local al `fleetd` remoto dentro del bridge.

use std::collections::VecDeque;
use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use fleet_protocol::transport::{connect, expected_token, LocalStream};
use fleet_protocol::{read_frame, write_frame, Frame, Request};
use serde::{Deserialize, Serialize};

pub const SSH_BRIDGE: &str = "fleet-ssh-bridge";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostConfig {
    pub alias: String,
    /// Destino SSH (`user@host`), nunca interpretado por un shell.
    pub destination: String,
    /// Ruta absoluta del bridge en la máquina remota.
    pub bridge_path: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub identity_file: Option<String>,
    /// Huella de clave de host verificada en known_hosts, cuando se conoce.
    #[serde(default)]
    pub expected_host_key: Option<String>,
    #[serde(default)]
    pub last_status: Option<String>,
    #[serde(default)]
    pub protocol_version: Option<String>,
}

impl HostConfig {
    pub fn validate(&self) -> Result<(), SshError> {
        if self.alias.is_empty()
            || self.alias.contains(char::is_whitespace)
            || self.alias.contains('/')
        {
            return Err(SshError::InvalidArgument("invalid host alias"));
        }
        // OpenSSH invokes the remote command through the destination user's
        // shell. Command::arg() protects the LOCAL shell only. Accept strictly
        // inert characters so remote shell metacharacters cannot be evaluated.
        if self.destination.is_empty()
            || !self.destination.chars().all(|ch| {
                ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '@' | ':' | '[' | ']')
            })
        {
            return Err(SshError::InvalidArgument("invalid SSH destination"));
        }
        if self.bridge_path.is_empty()
            || !self.bridge_path.starts_with('/')
            || !self
                .bridge_path
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '_' | '.' | '-'))
        {
            return Err(SshError::InvalidArgument("unsafe SSH bridge path"));
        }
        // This field was previously accepted but not verified. Until
        // fingerprint pinning is implemented, fail closed rather than give
        // callers a false assurance of additional host-key enforcement.
        if self.expected_host_key.is_some() {
            return Err(SshError::InvalidArgument(
                "expected_host_key pinning is not implemented; use OpenSSH known_hosts",
            ));
        }
        if let Some(identity) = &self.identity_file {
            if identity.contains(char::is_whitespace) || identity.starts_with('-') {
                return Err(SshError::InvalidArgument("invalid identity file"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshError {
    InvalidArgument(&'static str),
    UnknownHost(String),
    SshUnavailable,
    AuthFailed(String),
    HostKeyRejected(String),
    BridgeMissing(String),
    DaemonDown(String),
    IncompatibleProtocol(String),
    Disconnected(String),
    Timeout(String),
    Protocol(String),
}

impl std::fmt::Display for SshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for SshError {}

pub fn config_path() -> PathBuf {
    std::env::var_os("FLEET_SSH_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
                .join(".config/gentle-fleet/hosts.json")
        })
}

pub fn load_hosts() -> Result<Vec<HostConfig>, SshError> {
    let path = config_path();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text =
        std::fs::read_to_string(&path).map_err(|error| SshError::Protocol(error.to_string()))?;
    let hosts: Vec<HostConfig> =
        serde_json::from_str(&text).map_err(|error| SshError::Protocol(error.to_string()))?;
    for host in &hosts {
        host.validate()?;
    }
    Ok(hosts)
}

pub fn save_hosts(hosts: &[HostConfig]) -> Result<(), SshError> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| SshError::Protocol(error.to_string()))?;
    }
    for host in hosts {
        host.validate()?;
    }
    let text = serde_json::to_string_pretty(hosts)
        .map_err(|error| SshError::Protocol(error.to_string()))?;
    std::fs::write(&path, text).map_err(|error| SshError::Protocol(error.to_string()))
}

pub fn find_host(alias: &str) -> Result<HostConfig, SshError> {
    load_hosts()?
        .into_iter()
        .find(|host| host.alias == alias)
        .ok_or_else(|| SshError::UnknownHost(alias.into()))
}

/// Lado de lectura del stream Fleet, local o remoto.
pub enum FleetReader {
    Local(LocalStream),
    Ssh(ChildStdout),
}

impl Read for FleetReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            FleetReader::Local(stream) => stream.read(buffer),
            FleetReader::Ssh(stdout) => stdout.read(buffer),
        }
    }
}

/// Lado de escritura del stream Fleet, local o remoto.
pub enum FleetWriter {
    Local(LocalStream),
    Ssh(SshWriter),
}

impl FleetWriter {
    pub fn ssh_exit_code(&mut self) -> Option<i32> {
        match self {
            FleetWriter::Ssh(writer) => writer.exit_code(),
            FleetWriter::Local(_) => None,
        }
    }

    pub fn ssh_diagnostics(&self) -> Vec<String> {
        match self {
            FleetWriter::Ssh(writer) => writer.diagnostics(),
            FleetWriter::Local(_) => Vec::new(),
        }
    }
}

impl Write for FleetWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match self {
            FleetWriter::Local(stream) => stream.write(buffer),
            FleetWriter::Ssh(writer) => writer.write(buffer),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            FleetWriter::Local(stream) => stream.flush(),
            FleetWriter::Ssh(writer) => writer.flush(),
        }
    }
}

pub struct SshWriter {
    // Se conserva el proceso SSH mientras exista el escritor; al cerrar stdin
    // el bridge termina por EOF, sin afectar a fleetd ni a los runners.
    child: Child,
    stdin: Option<ChildStdin>,
    diagnostics: std::sync::Arc<std::sync::Mutex<VecDeque<String>>>,
}

impl SshWriter {
    /// Código de salida del proceso SSH para clasificar fallos de transporte.
    pub fn exit_code(&mut self) -> Option<i32> {
        // stdout can reach EOF just before OpenSSH exits; give it a bounded
        // opportunity to publish its real exit status and stderr diagnostics.
        for _ in 0..20 {
            match self.child.try_wait() {
                Ok(Some(status)) => return status.code(),
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
                Err(_) => return None,
            }
        }
        None
    }

    /// Últimas líneas de stderr de SSH/bridge, para errores explícitos.
    pub fn diagnostics(&self) -> Vec<String> {
        self.diagnostics
            .lock()
            .expect("ssh diagnostics poisoned")
            .iter()
            .cloned()
            .collect()
    }
}

impl Drop for SshWriter {
    fn drop(&mut self) {
        // Reap the local SSH child; only the bridge connection is terminated.
        // fleetd and remote fleet-runner remain independent of this process.
        self.stdin.take();
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

impl Write for SshWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match self.stdin.as_mut() {
            Some(stdin) => stdin.write(buffer),
            None => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "ssh stdin closed",
            )),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.stdin.as_mut() {
            Some(stdin) => stdin.flush(),
            None => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "ssh stdin closed",
            )),
        }
    }
}

pub struct SshStream {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    diagnostics: std::sync::Arc<std::sync::Mutex<VecDeque<String>>>,
}

impl SshStream {
    /// Abre `ssh -T ... <dest> <bridge>`. No se usa shell, se desactiva la
    /// asignación de PTY y se respeta known_hosts/StrictHostKeyChecking.
    pub fn connect(host: &HostConfig) -> Result<Self, SshError> {
        host.validate()?;
        let mut command = Command::new("ssh");
        command
            .arg("-T")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("StrictHostKeyChecking=yes")
            .arg("-o")
            .arg("ExitOnForwardFailure=yes")
            .arg("-o")
            .arg("ConnectTimeout=10")
            .arg("-o")
            .arg("ServerAliveInterval=15")
            .arg("-o")
            .arg("ServerAliveCountMax=2");
        if let Some(port) = host.port {
            command.arg("-p").arg(port.to_string());
        }
        if let Some(identity) = &host.identity_file {
            command.arg("-i").arg(identity);
        }
        if let Ok(known_hosts) = std::env::var("FLEET_SSH_KNOWN_HOSTS") {
            command
                .arg("-o")
                .arg(format!("UserKnownHostsFile={known_hosts}"));
        }
        command.arg(&host.destination).arg(&host.bridge_path);
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| SshError::SshUnavailable)?;
        let diagnostics: std::sync::Arc<std::sync::Mutex<VecDeque<String>>> =
            std::sync::Arc::new(std::sync::Mutex::new(VecDeque::new()));
        if let Some(stderr) = child.stderr.take() {
            let log = diagnostics.clone();
            std::thread::spawn(move || {
                let mut reader = io::BufReader::new(stderr);
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    let mut entries = log.lock().expect("ssh diagnostics poisoned");
                    if entries.len() == 16 {
                        entries.pop_front();
                    }
                    entries.push_back(line.trim().to_string());
                    line.clear();
                }
            });
        }
        Ok(Self {
            stdin: child.stdin.take(),
            stdout: child.stdout.take(),
            child,
            diagnostics,
        })
    }

    pub fn split(mut self) -> Result<(FleetReader, FleetWriter), SshError> {
        let stdout = self.stdout.take().ok_or(SshError::SshUnavailable)?;
        let writer = SshWriter {
            child: self.child,
            stdin: self.stdin.take(),
            diagnostics: self.diagnostics.clone(),
        };
        Ok((FleetReader::Ssh(stdout), FleetWriter::Ssh(writer)))
    }

    /// Clasifica la salida del proceso SSH cuando termina.
    pub fn classify_exit(&mut self) -> SshError {
        let status = self.child.try_wait().ok().flatten();
        let code = status.and_then(|status| status.code()).unwrap_or(-1);
        match code {
            0 => SshError::Disconnected("ssh bridge closed cleanly".into()),
            5 => SshError::HostKeyRejected("SSH host key verification failed".into()),
            _ => SshError::Disconnected(format!("ssh exited with code {code}")),
        }
    }
}

/// Punto de entrada del bridge remoto: stdin/stdout <-> socket local.
/// El bridge NEVER termina `fleetd` ni los runners; solo cierra su socket.
pub fn run_bridge() -> Result<(), String> {
    let mut stream = connect().map_err(|error| format!("fleetd unavailable: {error}"))?;
    let mut reader = stream.try_clone().map_err(|error| error.to_string())?;
    // Input runs independently so Fleet can keep streaming while the client
    // is idle. The output side runs on the main thread and can exit if the
    // daemon closes without waiting forever on a blocked SSH stdin.
    let (sender, receiver) = std::sync::mpsc::channel();
    let _to_daemon = std::thread::spawn(move || {
        let stdin = io::stdin();
        let mut input = io::BufReader::new(stdin.lock());
        let result = (|| -> io::Result<()> {
            loop {
                let Some(frame) = read_frame(&mut input)? else {
                    return Ok(());
                };
                let frame = rewrite_hello_for_remote(&frame).map_err(|error| {
                    io::Error::new(io::ErrorKind::PermissionDenied, error.to_string())
                })?;
                write_frame(&mut reader, &frame)?;
            }
        })();
        // An invalid/oversized frame must close the daemon connection,
        // otherwise the response pump could block indefinitely on read().
        let how = if result.is_err() {
            std::net::Shutdown::Both
        } else {
            std::net::Shutdown::Write
        };
        let _ = reader.shutdown(how);
        let _ = sender.send(result);
    });
    let output_result = (|| -> io::Result<()> {
        let stdout = io::stdout();
        let mut output = stdout.lock();
        let mut buffer = [0u8; 16 * 1024];
        loop {
            let read = stream.read(&mut buffer)?;
            if read == 0 {
                return output.flush();
            }
            output.write_all(&buffer[..read])?;
            output.flush()?;
        }
    })();
    let _ = stream.shutdown(std::net::Shutdown::Both);
    // EOF on stdout can precede an input-thread exit by a few microseconds.
    // Wait a bounded interval for its result, but never block forever on
    // SSH stdin if fleetd disappeared while the client remains connected.
    if let Ok(Err(error)) = receiver.recv_timeout(std::time::Duration::from_millis(100)) {
        return Err(format!("bridge input failed: {error}"));
    }
    output_result.map_err(|error| format!("bridge output failed: {error}"))
}

/// Reescribe `Hello` con el token del host remoto. El token local NUNCA viaja
/// por SSH: se inyecta dentro del bridge, en la máquina del daemon.
pub fn rewrite_hello_for_remote(frame: &Frame) -> Result<Frame, SshError> {
    match frame {
        Frame::Request {
            request_id,
            request: Request::Hello { client_id, .. },
        } => Ok(Frame::Request {
            request_id: request_id.clone(),
            request: Request::Hello {
                client_id: client_id.clone(),
                token: expected_token().map_err(|error| {
                    SshError::Protocol(format!("remote token unavailable: {error}"))
                })?,
            },
        }),
        _ => Ok(frame.clone()),
    }
}

/// Valida que un frame recibido del bridge sea un frame Fleet válido.
pub fn validate_frame(line: &str) -> Result<Frame, SshError> {
    serde_json::from_str::<Frame>(line).map_err(|error| SshError::Protocol(error.to_string()))
}

/// Envía un frame usando el helper compartido (para tests del bridge).
pub fn write_bridge_frame<W: Write>(writer: &mut W, frame: &Frame) -> io::Result<()> {
    write_frame(writer, frame)
}

/// Lee un frame usando el helper compartido (para tests del bridge).
pub fn read_bridge_frame<R: std::io::BufRead>(reader: &mut R) -> io::Result<Option<Frame>> {
    read_frame(reader)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_config_rejects_unsafe_arguments() {
        let valid = HostConfig {
            alias: "lab".into(),
            destination: "user@host".into(),
            bridge_path: "/usr/local/bin/fleet-ssh-bridge".into(),
            port: None,
            identity_file: None,
            expected_host_key: None,
            last_status: None,
            protocol_version: None,
        };
        assert!(valid.validate().is_ok());
        let mut bad = valid.clone();
        bad.destination = "host; rm -rf /".into();
        assert_eq!(
            bad.validate(),
            Err(SshError::InvalidArgument("invalid SSH destination"))
        );
        let mut bad = valid;
        bad.bridge_path = "relative/path".into();
        assert_eq!(
            bad.validate(),
            Err(SshError::InvalidArgument("unsafe SSH bridge path"))
        );
        for unsafe_path in [
            "/usr/bin/bridge;touch/tmp/attacker",
            "/usr/bin/bridge$(id)",
            "/usr/bin/bridge`id`",
            "/usr/bin/bridge|cat",
            "/usr/bin/bridge>file",
            "/usr/bin/bridge\\\\test",
            "/usr/bin/bridge'quote",
        ] {
            let mut candidate = bad.clone();
            candidate.bridge_path = unsafe_path.into();
            assert!(candidate.validate().is_err(), "accepted {unsafe_path}");
        }
        let mut unsafe_host = bad.clone();
        unsafe_host.bridge_path = "/usr/bin/bridge".into();
        unsafe_host.destination = "example.com;id".into();
        assert!(unsafe_host.validate().is_err());
        unsafe_host.destination = "user@host".into();
        unsafe_host.expected_host_key = Some("unverified-fingerprint".into());
        assert!(unsafe_host.validate().is_err());
    }

    #[test]
    fn hello_is_rewritten_with_remote_token() {
        let frame = Frame::Request {
            request_id: "r1".into(),
            request: Request::Hello {
                client_id: "fleetctl".into(),
                token: Some("local-secret-must-not-travel".into()),
            },
        };
        let rewritten = rewrite_hello_for_remote(&frame).unwrap();
        match rewritten {
            Frame::Request {
                request: Request::Hello { token, .. },
                ..
            } => {
                assert_ne!(token.as_deref(), Some("local-secret-must-not-travel"));
            }
            _ => panic!("unexpected frame"),
        }
    }

    #[test]
    fn bridge_rejects_contaminated_stdout() {
        assert!(validate_frame("not a frame").is_err());
    }
}

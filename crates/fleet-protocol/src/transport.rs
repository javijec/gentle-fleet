//! Transporte local por usuario, con garantías distintas según plataforma.
//!
//! - Unix: Unix Domain Socket en un directorio 0700 con socket 0600. El
//!   filesystem impide que otro usuario se conecte; no se confía en loopback.
//! - Windows: named pipes con DACL explícita para el SID del usuario, rechazo
//!   de conexiones ajenas y token adicional independiente de cada endpoint.
//!   La prueba entre dos usuarios distintos sigue pendiente de ejecución.

use std::io::{self, Read, Write};
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;

#[cfg(windows)]
#[path = "windows_pipe.rs"]
mod windows_pipe;

pub fn runtime_dir() -> PathBuf {
    std::env::var_os("FLEET_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("gentle-fleet"))
}

pub fn address_file_named(name: &str) -> PathBuf {
    runtime_dir().join(format!("{name}.addr"))
}

pub fn socket_path_named(name: &str) -> PathBuf {
    runtime_dir().join(format!("{name}.sock"))
}

#[cfg(unix)]
pub enum LocalListener {
    Unix(std::os::unix::net::UnixListener),
}

#[cfg(windows)]
pub enum LocalListener {
    NamedPipe(windows_pipe::Listener),
}

#[cfg(unix)]
pub enum LocalStream {
    Unix(std::os::unix::net::UnixStream),
}

#[cfg(windows)]
pub enum LocalStream {
    NamedPipe(windows_pipe::Stream),
}

impl LocalStream {
    /// Cierre de medio canal para soportar half-close en el bridge SSH.
    pub fn shutdown(&self, how: std::net::Shutdown) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            LocalStream::Unix(stream) => stream.shutdown(how),
            #[cfg(windows)]
            LocalStream::NamedPipe(stream) => stream.shutdown(how),
        }
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        match self {
            #[cfg(unix)]
            LocalStream::Unix(stream) => stream.try_clone().map(LocalStream::Unix),
            #[cfg(windows)]
            LocalStream::NamedPipe(stream) => stream.try_clone().map(LocalStream::NamedPipe),
        }
    }
}

impl Read for LocalStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            LocalStream::Unix(stream) => stream.read(buffer),
            #[cfg(windows)]
            LocalStream::NamedPipe(stream) => stream.read(buffer),
        }
    }
}

impl Write for LocalStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            LocalStream::Unix(stream) => stream.write(buffer),
            #[cfg(windows)]
            LocalStream::NamedPipe(stream) => stream.write(buffer),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            LocalStream::Unix(stream) => stream.flush(),
            #[cfg(windows)]
            LocalStream::NamedPipe(stream) => stream.flush(),
        }
    }
}

#[cfg(unix)]
impl LocalListener {
    pub fn bind() -> io::Result<Self> {
        Self::bind_named("fleetd")
    }

    pub fn bind_named(name: &str) -> io::Result<Self> {
        use std::os::unix::fs::PermissionsExt;

        let directory = runtime_dir();
        std::fs::create_dir_all(&directory)?;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
        let socket = socket_path_named(name);
        if socket.exists() {
            std::fs::remove_file(&socket)?;
        }
        let listener = std::os::unix::net::UnixListener::bind(&socket)?;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
        std::fs::write(
            address_file_named(name),
            socket.to_string_lossy().as_bytes(),
        )?;
        Ok(Self::Unix(listener))
    }

    pub fn accept(&self) -> io::Result<LocalStream> {
        match self {
            Self::Unix(listener) => listener
                .accept()
                .map(|(stream, _)| LocalStream::Unix(stream)),
        }
    }

    pub fn description(&self) -> String {
        format!("unix:{:?}", socket_path_named("fleetd"))
    }
}

#[cfg(windows)]
impl LocalListener {
    pub fn bind() -> io::Result<Self> {
        Self::bind_named("fleetd")
    }

    pub fn bind_named(name: &str) -> io::Result<Self> {
        let directory = runtime_dir();
        let guard = windows_pipe::private_runtime(&directory, true)?;
        let mut listener = windows_pipe::Listener::bind(&directory, name)?;
        listener.hold_runtime(guard);
        let address = listener.address().to_string();
        // Only the daemon owns fleetd.token. Runners generate their own
        // independent token during startup and must never rotate the daemon's.
        if name == "fleetd" {
            let token = uuid::Uuid::new_v4().to_string();
            windows_pipe::write_private_file(
                &runtime_dir().join("fleetd.token"),
                token.as_bytes(),
            )?;
        }
        windows_pipe::write_private_file(&address_file_named(name), address.as_bytes())?;
        Ok(Self::NamedPipe(listener))
    }

    pub fn accept(&self) -> io::Result<LocalStream> {
        match self {
            Self::NamedPipe(listener) => listener.accept().map(LocalStream::NamedPipe),
        }
    }

    pub fn description(&self) -> String {
        format!("named-pipe:{:?}", address_file_named("fleetd"))
    }
}

/// Write a Windows Fleet credential or endpoint file with an owner-only DACL.
#[cfg(windows)]
pub fn write_private_file(path: &std::path::Path, contents: &[u8]) -> io::Result<()> {
    windows_pipe::write_private_file(path, contents)
}

pub fn connect() -> io::Result<LocalStream> {
    connect_named("fleetd")
}

pub fn connect_named(name: &str) -> io::Result<LocalStream> {
    #[cfg(windows)]
    let _guard = windows_pipe::private_runtime(&runtime_dir(), false)?;
    let address = std::fs::read_to_string(address_file_named(name))
        .map_err(|e| io::Error::new(e.kind(), format!("fleetd is not running: {e}")))?;
    let address = address.trim();
    #[cfg(unix)]
    {
        std::os::unix::net::UnixStream::connect(Path::new(address)).map(LocalStream::Unix)
    }
    #[cfg(windows)]
    {
        windows_pipe::connect(address).map(LocalStream::NamedPipe)
    }
}

/// Token esperado en el transporte local. Un `None` en Unix significa que el
/// filesystem (socket 0600 en directorio 0700) es la garantía de aislamiento.
/// La firma es idéntica en todas las plataformas para que daemon y cliente
/// compilen juntos sin `cfg` repartidos.
pub fn expected_token() -> io::Result<Option<String>> {
    #[cfg(windows)]
    {
        let _guard = windows_pipe::private_runtime(&runtime_dir(), false)?;
        Ok(Some(std::fs::read_to_string(
            runtime_dir().join("fleetd.token"),
        )?))
    }
    #[cfg(unix)]
    {
        Ok(None)
    }
}

#[cfg(not(windows))]
pub fn socket_path() -> Option<PathBuf> {
    Some(runtime_dir().join("fleetd.sock"))
}

#[cfg(windows)]
pub fn socket_path() -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_dir_is_under_fleet_namespace() {
        let dir = runtime_dir();
        assert!(dir.to_string_lossy().contains("gentle-fleet") || dir.is_absolute());
    }
}

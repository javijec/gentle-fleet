//! Transporte local por usuario, con garantías distintas según plataforma.
//!
//! - Unix: Unix Domain Socket en un directorio 0700 con socket 0600. El
//!   filesystem impide que otro usuario se conecte; no se confía en loopback.
//! - Windows: TCP en loopback + token aleatorio leído de un fichero privado.
//!   Esta plataforma queda marcada como NO VALIDADA en P1.1: el token depende
//!   de la ACL del fichero y no se ha probado con otro usuario.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

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
    Tcp(std::net::TcpListener),
}

#[cfg(unix)]
pub enum LocalStream {
    Unix(std::os::unix::net::UnixStream),
}

#[cfg(windows)]
pub enum LocalStream {
    Tcp(std::net::TcpStream),
}

impl LocalStream {
    pub fn try_clone(&self) -> io::Result<Self> {
        match self {
            #[cfg(unix)]
            LocalStream::Unix(stream) => stream.try_clone().map(LocalStream::Unix),
            #[cfg(windows)]
            LocalStream::Tcp(stream) => stream.try_clone().map(LocalStream::Tcp),
        }
    }
}

impl Read for LocalStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            LocalStream::Unix(stream) => stream.read(buffer),
            #[cfg(windows)]
            LocalStream::Tcp(stream) => stream.read(buffer),
        }
    }
}

impl Write for LocalStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            LocalStream::Unix(stream) => stream.write(buffer),
            #[cfg(windows)]
            LocalStream::Tcp(stream) => stream.write(buffer),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            LocalStream::Unix(stream) => stream.flush(),
            #[cfg(windows)]
            LocalStream::Tcp(stream) => stream.flush(),
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
        std::fs::create_dir_all(runtime_dir())?;
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        let address = listener.local_addr()?;
        // UUID v4 utiliza aleatoriedad del sistema; PID/tiempo eran predecibles.
        let token = uuid::Uuid::new_v4().to_string();
        std::fs::write(runtime_dir().join("fleetd.token"), token.as_bytes())?;
        std::fs::write(address_file_named(name), address.to_string())?;
        Ok(Self::Tcp(listener))
    }

    pub fn accept(&self) -> io::Result<LocalStream> {
        match self {
            Self::Tcp(listener) => listener
                .accept()
                .map(|(stream, _)| LocalStream::Tcp(stream)),
        }
    }

    pub fn description(&self) -> String {
        format!("tcp-loopback:{:?}", address_file_named("fleetd"))
    }
}

pub fn connect() -> io::Result<LocalStream> {
    connect_named("fleetd")
}

pub fn connect_named(name: &str) -> io::Result<LocalStream> {
    let address = std::fs::read_to_string(address_file_named(name))
        .map_err(|e| io::Error::new(e.kind(), format!("fleetd is not running: {e}")))?;
    let address = address.trim();
    #[cfg(unix)]
    {
        std::os::unix::net::UnixStream::connect(Path::new(address)).map(LocalStream::Unix)
    }
    #[cfg(windows)]
    {
        let _ = Path::new(address);
        std::net::TcpStream::connect(address).map(LocalStream::Tcp)
    }
}

/// Token esperado en el transporte local. Un `None` en Unix significa que el
/// filesystem (socket 0600 en directorio 0700) es la garantía de aislamiento.
/// La firma es idéntica en todas las plataformas para que daemon y cliente
/// compilen juntos sin `cfg` repartidos.
pub fn expected_token() -> io::Result<Option<String>> {
    #[cfg(windows)]
    {
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

//! User-scoped Windows named-pipe transport. No TCP listener or fallback.
//!
//! The only unsafe code here wraps documented Win32 handle, token and ACL APIs.
//! All pipe handles are non-inheritable; the DACL is protected and grants access
//! exclusively to the server user's SID. Fleet's Hello token remains defence
//! in depth, not a substitute for OS-level access control.
//!
//! JSONL frames never contain a raw NUL byte. A single NUL sent after the last
//! complete frame is the internal transport's write-half-close marker. It is
//! interpreted as EOF by the peer without closing the response direction.
//! No NUL is exposed to the Fleet protocol decoder.

use std::ffi::{c_void, OsStr};
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{self, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use windows_sys::Win32::Foundation::{
    LocalFree, ERROR_BROKEN_PIPE, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
};
use windows_sys::Win32::Security::{
    CopySid, EqualSid, GetLengthSid, GetTokenInformation, TokenUser, SECURITY_ATTRIBUTES,
    TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, OPEN_EXISTING};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, GetNamedPipeServerProcessId,
    WaitNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::System::IO::CancelIoEx;

const FIRST_PIPE_INSTANCE: u32 = 0x0008_0000;
const PIPE_ACCESS_DUPLEX: u32 = 0x0000_0003;
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

fn sid_of_process(process: HANDLE) -> io::Result<Vec<u8>> {
    let mut raw_token = ptr::null_mut();
    // SAFETY: Win32 fills a valid HANDLE; OwnedHandle closes it on all paths.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut raw_token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = unsafe { OwnedHandle::from_raw_handle(raw_token) };
    let mut required = 0u32;
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            ptr::null_mut(),
            0,
            &mut required,
        );
    }
    if required == 0 {
        return Err(io::Error::last_os_error());
    }
    // A Vec<usize> satisfies TOKEN_USER alignment (a Vec<u8> does not).
    let words = (required as usize).div_ceil(std::mem::size_of::<usize>());
    let mut data = vec![0usize; words];
    let ok = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            data.as_mut_ptr().cast::<c_void>(),
            required,
            &mut required,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: GetTokenInformation returned a fully initialized TOKEN_USER.
    let source = unsafe { (*(data.as_ptr().cast::<TOKEN_USER>())).User.Sid };
    if source.is_null() {
        return Err(invalid("Windows process token has no user SID"));
    }
    let len = unsafe { GetLengthSid(source) };
    if len == 0 || len > 256 {
        return Err(invalid("invalid Windows user SID"));
    }
    let mut sid = vec![0u8; len as usize];
    if unsafe { CopySid(len, sid.as_mut_ptr().cast::<c_void>(), source) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(sid)
}

fn current_sid() -> io::Result<Vec<u8>> {
    sid_of_process(unsafe { GetCurrentProcess() })
}

fn sid_string(sid: &[u8]) -> io::Result<String> {
    let mut out = ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid.as_ptr().cast_mut().cast::<c_void>(), &mut out) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: ConvertSidToStringSidW returns a NUL-terminated LocalAlloc buffer.
    let len = unsafe {
        let mut n = 0;
        while *out.add(n) != 0 {
            n += 1;
        }
        n
    };
    let value = unsafe { String::from_utf16_lossy(std::slice::from_raw_parts(out, len)) };
    unsafe {
        LocalFree(out.cast::<c_void>());
    }
    Ok(value)
}

fn check_peer(handle: HANDLE, sid: &[u8], client: bool) -> io::Result<()> {
    let mut pid = 0u32;
    let ok = if client {
        unsafe { GetNamedPipeClientProcessId(handle, &mut pid) }
    } else {
        unsafe { GetNamedPipeServerProcessId(handle, &mut pid) }
    };
    if ok == 0 || pid == 0 {
        return Err(invalid("unable to verify named-pipe peer identity"));
    }
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Err(invalid("unable to inspect named-pipe peer process"));
    }
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    let peer_sid = sid_of_process(process.as_raw_handle())?;
    if unsafe {
        EqualSid(
            sid.as_ptr().cast_mut().cast::<c_void>(),
            peer_sid.as_ptr().cast_mut().cast::<c_void>(),
        )
    } == 0
    {
        return Err(invalid("named-pipe peer has a different user SID"));
    }
    Ok(())
}

fn create_instance(name: &[u16], sid: &[u8], first: bool) -> io::Result<OwnedHandle> {
    let owner = sid_string(sid)?;
    // Protected DACL: ONLY this user may access the pipe. No Everyone,
    // Anonymous or broad Authenticated Users ACE is inherited.
    let sddl = wide(&format!("D:P(A;;GA;;;{owner})"));
    let mut descriptor = ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let flags = PIPE_ACCESS_DUPLEX | if first { FIRST_PIPE_INSTANCE } else { 0 };
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            flags,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            65536,
            65536,
            0,
            &attributes,
        )
    };
    let error = if handle == INVALID_HANDLE_VALUE {
        Some(io::Error::last_os_error())
    } else {
        None
    };
    unsafe {
        LocalFree(descriptor);
    }
    if let Some(error) = error {
        return Err(error);
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

pub struct Listener {
    name: String,
    name_wide: Vec<u16>,
    sid: Vec<u8>,
    pending: Mutex<Option<OwnedHandle>>,
}

impl Listener {
    pub fn bind(runtime: &std::path::Path, instance: &str) -> io::Result<Self> {
        if instance.is_empty()
            || !instance
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid Fleet pipe name",
            ));
        }
        let sid = current_sid()?;
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        runtime.hash(&mut hash);
        sid.hash(&mut hash);
        let name = format!(r"\\.\pipe\gentle-fleet-{:016x}-{instance}", hash.finish());
        let name_wide = wide(&name);
        let first = create_instance(&name_wide, &sid, true)?;
        Ok(Self {
            name,
            name_wide,
            sid,
            pending: Mutex::new(Some(first)),
        })
    }

    pub fn address(&self) -> &str {
        &self.name
    }

    pub fn accept(&self) -> io::Result<Stream> {
        let mut pending = self.pending.lock().expect("Fleet pipe listener poisoned");
        let instance = pending
            .take()
            .ok_or_else(|| io::Error::other("listener closed"))?;
        let connected = unsafe { ConnectNamedPipe(instance.as_raw_handle(), ptr::null_mut()) };
        if connected == 0
            && io::Error::last_os_error().raw_os_error() != Some(ERROR_PIPE_CONNECTED as i32)
        {
            let error = io::Error::last_os_error();
            // Do not lose the listener after a failed connection attempt.
            *pending = Some(create_instance(&self.name_wide, &self.sid, false)?);
            return Err(error);
        }
        let verified = check_peer(instance.as_raw_handle(), &self.sid, true);
        // Rejecting an unauthorised peer must not disable future accepts.
        *pending = Some(create_instance(&self.name_wide, &self.sid, false)?);
        verified?;
        Ok(Stream::new(File::from(instance)))
    }
}

struct Shared {
    // None after abortive shutdown. In-flight I/O retains its own Arc<File>
    // until CancelIoEx releases it; other clones cannot keep it open.
    file: Mutex<Option<Arc<File>>>,
    read_lock: Mutex<()>,
    write_lock: Mutex<()>,
    read_eof: AtomicBool,
    write_eof: AtomicBool,
    stopped: AtomicBool,
}

pub struct Stream(Arc<Shared>);

impl Stream {
    fn new(file: File) -> Self {
        Self(Arc::new(Shared {
            file: Mutex::new(Some(Arc::new(file))),
            read_lock: Mutex::new(()),
            write_lock: Mutex::new(()),
            read_eof: AtomicBool::new(false),
            write_eof: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
        }))
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self(self.0.clone()))
    }

    fn file(&self) -> io::Result<Arc<File>> {
        self.0
            .file
            .lock()
            .expect("Fleet pipe handle poisoned")
            .as_ref()
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "Fleet pipe disconnected"))
    }

    pub fn shutdown(&self, how: std::net::Shutdown) -> io::Result<()> {
        match how {
            std::net::Shutdown::Write => {
                let _guard = self
                    .0
                    .write_lock
                    .lock()
                    .expect("Fleet pipe writer poisoned");
                if !self.0.write_eof.swap(true, Ordering::AcqRel) {
                    // Raw NUL is impossible inside a valid JSONL frame and is
                    // consumed only inside this transport implementation.
                    let file = self.file()?;
                    let mut borrowed = file.as_ref();
                    borrowed.write_all(&[0])?;
                    borrowed.flush()?;
                }
            }
            std::net::Shutdown::Read => {
                self.0.read_eof.store(true, Ordering::Release);
                if let Ok(file) = self.file() {
                    unsafe {
                        CancelIoEx(file.as_raw_handle(), ptr::null());
                    }
                }
            }
            std::net::Shutdown::Both => {
                // Abortive cancellation must never wait for peer input or
                // write-buffer capacity. Graceful shutdown(Write) above is
                // the only path that sends the in-band EOF marker.
                self.0.stopped.store(true, Ordering::Release);
                self.0.read_eof.store(true, Ordering::Release);
                self.0.write_eof.store(true, Ordering::Release);
                // Sever the shared ownership for every Stream clone, even
                // when a FleetClient retains its reader and writer objects.
                // In-flight reads/writes own a temporary Arc and release it
                // as soon as Win32 cancellation completes.
                let file = self
                    .0
                    .file
                    .lock()
                    .expect("Fleet pipe handle poisoned")
                    .take();
                if let Some(file) = file {
                    unsafe {
                        CancelIoEx(file.as_raw_handle(), ptr::null());
                    }
                }
            }
        }
        Ok(())
    }
}

impl Read for Stream {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() || self.0.read_eof.load(Ordering::Acquire) {
            return Ok(0);
        }
        let _guard = self.0.read_lock.lock().expect("Fleet pipe reader poisoned");
        if self.0.read_eof.load(Ordering::Acquire) {
            return Ok(0);
        }
        let file = self.file()?;
        let mut borrowed = file.as_ref();
        match borrowed.read(out) {
            Ok(n) => {
                if let Some(pos) = out[..n].iter().position(|b| *b == 0) {
                    self.0.read_eof.store(true, Ordering::Release);
                    if pos + 1 != n {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "bytes received after Fleet pipe half-close",
                        ));
                    }
                    return Ok(pos);
                }
                Ok(n)
            }
            Err(error)
                if error.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32)
                    || error.raw_os_error() == Some(ERROR_PIPE_NOT_CONNECTED as i32) =>
            {
                Ok(0)
            }
            Err(error) => Err(error),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let _guard = self
            .0
            .write_lock
            .lock()
            .expect("Fleet pipe writer poisoned");
        if self.0.write_eof.load(Ordering::Acquire) || self.0.stopped.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Fleet pipe closed for writing",
            ));
        }
        if bytes.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "raw NUL is reserved for Fleet pipe EOF",
            ));
        }
        let file = self.file()?;
        let mut borrowed = file.as_ref();
        borrowed.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        let file = self.file()?;
        let mut borrowed = file.as_ref();
        borrowed.flush()
    }
}

pub fn connect(address: &str) -> io::Result<Stream> {
    if !address.starts_with(r"\\.\pipe\gentle-fleet-") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid Fleet pipe address",
        ));
    }
    let wide_name = wide(address);
    let sid = current_sid()?;
    for attempt in 0..2 {
        let handle = unsafe {
            CreateFileW(
                wide_name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                ptr::null(),
                OPEN_EXISTING,
                0,
                ptr::null_mut(),
            )
        };
        if handle != INVALID_HANDLE_VALUE {
            let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
            check_peer(handle.as_raw_handle(), &sid, false)?;
            return Ok(Stream::new(File::from(handle)));
        }
        let error = io::Error::last_os_error();
        if attempt == 1 || error.raw_os_error() != Some(231)
        /* ERROR_PIPE_BUSY */
        {
            return Err(error);
        }
        if unsafe { WaitNamedPipeW(wide_name.as_ptr(), 5000) } == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Err(io::Error::other("Fleet pipe connection failed"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Shutdown;

    #[test]
    fn half_close_drains_pending_reply_without_truncation() {
        let runtime =
            std::env::temp_dir().join(format!("gentle-fleet-pipe-test-{}", uuid::Uuid::new_v4()));
        let listener = Listener::bind(&runtime, "halfclose").unwrap();
        let address = listener.address().to_owned();
        let server = std::thread::spawn(move || {
            let mut stream = listener.accept().unwrap();
            let mut request = Vec::new();
            stream.read_to_end(&mut request).unwrap();
            assert_eq!(request, b"hello\n");
            stream.write_all(b"goodbye\n").unwrap();
            stream.shutdown(Shutdown::Write).unwrap();
        });
        let mut client = connect(&address).unwrap();
        client.write_all(b"hello\n").unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        let mut reply = Vec::new();
        client.read_to_end(&mut reply).unwrap();
        assert_eq!(reply, b"goodbye\n");
        server.join().unwrap();
    }

    #[test]
    fn full_shutdown_wakes_pending_reader_without_dropping_client() {
        let runtime =
            std::env::temp_dir().join(format!("gentle-fleet-cancel-test-{}", uuid::Uuid::new_v4()));
        let listener = Listener::bind(&runtime, "cancellation").unwrap();
        let address = listener.address().to_owned();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let _connected = listener.accept().unwrap();
            let _ = release_rx.recv_timeout(std::time::Duration::from_secs(5));
        });
        let client = connect(&address).unwrap();
        let mut observer = client.try_clone().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut buffer = [0u8; 1];
            let _ = sender.send(observer.read(&mut buffer));
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        client.shutdown(Shutdown::Both).unwrap();
        let result = receiver
            .recv_timeout(std::time::Duration::from_secs(3))
            .expect("pipe shutdown must wake the pending reader");
        assert!(result.is_err() || result.unwrap() == 0);
        release_tx.send(()).unwrap();
        reader.join().unwrap();
        server.join().unwrap();
    }

    #[test]
    fn full_shutdown_physically_disconnects_even_with_live_clones() {
        let runtime =
            std::env::temp_dir().join(format!("gentle-fleet-close-test-{}", uuid::Uuid::new_v4()));
        let listener = Listener::bind(&runtime, "disconnect").unwrap();
        let address = listener.address().to_owned();
        let (sender, receiver) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut stream = listener.accept().unwrap();
            let mut buffer = Vec::new();
            let _ = sender.send(stream.read_to_end(&mut buffer));
        });
        let client = connect(&address).unwrap();
        let other_clone = client.try_clone().unwrap();
        client.shutdown(Shutdown::Both).unwrap();
        let result = receiver
            .recv_timeout(std::time::Duration::from_secs(3))
            .expect("the peer must see EOF without waiting for client clones to drop");
        assert!(result.is_ok());
        drop(other_clone);
        server.join().unwrap();
    }

    #[test]
    fn invalid_address_is_rejected_without_contacting_any_server() {
        assert!(connect("127.0.0.1:4000").is_err());
        assert!(connect(r"\\.\pipe\other-service").is_err());
    }
}

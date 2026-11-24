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
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use windows_sys::Win32::Foundation::{
    LocalFree, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED,
    HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
};
use windows_sys::Win32::Security::{
    CopySid, EqualSid, GetLengthSid, GetTokenInformation, TokenUser, SECURITY_ATTRIBUTES,
    TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED,
    FILE_SHARE_READ, OPEN_EXISTING,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, GetNamedPipeServerProcessId,
    WaitNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, OpenProcess, OpenProcessToken, WaitForSingleObject, INFINITE,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

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

fn overlapped_op(
    handle: HANDLE,
    timeout: Option<std::time::Duration>,
    start: impl FnOnce(*mut OVERLAPPED, *mut u32) -> i32,
) -> io::Result<u32> {
    // Each operation owns its event and OVERLAPPED until completion, including
    // after cancellation. No pointer outlives the blocking result wait.
    let event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
    if event.is_null() {
        return Err(io::Error::last_os_error());
    }
    let event = unsafe { OwnedHandle::from_raw_handle(event) };
    let mut operation: OVERLAPPED = unsafe { std::mem::zeroed() };
    operation.hEvent = event.as_raw_handle();
    let mut transferred = 0u32;
    if start(&mut operation, &mut transferred) != 0 {
        return Ok(transferred);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
        return Err(error);
    }
    let milliseconds = timeout.map_or(INFINITE, |duration| {
        duration.as_millis().max(1).min((INFINITE - 1) as u128) as u32
    });
    let wait = unsafe { WaitForSingleObject(event.as_raw_handle(), milliseconds) };
    if wait != WAIT_OBJECT_0 {
        let wait_error = if wait == WAIT_TIMEOUT {
            io::Error::new(io::ErrorKind::TimedOut, "Fleet pipe I/O timed out")
        } else {
            io::Error::last_os_error()
        };
        // The operation's stack-backed OVERLAPPED and buffer must remain
        // alive until the kernel reports completion, even if it raced timeout.
        unsafe {
            CancelIoEx(handle, &operation);
            GetOverlappedResult(handle, &operation, &mut transferred, 1);
        }
        return Err(wait_error);
    }
    if unsafe { GetOverlappedResult(handle, &operation, &mut transferred, 1) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(transferred)
}

fn pipe_read(
    file: &File,
    buffer: &mut [u8],
    timeout: Option<std::time::Duration>,
) -> io::Result<usize> {
    let handle = file.as_raw_handle();
    let len = buffer.len().min(u32::MAX as usize) as u32;
    overlapped_op(handle, timeout, |operation, count| unsafe {
        ReadFile(handle, buffer.as_mut_ptr(), len, count, operation)
    })
    .map(|n| n as usize)
}

fn pipe_write(
    file: &File,
    buffer: &[u8],
    timeout: Option<std::time::Duration>,
) -> io::Result<usize> {
    let handle = file.as_raw_handle();
    let len = buffer.len().min(u32::MAX as usize) as u32;
    overlapped_op(handle, timeout, |operation, count| unsafe {
        WriteFile(handle, buffer.as_ptr(), len, count, operation)
    })
    .map(|n| n as usize)
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
    let flags =
        PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | if first { FIRST_PIPE_INSTANCE } else { 0 };
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

/// Create a Fleet endpoint credential or address file with a protected,
/// owner-only DACL *before* its contents become visible to any other process.
///
/// CREATE_NEW is mandatory: CreateFileW ignores the security descriptor if
/// an existing file is opened. An old file is removed only after Fleet has
/// reserved its named-pipe endpoint, so a second Fleet daemon cannot rotate
/// the first daemon's credentials. If another process races the creation,
/// CREATE_NEW fails closed rather than overwriting its file.
///
/// This does not protect an insecure parent directory from DELETE_CHILD:
/// runtime-directory validation remains an explicit W1-B security gate.
pub fn write_private_file(path: &Path, contents: &[u8]) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Fleet credential path must be absolute",
        ));
    }
    let sid = current_sid()?;
    let owner = sid_string(&sid)?;
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
    // Remove a stale file rather than reopening it with inherited DACL.
    // A symlink at this path is removed, not followed; CREATE_NEW then
    // refuses any unexpected replacement created during the race.
    let removed = match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    };
    let created = removed.and_then(|()| {
        let path_wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let handle = unsafe {
            CreateFileW(
                path_wide.as_ptr(),
                GENERIC_WRITE,
                FILE_SHARE_READ,
                &attributes,
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
        let mut file = File::from(handle);
        file.write_all(contents)?;
        file.sync_all()
    });
    unsafe {
        LocalFree(descriptor);
    }
    created
}

// Keep a directory handle without FILE_SHARE_DELETE for the lifetime of
// the listener. Reject pre-existing directories with unexpected ACLs rather
// than silently taking ownership or changing permissions on arbitrary paths.
pub fn private_runtime(path: &Path, create: bool) -> io::Result<OwnedHandle> {
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateDirectoryW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_WRITE,
    };
    if !path.is_absolute() {
        return Err(invalid("Fleet runtime must be an absolute path"));
    }
    let sid = sid_string(&current_sid()?)?;
    let expected = format!("O:{sid}D:P(A;;FA;;;{sid})");
    let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    if create {
        let mut descriptor = ptr::null_mut();
        let sddl = wide(&expected);
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
        let ok = unsafe { CreateDirectoryW(name.as_ptr(), &attributes) };
        let error = io::Error::last_os_error();
        unsafe {
            LocalFree(descriptor);
        }
        if ok == 0 && error.raw_os_error() != Some(183) {
            return Err(error);
        }
    }
    // READ_CONTROL, with no delete sharing, and do not follow a junction.
    let raw = unsafe {
        CreateFileW(
            name.as_ptr(),
            0x0002_0000,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(raw, &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
        || info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(invalid(
            "Fleet runtime must be a real directory, not a reparse point",
        ));
    }
    let flags = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
    let mut descriptor = ptr::null_mut();
    let status = unsafe {
        GetSecurityInfo(
            raw,
            SE_FILE_OBJECT,
            flags,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let mut actual = ptr::null_mut();
    let ok = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            1,
            flags,
            &mut actual,
            ptr::null_mut(),
        )
    };
    let error = io::Error::last_os_error();
    unsafe {
        LocalFree(descriptor);
    }
    if ok == 0 {
        return Err(error);
    }
    let mut len = 0;
    unsafe {
        while *actual.add(len) != 0 {
            len += 1;
        }
    }
    let actual_string =
        unsafe { String::from_utf16_lossy(std::slice::from_raw_parts(actual, len)) };
    unsafe {
        LocalFree(actual.cast());
    }
    if actual_string != expected {
        return Err(invalid(
            "Fleet runtime owner or DACL is not private; choose a new runtime directory",
        ));
    }
    Ok(handle)
}

pub struct Listener {
    _runtime_guard: Option<OwnedHandle>,
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
            _runtime_guard: None,
            name,
            name_wide,
            sid,
            pending: Mutex::new(Some(first)),
        })
    }

    pub fn hold_runtime(&mut self, guard: OwnedHandle) {
        self._runtime_guard = Some(guard);
    }

    pub fn address(&self) -> &str {
        &self.name
    }

    pub fn accept(&self) -> io::Result<Stream> {
        let mut pending = self.pending.lock().expect("Fleet pipe listener poisoned");
        let instance = pending
            .take()
            .ok_or_else(|| io::Error::other("listener closed"))?;
        let handle = instance.as_raw_handle();
        let result = overlapped_op(handle, None, |operation, _| unsafe {
            ConnectNamedPipe(handle, operation)
        });
        if let Err(error) = result {
            if error.raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32) {
                // Client connected between CreateNamedPipe and ConnectNamedPipe.
            } else {
                // Do not lose the listener after a failed connection attempt.
                *pending = Some(create_instance(&self.name_wide, &self.sid, false)?);
                return Err(error);
            }
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
    read_timeout: Mutex<Option<std::time::Duration>>,
    write_timeout: Mutex<Option<std::time::Duration>>,
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
            read_timeout: Mutex::new(None),
            write_timeout: Mutex::new(None),
        }))
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self(self.0.clone()))
    }

    pub fn set_read_timeout(&self, timeout: Option<std::time::Duration>) -> io::Result<()> {
        if timeout == Some(std::time::Duration::ZERO) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "zero timeout"));
        }
        *self.0.read_timeout.lock().expect("pipe timeout poisoned") = timeout;
        Ok(())
    }

    pub fn set_write_timeout(&self, timeout: Option<std::time::Duration>) -> io::Result<()> {
        if timeout == Some(std::time::Duration::ZERO) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "zero timeout"));
        }
        *self.0.write_timeout.lock().expect("pipe timeout poisoned") = timeout;
        Ok(())
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
                    let timeout = *self.0.write_timeout.lock().expect("pipe timeout poisoned");
                    if pipe_write(&file, &[0], timeout)? != 1 {
                        return Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "pipe EOF not sent",
                        ));
                    }
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
        let timeout = *self.0.read_timeout.lock().expect("pipe timeout poisoned");
        match pipe_read(&file, out, timeout) {
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
        let timeout = *self.0.write_timeout.lock().expect("pipe timeout poisoned");
        pipe_write(&file, bytes, timeout)
    }
    fn flush(&mut self) -> io::Result<()> {
        // WriteFile completion has already handed bytes to the pipe. Calling
        // FlushFileBuffers here waits for the peer to read and can deadlock a
        // full-duplex request/response exchange.
        self.file().map(|_| ())
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
                FILE_FLAG_OVERLAPPED,
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
    fn concurrent_read_does_not_prevent_cloned_writer_from_sending() {
        let runtime =
            std::env::temp_dir().join(format!("gentle-fleet-duplex-test-{}", uuid::Uuid::new_v4()));
        let listener = Listener::bind(&runtime, "duplex").unwrap();
        let address = listener.address().to_owned();
        let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut stream = listener.accept().unwrap();
            accepted_tx.send(()).unwrap();
            let mut byte = [0u8; 1];
            stream.read_exact(&mut byte).unwrap();
            stream.write_all(&byte).unwrap();
        });
        let client = connect(&address).unwrap();
        accepted_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let mut reader = client.try_clone().unwrap();
        let mut writer = client.try_clone().unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let mut byte = [0u8; 1];
            let result = reader.read_exact(&mut byte).map(|()| byte[0]);
            let _ = done_tx.send(result);
        });
        started_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        // Allow the reader to enter the blocking OS read before the write.
        std::thread::sleep(std::time::Duration::from_millis(100));
        let (sent_tx, sent_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sent_tx.send(writer.write_all(b"x"));
        });
        let sent = sent_rx.recv_timeout(std::time::Duration::from_secs(2));
        if sent.is_err() {
            let _ = client.shutdown(Shutdown::Both);
        }
        sent.expect("blocked read must not block a concurrent writer")
            .unwrap();
        assert_eq!(
            done_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap()
                .unwrap(),
            b'x'
        );
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
    fn read_timeout_is_bounded_and_does_not_break_the_connection() {
        let runtime =
            std::env::temp_dir().join(format!("gentle-fleet-timeout-{}", uuid::Uuid::new_v4()));
        let listener = Listener::bind(&runtime, "timeout").unwrap();
        let address = listener.address().to_owned();
        let (send, receive) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut stream = listener.accept().unwrap();
            receive.recv().unwrap();
            stream.write_all(b"x").unwrap();
        });
        let mut client = connect(&address).unwrap();
        client
            .set_read_timeout(Some(std::time::Duration::from_millis(100)))
            .unwrap();
        let mut byte = [0u8; 1];
        assert_eq!(
            client.read(&mut byte).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        send.send(()).unwrap();
        client.read_exact(&mut byte).unwrap();
        assert_eq!(byte, *b"x");
        server.join().unwrap();
    }

    #[test]
    fn full_shutdown_physically_disconnects_even_with_live_clones() {
        let runtime =
            std::env::temp_dir().join(format!("gentle-fleet-close-test-{}", uuid::Uuid::new_v4()));
        let listener = Listener::bind(&runtime, "disconnect").unwrap();
        let address = listener.address().to_owned();
        let (sender, receiver) = std::sync::mpsc::channel();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut stream = listener.accept().unwrap();
            ready_tx.send(()).unwrap();
            let mut buffer = Vec::new();
            let _ = sender.send(stream.read_to_end(&mut buffer));
        });
        let client = connect(&address).unwrap();
        let other_clone = client.try_clone().unwrap();
        ready_rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .expect("server must accept before the client tests abortive shutdown");
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

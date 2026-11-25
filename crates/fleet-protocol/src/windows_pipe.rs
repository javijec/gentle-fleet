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
use std::path::{Path, PathBuf};
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
    CopySid, EqualSid, GetLengthSid, GetTokenInformation, RevertToSelf, TokenUser,
    SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED,
    FILE_SHARE_READ, OPEN_EXISTING, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, GetNamedPipeServerProcessId,
    ImpersonateNamedPipeClient, WaitNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetCurrentThread, OpenProcess, OpenProcessToken,
    OpenThreadToken, WaitForSingleObject, INFINITE, PROCESS_QUERY_LIMITED_INFORMATION,
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

fn timeout_completion(wait_error: io::Error, completion: io::Result<u32>) -> io::Result<u32> {
    match completion {
        Ok(transferred) => Ok(transferred),
        Err(error)
            if error.raw_os_error()
                == Some(windows_sys::Win32::Foundation::ERROR_OPERATION_ABORTED as i32) =>
        {
            Err(wait_error)
        }
        Err(error) => Err(error),
    }
}

fn overlapped_op(
    handle: HANDLE,
    timeout: Option<std::time::Duration>,
    stream: Option<(&Shared, bool)>,
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
    let operation_ptr = &raw mut operation;
    let mut transferred = 0u32;
    // Serialize only submission, never the blocking wait. Shutdown takes
    // this same gate, so it cannot miss an operation registered after cancel.
    let mut gate =
        stream.map(|(shared, _)| shared.submission.lock().expect("pipe submission poisoned"));
    if let Some((shared, reading)) = stream {
        if shared.stopped.load(Ordering::Acquire)
            || (reading && shared.read_eof.load(Ordering::Acquire))
        {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Fleet pipe disconnected",
            ));
        }
    }
    if start(operation_ptr, &mut transferred) != 0 {
        return Ok(transferred);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
        return Err(error);
    }
    if let (Some((_, true)), Some(gate)) = (stream, gate.as_mut()) {
        **gate = Some(operation_ptr as usize);
    }
    drop(gate);
    let milliseconds = timeout.map_or(INFINITE, |duration| {
        duration.as_millis().max(1).min((INFINITE - 1) as u128) as u32
    });
    let wait = unsafe { WaitForSingleObject(event.as_raw_handle(), milliseconds) };
    let result = if wait != WAIT_OBJECT_0 {
        let wait_error = if wait == WAIT_TIMEOUT {
            io::Error::new(io::ErrorKind::TimedOut, "Fleet pipe I/O timed out")
        } else {
            io::Error::last_os_error()
        };
        // The operation's stack-backed OVERLAPPED and buffer must remain
        // alive until the kernel reports completion, even if it raced timeout.
        let completed = unsafe {
            CancelIoEx(handle, operation_ptr);
            GetOverlappedResult(handle, operation_ptr, &mut transferred, 1)
        };
        let completion = if completed != 0 {
            Ok(transferred)
        } else {
            Err(io::Error::last_os_error())
        };
        timeout_completion(wait_error, completion)
    } else if unsafe { GetOverlappedResult(handle, operation_ptr, &mut transferred, 1) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(transferred)
    };
    if let Some((shared, true)) = stream {
        // read_lock guarantees that only this reader can own the slot.
        // Clear it under the gate before the stack OVERLAPPED goes away.
        *shared.submission.lock().expect("pipe submission poisoned") = None;
    }
    result
}

fn pipe_read(
    file: &File,
    buffer: &mut [u8],
    timeout: Option<std::time::Duration>,
    shared: &Shared,
) -> io::Result<usize> {
    let handle = file.as_raw_handle();
    let len = buffer.len().min(u32::MAX as usize) as u32;
    overlapped_op(
        handle,
        timeout,
        Some((shared, true)),
        |operation, count| unsafe { ReadFile(handle, buffer.as_mut_ptr(), len, count, operation) },
    )
    .map(|n| n as usize)
}

fn pipe_write(
    file: &File,
    buffer: &[u8],
    timeout: Option<std::time::Duration>,
    shared: &Shared,
) -> io::Result<usize> {
    let handle = file.as_raw_handle();
    let len = buffer.len().min(u32::MAX as usize) as u32;
    overlapped_op(
        handle,
        timeout,
        Some((shared, false)),
        |operation, count| unsafe { WriteFile(handle, buffer.as_ptr(), len, count, operation) },
    )
    .map(|n| n as usize)
}

fn sid_of_process(process: HANDLE) -> io::Result<Vec<u8>> {
    let mut raw_token = ptr::null_mut();
    // SAFETY: Win32 fills a valid HANDLE; OwnedHandle closes it on all paths.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut raw_token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = unsafe { OwnedHandle::from_raw_handle(raw_token) };
    sid_of_token(&token)
}

fn sid_of_token(token: &OwnedHandle) -> io::Result<Vec<u8>> {
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

// Query the security context attached to the last message read from this
// connection, not a process found later by its numeric PID. No application
// work executes while impersonating. OpenAsSelf allows TOKEN_QUERY even at
// identification level; the token handle is queried only after reverting.
fn check_client_message(handle: HANDLE, sid: &[u8]) -> io::Result<()> {
    if unsafe { ImpersonateNamedPipeClient(handle) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut raw_token = ptr::null_mut();
    let opened = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut raw_token) };
    let error = if opened == 0 {
        Some(io::Error::last_os_error())
    } else {
        None
    };
    // Continuing after failed reversion would run Fleet under a foreign
    // security context. Terminate instead, as required by the Win32 contract.
    if unsafe { RevertToSelf() } == 0 {
        std::process::abort();
    }
    if let Some(error) = error {
        return Err(error);
    }
    let token = unsafe { OwnedHandle::from_raw_handle(raw_token) };
    let peer = sid_of_token(&token)?;
    if unsafe {
        EqualSid(
            sid.as_ptr().cast_mut().cast::<c_void>(),
            peer.as_ptr().cast_mut().cast::<c_void>(),
        )
    } == 0
    {
        return Err(invalid("named-pipe message has a different user SID"));
    }
    Ok(())
}

fn check_pipe_owner(handle: HANDLE, sid: &[u8]) -> io::Result<()> {
    use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::OWNER_SECURITY_INFORMATION;

    let mut owner = ptr::null_mut();
    let mut descriptor = ptr::null_mut();
    let status = unsafe {
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let matches = !owner.is_null()
        && unsafe { EqualSid(owner, sid.as_ptr().cast_mut().cast::<c_void>()) } != 0;
    unsafe {
        LocalFree(descriptor);
    }
    if !matches {
        return Err(invalid("named-pipe object has a different owner SID"));
    }
    Ok(())
}

fn create_instance(name: &[u16], sid: &[u8], first: bool) -> io::Result<OwnedHandle> {
    let owner = sid_string(sid)?;
    // Protected DACL: ONLY this user may access the pipe. No Everyone,
    // Anonymous or broad Authenticated Users ACE is inherited.
    let sddl = wide(&format!("O:{owner}D:P(A;;GA;;;{owner})"));
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

// Keep directory handles without FILE_SHARE_DELETE for the lifetime of the
// listener. Identity is derived from the open directory, not path spelling.
pub struct RuntimeGuard {
    _ancestors: Vec<OwnedHandle>,
    _directory: OwnedHandle,
    identity: (u32, u32, u32),
}

fn open_runtime_directory(
    path: &Path,
    access: u32,
) -> io::Result<(
    OwnedHandle,
    windows_sys::Win32::Storage::FileSystem::BY_HANDLE_FILE_INFORMATION,
)> {
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_SHARE_WRITE,
    };
    let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let raw = unsafe {
        CreateFileW(
            name.as_ptr(),
            access,
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
            "Fleet runtime path must contain real directories, not reparse points",
        ));
    }
    Ok((handle, info))
}

// Reject pre-existing directories with unexpected ACLs rather than taking
// ownership or changing permissions on arbitrary paths.
pub fn private_runtime(path: &Path, create: bool) -> io::Result<RuntimeGuard> {
    private_directory(path, create, false)
}

fn private_directory(path: &Path, create: bool, inheritable: bool) -> io::Result<RuntimeGuard> {
    use std::path::Component;
    use windows_sys::Win32::Storage::FileSystem::{CreateDirectoryW, FILE_READ_ATTRIBUTES};
    if !path.is_absolute() || path.components().any(|part| part == Component::ParentDir) {
        return Err(invalid(
            "Fleet runtime must be an absolute path without parent traversal",
        ));
    }
    // Open ancestors from the root down, without following a reparse point at
    // each step. Retain the handles to prevent ancestor rename/delete races.
    let mut parents: Vec<_> = path.ancestors().skip(1).collect();
    parents.reverse();
    let mut ancestors = Vec::with_capacity(parents.len());
    for parent in parents {
        let (handle, _) = open_runtime_directory(parent, FILE_READ_ATTRIBUTES)?;
        ancestors.push(handle);
    }
    let sid = sid_string(&current_sid()?)?;
    let ace = if inheritable { "OICI" } else { "" };
    let expected = format!("O:{sid}D:P(A;{ace};FA;;;{sid})");
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
    // READ_CONTROL; hold the validated directory for the listener lifetime.
    let (handle, info) = open_runtime_directory(path, 0x0002_0000)?;
    let actual = owner_dacl_string(handle.as_raw_handle())?;
    // Auto-inheritance control flags can be set by the filesystem. In both
    // accepted forms the protected DACL contains exactly the owner ACE.
    if actual != expected && !(inheritable && actual == format!("O:{sid}D:PAI(A;OICI;FA;;;{sid})"))
    {
        return Err(invalid(
            "Fleet directory owner or DACL is not private; choose a new private directory",
        ));
    }
    Ok(RuntimeGuard {
        _ancestors: ancestors,
        _directory: handle,
        identity: (
            info.dwVolumeSerialNumber,
            info.nFileIndexHigh,
            info.nFileIndexLow,
        ),
    })
}

fn owner_dacl_string(raw: HANDLE) -> io::Result<String> {
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION};

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
    Ok(actual_string)
}

/// A protected directory for SQLite and its inherited sidecar files.
/// Unlike endpoint credentials, existing data is never removed or rewritten
/// to repair permissions. Retain the ancestor/directory handles while in use.
pub struct DataDirectory {
    _guard: RuntimeGuard,
    path: PathBuf,
    sid: String,
}

/// The current user's actual Windows Known Folder, not a process override.
pub fn local_app_data() -> io::Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath};

    let mut raw: *mut u16 = ptr::null_mut();
    let result =
        unsafe { SHGetKnownFolderPath(&FOLDERID_LocalAppData, 0, ptr::null_mut(), &mut raw) };
    // The documented allocator must be used even on failure.
    struct KnownFolder(*mut u16);
    impl Drop for KnownFolder {
        fn drop(&mut self) {
            unsafe { CoTaskMemFree(self.0.cast()) };
        }
    }
    let allocated = KnownFolder(raw);
    if result < 0 || allocated.0.is_null() {
        return Err(io::Error::other(format!(
            "LocalAppData Known Folder unavailable (HRESULT {result:#x})"
        )));
    }
    let mut len = 0;
    unsafe {
        while *allocated.0.add(len) != 0 {
            len += 1;
        }
    }
    let path = PathBuf::from(std::ffi::OsString::from_wide(unsafe {
        std::slice::from_raw_parts(allocated.0, len)
    }));
    validate_data_path(&path)?;
    if !path.is_dir() {
        return Err(invalid(
            "LocalAppData Known Folder is not an existing directory",
        ));
    }
    Ok(path)
}

#[cfg(test)]
#[test]
fn known_folder_is_an_existing_absolute_path() {
    let folder = local_app_data().unwrap();
    assert!(folder.is_absolute());
    assert!(folder.is_dir());
}

fn validate_data_path(path: &Path) -> io::Result<()> {
    use std::path::{Component, Prefix};

    // Limit this cut to local drive paths. UNC and device namespaces require
    // separate filesystem/locking validation.
    if !path.is_absolute()
        || !matches!(
            path.components().next(),
            Some(Component::Prefix(prefix))
                if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
        )
    {
        return Err(invalid(
            "Fleet data path must be an absolute local drive path",
        ));
    }
    for component in path.components() {
        match component {
            Component::ParentDir => {
                return Err(invalid("Fleet data path contains parent traversal"))
            }
            Component::Normal(name) => {
                let name = name.to_string_lossy();
                if name.contains(':') || name.ends_with(['.', ' ']) {
                    return Err(invalid("Fleet data path contains an ambiguous filename"));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Exclusive, nonblocking process reservation in a private data directory.
/// Keep the lock file after release; deleting it would split the lock identity.
pub struct DataLease {
    _file: std::fs::File,
    _directory: DataDirectory,
}

impl DataLease {
    pub fn acquire(directory: &Path, name: &str) -> io::Result<Self> {
        // The callers supply canonical, reserved filenames, not paths.
        if name.is_empty()
            || !name.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_.".contains(&byte)
            })
            || !name.ends_with(".lease")
        {
            return Err(invalid("invalid durable lease name"));
        }
        let guard = DataDirectory::open(directory)?;
        let path = directory.join(name);
        guard.validate_private_file(&path)?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        guard.validate_private_file(&path)?;
        file.try_lock().map_err(|error| {
            io::Error::other(format!("durable storage lease unavailable: {error}"))
        })?;
        Ok(Self {
            _file: file,
            _directory: guard,
        })
    }

    /// Persist the physical database identity in the already exclusively
    /// locked daemon lease. A new name, replacement file or missing database
    /// must not silently become the authority for existing runner spools.
    /// Retain the returned handle until daemon exit to prevent replacement.
    pub fn bind_database(&mut self, database: &Path) -> io::Result<File> {
        let adopt = std::env::var_os("FLEET_ADOPT_LEGACY_DB").as_deref() == Some(OsStr::new("1"));
        self.bind_database_with_adoption(database, adopt)
    }

    fn bind_database_with_adoption(&mut self, database: &Path, adopt: bool) -> io::Result<File> {
        use std::io::{Seek, SeekFrom};
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_SHARE_WRITE,
        };

        self._directory.validate_sqlite_files(database)?;
        self._file.seek(SeekFrom::Start(0))?;
        let mut marker = Vec::new();
        (&mut self._file).take(16_385).read_to_end(&mut marker)?;
        if marker.len() > 16_384 {
            return Err(invalid("invalid durable database identity marker"));
        }

        let exists = match std::fs::symlink_metadata(database) {
            Ok(_) => true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(error),
        };
        if marker.is_empty() {
            // A legacy root with runner evidence cannot be adopted by
            // guessing which database owns those records.
            for entry in std::fs::read_dir(&self._directory.path)? {
                let name = entry?.file_name().to_string_lossy().to_ascii_lowercase();
                if name.starts_with("fleet-runner-") {
                    return Err(invalid(
                        "unbound legacy runner state; explicit recovery required",
                    ));
                }
            }
            if exists && !adopt {
                return Err(invalid(
                    "unbound legacy database; explicit adoption after backup required",
                ));
            }
            if !exists {
                for suffix in ["-wal", "-shm", "-journal"] {
                    let mut name = database.as_os_str().to_os_string();
                    name.push(suffix);
                    match std::fs::symlink_metadata(Path::new(&name)) {
                        Ok(_) => {
                            return Err(invalid(
                                "orphan SQLite sidecar; explicit recovery required",
                            ))
                        }
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error),
                    }
                }
                let fresh = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(database)?;
                fresh.sync_all()?;
            }
        } else if !exists {
            return Err(invalid(
                "bound durable database is missing; explicit recovery required",
            ));
        }

        let database_handle = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(database)?;
        self._directory.validate_private_file(database)?;
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(database_handle.as_raw_handle(), &mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // Include the exact filename as UTF-16 (lossless), because SQLite
        // sidecars are name-scoped even if the same DB is renamed.
        let filename = database
            .file_name()
            .ok_or_else(|| invalid("missing database filename"))?;
        let mut identity = format!(
            "fleet-db-v1:{:08x}:{:08x}:{:08x}:",
            info.dwVolumeSerialNumber, info.nFileIndexHigh, info.nFileIndexLow
        );
        for unit in filename.encode_wide() {
            use std::fmt::Write as _;
            write!(&mut identity, "{unit:04x}").expect("writing to String cannot fail");
        }
        identity.push('\n');
        if marker.is_empty() {
            self._file.seek(SeekFrom::Start(0))?;
            self._file.write_all(identity.as_bytes())?;
            self._file.sync_all()?;
        } else if marker != identity.as_bytes() {
            return Err(invalid(
                "durable database identity mismatch; explicit recovery required",
            ));
        }
        Ok(database_handle)
    }
}

#[cfg(test)]
#[test]
fn durable_lease_rejects_alias_and_reopens_after_release() {
    let root = std::env::temp_dir().join(format!("gf-data-lease-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let data = root.join("data");
    let first = DataLease::acquire(&data, "fleetd.lease").unwrap();
    assert!(DataLease::acquire(&root.join("DATA"), "fleetd.lease").is_err());
    let other = DataLease::acquire(&data, "fleet-runner-one.spool.lease").unwrap();
    assert!(DataLease::acquire(&data, "fleet-runner-one.spool.lease").is_err());
    drop(other);
    drop(first);
    drop(DataLease::acquire(&data, "fleetd.lease").unwrap());
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(test)]
#[test]
fn legacy_database_adoption_is_explicit_and_cannot_override_identity() {
    let root = std::env::temp_dir().join(format!("gf-legacy-db-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let data = root.join("data");
    let db = data.join("fleet.sqlite");
    let mut lease = DataLease::acquire(&data, "fleetd.lease").unwrap();
    // This fixture checks the filesystem policy, not the SQLite file format.
    let original = b"legacy database evidence";
    std::fs::write(&db, original).unwrap();
    let denial = lease.bind_database_with_adoption(&db, false).unwrap_err();
    assert!(denial.to_string().contains("explicit adoption"));
    assert_eq!(std::fs::read(&db).unwrap(), original);

    let spool = data.join("fleet-runner-legacy.spool.sqlite");
    std::fs::write(&spool, b"unreconciled spool").unwrap();
    let denial = lease.bind_database_with_adoption(&db, true).unwrap_err();
    assert!(denial.to_string().contains("unbound legacy runner state"));
    assert_eq!(std::fs::read(&spool).unwrap(), b"unreconciled spool");
    // Delete only our synthetic evidence to exercise the permitted case.
    std::fs::remove_file(spool).unwrap();

    drop(lease.bind_database_with_adoption(&db, true).unwrap());
    drop(lease);
    assert!(!std::fs::read(data.join("fleetd.lease")).unwrap().is_empty());
    let mut lease = DataLease::acquire(&data, "fleetd.lease").unwrap();
    drop(lease.bind_database_with_adoption(&db, false).unwrap());

    let backup = data.join("fixture-original.sqlite");
    std::fs::rename(&db, &backup).unwrap();
    std::fs::write(&db, b"replacement database evidence").unwrap();
    let denial = lease.bind_database_with_adoption(&db, true).unwrap_err();
    assert!(denial.to_string().contains("identity mismatch"));
    assert_eq!(std::fs::read(&backup).unwrap(), original);
    assert_eq!(
        std::fs::read(&db).unwrap(),
        b"replacement database evidence"
    );
    drop(lease);
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(test)]
#[test]
fn orphan_sidecar_and_torn_identity_remain_intact_even_with_adoption() {
    let root = std::env::temp_dir().join(format!("gf-torn-db-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let data = root.join("data");
    let db = data.join("fleet.sqlite");
    let mut lease = DataLease::acquire(&data, "fleetd.lease").unwrap();
    let wal = data.join("fleet.sqlite-wal");
    std::fs::write(&wal, b"unreconciled WAL").unwrap();
    let denial = lease.bind_database_with_adoption(&db, true).unwrap_err();
    assert!(denial.to_string().contains("orphan SQLite sidecar"));
    assert!(!db.exists());
    assert_eq!(std::fs::read(&wal).unwrap(), b"unreconciled WAL");
    drop(lease);

    let marker = data.join("fleetd.lease");
    std::fs::write(&marker, b"fleet-db-v1:incomplete").unwrap();
    std::fs::write(&db, b"original database").unwrap();
    let mut lease = DataLease::acquire(&data, "fleetd.lease").unwrap();
    let denial = lease.bind_database_with_adoption(&db, true).unwrap_err();
    assert!(denial.to_string().contains("identity mismatch"));
    drop(lease);
    assert_eq!(std::fs::read(&marker).unwrap(), b"fleet-db-v1:incomplete");
    assert_eq!(std::fs::read(&wal).unwrap(), b"unreconciled WAL");
    assert_eq!(std::fs::read(&db).unwrap(), b"original database");
    std::fs::remove_dir_all(root).unwrap();
}

// SQLite can delete its WAL/SHM/rollback journal as the last connection
// closes. Windows may return ACCESS_DENIED or DELETE_PENDING while that
// deletion completes. Retry the full validation, never interpret these
// errors as absence. A persistent denial still fails closed.
fn retry_sqlite_sidecar_validation(mut validate: impl FnMut() -> io::Result<()>) -> io::Result<()> {
    use windows_sys::Win32::Foundation::{
        ERROR_ACCESS_DENIED, ERROR_DELETE_PENDING, ERROR_SHARING_VIOLATION,
    };

    const RETRIES: usize = 10;
    for attempt in 0..=RETRIES {
        match validate() {
            Err(error)
                if attempt < RETRIES
                    && error.raw_os_error().is_some_and(|code| {
                        code == ERROR_ACCESS_DENIED as i32
                            || code == ERROR_DELETE_PENDING as i32
                            || code == ERROR_SHARING_VIOLATION as i32
                    }) =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            result => return result,
        }
    }
    unreachable!("bounded sidecar validation always returns on its last attempt")
}

#[cfg(test)]
#[test]
fn sidecar_validation_retries_transient_os_denial_but_never_accepts_persistent_denial() {
    use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;
    let mut calls = 0;
    retry_sqlite_sidecar_validation(|| {
        calls += 1;
        if calls == 1 {
            Err(io::Error::from_raw_os_error(ERROR_ACCESS_DENIED as i32))
        } else {
            Ok(())
        }
    })
    .unwrap();
    assert_eq!(calls, 2);

    let mut calls = 0;
    let error = retry_sqlite_sidecar_validation(|| {
        calls += 1;
        Err(io::Error::from_raw_os_error(ERROR_ACCESS_DENIED as i32))
    })
    .unwrap_err();
    assert_eq!(calls, 11);
    assert_eq!(error.raw_os_error(), Some(ERROR_ACCESS_DENIED as i32));

    let mut calls = 0;
    let error = retry_sqlite_sidecar_validation(|| {
        calls += 1;
        Err(invalid("fixture invalid ACL"))
    })
    .unwrap_err();
    assert_eq!(calls, 1);
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
}

impl DataDirectory {
    pub fn open(path: &Path) -> io::Result<Self> {
        validate_data_path(path)?;
        let guard = private_directory(path, true, true)?;
        Ok(Self {
            _guard: guard,
            path: path.to_path_buf(),
            sid: sid_string(&current_sid()?)?,
        })
    }

    /// Validate existing database and sidecars before SQLite opens by name.
    /// A missing sidecar is normal; SQLite may create/remove it concurrently.
    /// The protected parent prevents other ordinary users from substituting
    /// entries while the guard is held. Same-user processes are trusted.
    pub fn validate_sqlite_files(&self, database: &Path) -> io::Result<()> {
        validate_data_path(database)?;
        if database.parent() != Some(self.path.as_path()) || database.file_name().is_none() {
            return Err(invalid(
                "SQLite path must be inside its guarded data directory",
            ));
        }
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let mut name = database.as_os_str().to_os_string();
            name.push(suffix);
            let path = Path::new(&name);
            let result = if suffix.is_empty() {
                self.validate_file(path)
            } else {
                retry_sqlite_sidecar_validation(|| self.validate_file(path))
            };
            result.map_err(|error| {
                let file = if suffix.is_empty() {
                    "database"
                } else {
                    suffix
                };
                io::Error::new(error.kind(), format!("SQLite {file} validation: {error}"))
            })?;
        }
        Ok(())
    }

    /// Validate a regular owner-only file in this guarded directory, if it exists.
    pub fn validate_private_file(&self, path: &Path) -> io::Result<()> {
        validate_data_path(path)?;
        if path.parent() != Some(self.path.as_path()) || path.file_name().is_none() {
            return Err(invalid("file must be inside its guarded data directory"));
        }
        self.validate_file(path)
    }

    fn validate_file(&self, path: &Path) -> io::Result<()> {
        use windows_sys::Win32::Storage::FileSystem::{
            GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY,
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
            FILE_SHARE_DELETE, FILE_SHARE_WRITE,
        };

        let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let raw = unsafe {
            CreateFileW(
                name.as_ptr(),
                0x0002_0000 | FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT,
                ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::NotFound {
                return Ok(());
            }
            return Err(error);
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(handle.as_raw_handle(), &mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if info.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0
            || info.nNumberOfLinks != 1
        {
            return Err(invalid("Fleet data file must be a regular, unlinked file"));
        }
        let actual = owner_dacl_string(handle.as_raw_handle())?;
        let sid = &self.sid;
        // Accept a single inherited full-control ACE, or the equivalent
        // explicit protected ACE on an existing owner-only file. No broad
        // grants, additional ACEs, NULL DACLs, or alternate owners.
        if ![
            format!("O:{sid}D:AI(A;ID;FA;;;{sid})"),
            format!("O:{sid}D:(A;ID;FA;;;{sid})"),
            format!("O:{sid}D:P(A;;FA;;;{sid})"),
            format!("O:{sid}D:PAI(A;;FA;;;{sid})"),
        ]
        .contains(&actual)
        {
            return Err(invalid("Fleet data file owner or DACL is not private"));
        }
        Ok(())
    }
}

pub struct Listener {
    _runtime_guard: Option<RuntimeGuard>,
    name: String,
    name_wide: Vec<u16>,
    sid: Vec<u8>,
    pending: Mutex<Option<OwnedHandle>>,
}

impl Listener {
    #[cfg(test)]
    fn bind(runtime: &Path, instance: &str) -> io::Result<Self> {
        Self::bind_key(runtime, instance)
    }

    pub fn bind_runtime(guard: RuntimeGuard, instance: &str) -> io::Result<Self> {
        let mut listener = Self::bind_key(guard.identity, instance)?;
        listener._runtime_guard = Some(guard);
        Ok(listener)
    }

    fn bind_key(key: impl Hash, instance: &str) -> io::Result<Self> {
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
        key.hash(&mut hash);
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

    pub fn address(&self) -> &str {
        &self.name
    }

    pub fn accept(&self) -> io::Result<Stream> {
        let mut pending = self.pending.lock().expect("Fleet pipe listener poisoned");
        let instance = pending
            .take()
            .ok_or_else(|| io::Error::other("listener closed"))?;
        let handle = instance.as_raw_handle();
        let result = overlapped_op(handle, None, None, |operation, _| unsafe {
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
        Ok(Stream::new(File::from(instance), Some(self.sid.clone())))
    }
}

struct Shared {
    // None after abortive shutdown. In-flight I/O retains its own Arc<File>
    // until CancelIoEx releases it; other clones cannot keep it open.
    file: Mutex<Option<Arc<File>>>,
    read_lock: Mutex<()>,
    write_lock: Mutex<()>,
    // Address of the pending read OVERLAPPED, valid only while the read
    // holds its direction lock. All access is serialized with submission.
    submission: Mutex<Option<usize>>,
    // Only accepted server streams verify incoming client security contexts.
    client_sid: Option<Vec<u8>>,
    read_eof: AtomicBool,
    write_eof: AtomicBool,
    stopped: AtomicBool,
    read_timeout: Mutex<Option<std::time::Duration>>,
    write_timeout: Mutex<Option<std::time::Duration>>,
}

pub struct Stream(Arc<Shared>);

impl Stream {
    fn new(file: File, client_sid: Option<Vec<u8>>) -> Self {
        Self(Arc::new(Shared {
            file: Mutex::new(Some(Arc::new(file))),
            read_lock: Mutex::new(()),
            write_lock: Mutex::new(()),
            submission: Mutex::new(None),
            client_sid,
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
                    let result = pipe_write(&file, &[0], timeout, &self.0);
                    match result {
                        Ok(1) => {}
                        Ok(_) => {
                            self.shutdown(std::net::Shutdown::Both)?;
                            return Err(io::Error::new(
                                io::ErrorKind::WriteZero,
                                "pipe EOF not sent",
                            ));
                        }
                        Err(error) => {
                            self.shutdown(std::net::Shutdown::Both)?;
                            return Err(error);
                        }
                    }
                }
            }
            std::net::Shutdown::Read => {
                let gate = self.0.submission.lock().expect("pipe submission poisoned");
                self.0.read_eof.store(true, Ordering::Release);
                if let (Some(operation), Ok(file)) = (*gate, self.file()) {
                    unsafe {
                        CancelIoEx(file.as_raw_handle(), operation as *const OVERLAPPED);
                    }
                }
            }
            std::net::Shutdown::Both => {
                // Abortive cancellation must never wait for peer input or
                // write-buffer capacity. Graceful shutdown(Write) above is
                // the only path that sends the in-band EOF marker.
                let _gate = self.0.submission.lock().expect("pipe submission poisoned");
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
        match pipe_read(&file, out, timeout, &self.0) {
            Ok(n) => {
                if let (true, Some(sid)) = (n != 0, &self.0.client_sid) {
                    if let Err(error) = check_client_message(file.as_raw_handle(), sid) {
                        // Never expose unauthenticated bytes to the decoder.
                        let _ = self.shutdown(std::net::Shutdown::Both);
                        return Err(error);
                    }
                }
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
        let result = pipe_write(&file, bytes, timeout, &self.0);
        if result.is_err() {
            // An aborted or failed write can leave an uncertain JSONL prefix
            // at the peer. Do not permit subsequent frames on this stream.
            self.shutdown(std::net::Shutdown::Both)?;
        }
        result
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
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                ptr::null_mut(),
            )
        };
        if handle != INVALID_HANDLE_VALUE {
            let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
            // Object ownership is independent of the PID lookup and remains
            // an additional fail-closed check even if a PID has been reused.
            check_pipe_owner(handle.as_raw_handle(), &sid)?;
            check_peer(handle.as_raw_handle(), &sid, false)?;
            return Ok(Stream::new(File::from(handle), None));
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
    fn anonymous_client_bytes_are_not_delivered_to_server() {
        use std::sync::mpsc;
        use std::time::Duration;

        let runtime =
            std::env::temp_dir().join(format!("gentle-fleet-anonymous-{}", uuid::Uuid::new_v4()));
        let listener = Listener::bind(&runtime, "anonymous").unwrap();
        let address = wide(listener.address());
        let (ready_tx, ready_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut stream = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            ready_tx.send(()).unwrap();
            let mut byte = [0u8; 1];
            assert!(stream.read(&mut byte).is_err());
        });
        // SECURITY_SQOS_PRESENT with no impersonation level bits explicitly
        // requests SecurityAnonymous. It can connect under the owner's DACL
        // but the server must not consume its unauthenticated message.
        let handle = unsafe {
            CreateFileW(
                address.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT,
                ptr::null_mut(),
            )
        };
        assert_ne!(handle, INVALID_HANDLE_VALUE);
        let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
        let mut client = Stream::new(File::from(handle), None);
        client
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        client.write_all(b"x").unwrap();
        server.join().unwrap();
    }

    #[test]
    fn timeout_racing_success_reports_transferred_bytes() {
        let timeout = io::Error::new(io::ErrorKind::TimedOut, "timeout");
        assert_eq!(timeout_completion(timeout, Ok(37)).unwrap(), 37);
        let timeout = io::Error::new(io::ErrorKind::TimedOut, "timeout");
        let aborted = io::Error::from_raw_os_error(
            windows_sys::Win32::Foundation::ERROR_OPERATION_ABORTED as i32,
        );
        assert_eq!(
            timeout_completion(timeout, Err(aborted))
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        let timeout = io::Error::new(io::ErrorKind::TimedOut, "timeout");
        let other = io::Error::from_raw_os_error(ERROR_BROKEN_PIPE as i32);
        assert_eq!(
            timeout_completion(timeout, Err(other))
                .unwrap_err()
                .raw_os_error(),
            Some(ERROR_BROKEN_PIPE as i32)
        );
    }

    #[test]
    fn failed_write_under_backpressure_closes_stream() {
        use std::sync::mpsc;
        use std::time::Duration;

        let runtime = std::env::temp_dir().join(format!(
            "gentle-fleet-write-timeout-{}",
            uuid::Uuid::new_v4()
        ));
        let listener = Listener::bind(&runtime, "write-timeout").unwrap();
        let address = listener.address().to_owned();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let _stream = listener.accept().unwrap();
            ready_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(10));
        });
        let mut client = connect(&address).unwrap();
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        client
            .set_write_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let mut clone = client.try_clone().unwrap();
        let result = client.write_all(&vec![b'x'; 8 * 1024 * 1024]);
        let next = clone.write(b"next");
        let _ = release_tx.send(());
        server.join().unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert_eq!(next.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn full_shutdown_cancels_pending_write_under_backpressure() {
        use std::sync::mpsc;
        use std::time::Duration;

        let runtime =
            std::env::temp_dir().join(format!("gentle-fleet-pressure-{}", uuid::Uuid::new_v4()));
        let listener = Listener::bind(&runtime, "pressure").unwrap();
        let address = listener.address().to_owned();
        let (accepted_tx, accepted_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let _stream = listener.accept().unwrap();
            accepted_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(15));
        });
        let client = connect(&address).unwrap();
        accepted_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let file = client.file().unwrap();
        let (pending_tx, pending_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let shared = Arc::clone(&client.0);
        let writer = std::thread::spawn(move || {
            let bytes = vec![b'x'; 8 * 1024 * 1024];
            let handle = file.as_raw_handle();
            let result = overlapped_op(handle, None, Some((&shared, false)), |operation, count| {
                let result = unsafe {
                    WriteFile(handle, bytes.as_ptr(), bytes.len() as u32, count, operation)
                };
                let error = unsafe { windows_sys::Win32::Foundation::GetLastError() };
                let pending = result == 0 && error == ERROR_IO_PENDING;
                let _ = pending_tx.send(pending);
                unsafe { windows_sys::Win32::Foundation::SetLastError(error) };
                result
            });
            let _ = done_tx.send(result);
        });
        let pending = pending_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        client.shutdown(Shutdown::Both).unwrap();
        let result = done_rx.recv_timeout(Duration::from_secs(3));
        let _ = release_tx.send(());
        writer.join().unwrap();
        server.join().unwrap();
        assert!(pending, "fixture must establish a pending overlapped write");
        assert!(
            result
                .expect("shutdown failed to release the pending write")
                .is_err(),
            "cancelled write must not report success"
        );
    }

    #[test]
    fn stale_handle_cannot_submit_io_after_shutdown() {
        use std::sync::mpsc;
        use std::time::Duration;

        let runtime =
            std::env::temp_dir().join(format!("gentle-fleet-submit-test-{}", uuid::Uuid::new_v4()));
        let listener = Listener::bind(&runtime, "submission").unwrap();
        let address = listener.address().to_owned();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let _stream = listener.accept().unwrap();
            ready_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(10));
        });
        let client = connect(&address).unwrap();
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        // Exactly the problematic ordering: a clone obtains the handle,
        // cancellation happens, and only then does the operation submit.
        let file = client.file().unwrap();
        client.shutdown(Shutdown::Both).unwrap();
        let mut byte = [0u8; 1];
        let read = pipe_read(
            &file,
            &mut byte,
            Some(Duration::from_millis(200)),
            &client.0,
        );
        let write = pipe_write(&file, b"x", Some(Duration::from_millis(200)), &client.0);
        drop(file);
        release_tx.send(()).unwrap();
        server.join().unwrap();
        assert_eq!(read.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(write.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn read_shutdown_does_not_cancel_pending_write() {
        use std::sync::mpsc;
        use std::time::Duration;

        let runtime =
            std::env::temp_dir().join(format!("gentle-fleet-read-close-{}", uuid::Uuid::new_v4()));
        let listener = Listener::bind(&runtime, "read-close").unwrap();
        let address = listener.address().to_owned();
        let (accepted_tx, accepted_rx) = mpsc::channel();
        let (drain_tx, drain_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut stream = listener.accept().unwrap();
            accepted_tx.send(()).unwrap();
            drain_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = vec![0u8; 8 * 1024 * 1024];
            stream.read_exact(&mut bytes).unwrap();
            assert!(bytes.iter().all(|byte| *byte == b'x'));
        });
        let client = connect(&address).unwrap();
        accepted_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let file = client.file().unwrap();
        let shared = Arc::clone(&client.0);
        let (pending_tx, pending_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            let bytes = vec![b'x'; 8 * 1024 * 1024];
            let handle = file.as_raw_handle();
            let result = overlapped_op(
                handle,
                Some(Duration::from_secs(8)),
                Some((&shared, false)),
                |operation, count| {
                    let result = unsafe {
                        WriteFile(handle, bytes.as_ptr(), bytes.len() as u32, count, operation)
                    };
                    let error = unsafe { windows_sys::Win32::Foundation::GetLastError() };
                    let _ = pending_tx.send(result == 0 && error == ERROR_IO_PENDING);
                    unsafe { windows_sys::Win32::Foundation::SetLastError(error) };
                    result
                },
            );
            let _ = done_tx.send(result);
        });
        assert!(pending_rx.recv_timeout(Duration::from_secs(5)).unwrap());
        client.shutdown(Shutdown::Read).unwrap();
        drain_tx.send(()).unwrap();
        let result = done_rx.recv_timeout(Duration::from_secs(8));
        writer.join().unwrap();
        server.join().unwrap();
        assert_eq!(result.unwrap().unwrap(), 8 * 1024 * 1024);
    }

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

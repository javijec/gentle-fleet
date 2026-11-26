//! Shared provider launch policy. Windows never invokes a batch shim.
//! Linux keeps the existing PATH semantics. Explicit Windows JS entrypoints
//! execute with Node directly, not through cmd.exe or PowerShell.

use crate::AgentError;
#[cfg(windows)]
use std::ffi::OsStr;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

mod owned;
pub use owned::{spawn_owned, OwnedChild};

const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_VERSION_BYTES: usize = 64 * 1024;

fn unavailable(message: &str) -> AgentError {
    AgentError {
        code: "provider_unavailable".into(),
        message: message.into(),
    }
}

#[cfg(windows)]
pub fn resolve_native_exe(
    name: &str,
    override_path: Option<&OsStr>,
    path: Option<&OsStr>,
) -> Result<std::ffi::OsString, AgentError> {
    fn is_native(path: &std::path::Path) -> bool {
        path.is_absolute()
            && path
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
            && path.is_file()
    }

    if let Some(value) = override_path {
        let candidate = std::path::Path::new(value);
        return if is_native(candidate) {
            Ok(candidate.as_os_str().to_owned())
        } else {
            Err(unavailable(
                "provider override must be an existing absolute .exe",
            ))
        };
    }
    if let Some(path) = path {
        for directory in std::env::split_paths(path) {
            if !directory.is_absolute() {
                continue;
            }
            let candidate = directory.join(format!("{name}.exe"));
            if is_native(&candidate) {
                return Ok(candidate.into_os_string());
            }
        }
    }
    Err(unavailable(
        "native provider .exe not found in absolute PATH entries",
    ))
}

/// Construct a command without interpolating any argument into a shell.
/// A configured JS entrypoint is an explicit operator trust decision.
pub fn provider_command(
    name: &str,
    exe_env: &str,
    js_env: Option<&str>,
) -> Result<Command, AgentError> {
    #[cfg(windows)]
    {
        let native = std::env::var_os(exe_env);
        let path = std::env::var_os("PATH");
        if let Some(script) = js_env.and_then(std::env::var_os) {
            if native.is_some() {
                return Err(unavailable("configure either provider EXE or JS, not both"));
            }
            let entry = PathBuf::from(script);
            if !entry.is_absolute()
                || !entry.is_file()
                || !entry
                    .extension()
                    .and_then(OsStr::to_str)
                    .is_some_and(|ext| {
                        ["js", "mjs", "cjs"]
                            .iter()
                            .any(|allowed| ext.eq_ignore_ascii_case(allowed))
                    })
            {
                return Err(unavailable(
                    "provider JS must be an existing absolute .js/.mjs/.cjs",
                ));
            }
            let node = resolve_native_exe(
                "node",
                std::env::var_os("FLEET_NODE_BIN").as_deref(),
                path.as_deref(),
            )?;
            let mut command = Command::new(node);
            command.arg(entry);
            return Ok(command);
        }
        Ok(Command::new(resolve_native_exe(
            name,
            native.as_deref(),
            path.as_deref(),
        )?))
    }
    #[cfg(not(windows))]
    {
        let _ = (exe_env, js_env);
        Ok(Command::new(name))
    }
}

/// Do not fall back to a shared temporary directory for user state.
pub fn user_home() -> Option<PathBuf> {
    #[cfg(windows)]
    let key = "USERPROFILE";
    #[cfg(not(windows))]
    let key = "HOME";
    std::env::var_os(key)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

/// Bounded stdout capture, null stdin/stderr, successful exit required.
/// Windows contains the process tree in an owned job and requires it empty.
/// Unix retains direct-child supervision. The probe deadline is independent
/// of EOF from stdout.
pub fn version_stdout(command: &Command) -> Result<Vec<u8>, AgentError> {
    // provider_command builds only a program and fixed prefix arguments.
    // Preserve the exact executable and entrypoint used for the later RPC.
    let mut probe = Command::new(command.get_program());
    probe.args(command.get_args()).arg("--version");
    capture_stdout(probe, VERSION_TIMEOUT)
}

fn capture_stdout(mut command: Command, timeout: Duration) -> Result<Vec<u8>, AgentError> {
    let mut child = spawn_owned(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .map_err(|_| unavailable("cannot start contained provider version probe"))?;
    let deadline = Instant::now() + timeout;
    let stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = mpsc::sync_channel(1);
    let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader_cancelled = cancelled.clone();
    let reader = std::thread::Builder::new()
        .spawn(move || {
            let mut bytes = Vec::new();
            let mut stdout = stdout.take((MAX_VERSION_BYTES + 1) as u64);
            let result = (|| {
                let mut chunk = [0u8; 4096];
                loop {
                    if reader_cancelled.load(std::sync::atomic::Ordering::Acquire) {
                        return Err(std::io::Error::other("version reader cancelled"));
                    }
                    let count = stdout.read(&mut chunk)?;
                    if count == 0 {
                        return Ok(bytes);
                    }
                    bytes.extend_from_slice(&chunk[..count]);
                }
            })();
            let _ = tx.send(result);
        })
        .map_err(|_| unavailable("cannot start provider version reader"))?;
    let result = (|| {
        let mut output = None;
        let mut exited = false;
        loop {
            if Instant::now() >= deadline {
                return Err(unavailable("provider version probe timed out"));
            }
            if !exited {
                if let Some(status) = child
                    .try_wait()
                    .map_err(|_| unavailable("provider version wait failed"))?
                {
                    if !status.success() {
                        return Err(unavailable("provider version probe exited unsuccessfully"));
                    }
                    child
                        .ensure_tree_empty()
                        .map_err(|_| unavailable("provider version probe left active descendants"))?;
                    exited = true;
                }
            }
            if output.is_none() {
                match rx.try_recv() {
                    Ok(Ok(bytes)) if bytes.len() <= MAX_VERSION_BYTES => output = Some(bytes),
                    Ok(_) => {
                        return Err(unavailable("provider version output invalid or too large"))
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        return Err(unavailable("provider version reader disconnected"));
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                }
            }
            if exited {
                if let Some(bytes) = output.take() {
                    return Ok(bytes);
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    if result.is_err() {
        cancelled.store(true, std::sync::atomic::Ordering::Release);
        let cleanup = child.kill().and_then(|_| child.wait());
        #[cfg(windows)]
        {
            // A leaked pipe handle outside the private job must not trap this
            // reader. Cancel only the thread created by this invocation.
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::System::IO::CancelSynchronousIo;
            let deadline = Instant::now() + Duration::from_secs(2);
            while !reader.is_finished() && Instant::now() < deadline {
                // Repeated cancellation covers the interval before ReadFile.
                unsafe { CancelSynchronousIo(reader.as_raw_handle()) };
                std::thread::sleep(Duration::from_millis(10));
            }
            if !reader.is_finished() {
                return Err(unavailable("provider version reader cancellation unconfirmed"));
            }
            let _ = reader.join();
        }
        if cleanup.is_err() {
            return Err(unavailable("provider version process cleanup unconfirmed"));
        }
    } else {
        let _ = reader.join();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::zombie_processes)] // tree fixture deliberately leaves a descendant to the job
    fn version_fixture() {
        match std::env::var("FLEET_TEST_VERSION_FIXTURE").as_deref() {
            Ok("ok") => println!("fixture 1.2.3"),
            Ok("fail") => std::process::exit(9),
            Ok("large") => print!("{}", "x".repeat(MAX_VERSION_BYTES + 1)),
            Ok("hang") => std::thread::sleep(Duration::from_secs(20)),
            #[cfg(windows)]
            Ok("tree") | Ok("tree_hang") => {
                let descendant = fixture("hang").spawn().unwrap();
                drop(descendant);
                println!("spawned descendant");
                if std::env::var("FLEET_TEST_VERSION_FIXTURE").as_deref() == Ok("tree_hang") {
                    std::thread::sleep(Duration::from_secs(20));
                }
            }
            #[cfg(windows)]
            Ok("job_owner") => {
                let child = spawn_owned(
                    fixture("hang")
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null()),
                )
                .unwrap();
                println!("job child pid={}", child.root_id());
                std::thread::sleep(Duration::from_secs(20));
                drop(child);
            }
            _ => {}
        }
    }

    fn fixture(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "process::tests::version_fixture", "--nocapture"])
            .env("FLEET_TEST_VERSION_FIXTURE", mode);
        command
    }

    #[test]
    fn probe_is_bounded_and_requires_success() {
        let output = capture_stdout(fixture("ok"), VERSION_TIMEOUT).unwrap();
        assert!(String::from_utf8_lossy(&output).contains("fixture 1.2.3"));
        assert!(capture_stdout(fixture("fail"), VERSION_TIMEOUT).is_err());
        assert!(capture_stdout(fixture("large"), VERSION_TIMEOUT).is_err());
        let start = Instant::now();
        assert!(capture_stdout(fixture("hang"), Duration::from_millis(250)).is_err());
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[cfg(windows)]
    #[test]
    fn job_reaps_descendants_after_root_exit_without_touching_sentinel() {
        let mut sentinel = spawn_owned(
            fixture("hang")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .unwrap();
        let mut root = spawn_owned(
            fixture("tree")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = root.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(Instant::now() < deadline, "root did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
        // The root already exited, but its descendant holds stdout open.
        assert!(root.active_processes().unwrap() > 0);
        let error = root.ensure_tree_empty().unwrap_err();
        assert_eq!(error.to_string(), "provider exited with active descendants");
        assert_eq!(root.active_processes().unwrap(), 0);
        let mut stdout = root.stdout.take().unwrap();
        let (tx, rx) = mpsc::sync_channel(1);
        let reader = std::thread::spawn(move || {
            let mut data = String::new();
            let result = stdout.read_to_string(&mut data).map(|_| data);
            let _ = tx.send(result);
        });
        assert!(rx
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .unwrap()
            .contains("spawned descendant"));
        reader.join().unwrap();
        assert!(sentinel.try_wait().unwrap().is_none());
        assert_eq!(sentinel.active_processes().unwrap(), 1);
        sentinel.kill().unwrap();
        sentinel.wait().unwrap();

        let error = capture_stdout(fixture("tree"), VERSION_TIMEOUT).unwrap_err();
        assert_eq!(error.message, "provider version probe left active descendants");
    }

    #[cfg(windows)]
    #[test]
    fn job_terminates_blocked_root_and_descendant() {
        let mut root = spawn_owned(
            fixture("tree_hang")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
        )
        .unwrap();
        let deadline = Instant::now() + VERSION_TIMEOUT;
        while root.active_processes().unwrap() < 2 {
            assert!(Instant::now() < deadline, "descendant was not started");
            assert!(root.try_wait().unwrap().is_none());
            std::thread::sleep(Duration::from_millis(10));
        }
        root.kill().unwrap();
        root.wait().unwrap();
        assert_eq!(root.active_processes().unwrap(), 0);
        let error = capture_stdout(fixture("tree_hang"), Duration::from_secs(2)).unwrap_err();
        assert_eq!(error.message, "provider version probe timed out");
    }

    #[cfg(windows)]
    #[test]
    fn job_kill_on_owner_death() {
        use std::io::BufRead;
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
        };

        // The owner itself is deliberately NOT placed in a private job by
        // this test: killing it must close its child's job handle, not kill
        // the child through some enclosing test job.
        let mut owner = fixture("job_owner")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = owner.stdout.take().unwrap();
        let (tx, rx) = mpsc::sync_channel(1);
        let reader = std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(pid) = line
                    .split("job child pid=")
                    .nth(1)
                    .and_then(|text| text.trim().parse::<u32>().ok())
                {
                    let _ = tx.send(pid);
                    break;
                }
            }
        });
        let outcome = (|| -> std::io::Result<OwnedHandle> {
            let pid = rx
                .recv_timeout(VERSION_TIMEOUT)
                .map_err(std::io::Error::other)?;
            // PID is used only to obtain an observation handle for a freshly
            // created fixture child while its owner is alive.
            let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
            if handle.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            let child = unsafe { OwnedHandle::from_raw_handle(handle) };
            if unsafe { WaitForSingleObject(child.as_raw_handle(), 0) } != WAIT_TIMEOUT {
                return Err(std::io::Error::other("fixture child not alive before owner death"));
            }
            Ok(child)
        })();
        owner.kill().unwrap();
        owner.wait().unwrap();
        reader.join().unwrap();
        let child = outcome.unwrap();
        assert_eq!(
            unsafe { WaitForSingleObject(child.as_raw_handle(), 5000) },
            WAIT_OBJECT_0,
            "private job did not kill child when its owner died"
        );
    }

    #[cfg(windows)]
    #[test]
    fn native_resolution_ignores_shims_and_relative_paths() {
        let dir = std::env::temp_dir().join(format!("fleet proveedor ñ & {}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let exe = dir.join("fixture.exe");
        let shim = dir.join("fixture.cmd");
        std::fs::copy(std::env::current_exe().unwrap(), &exe).unwrap();
        std::fs::write(&shim, "@exit /b 1").unwrap();
        let path = std::env::join_paths([std::path::Path::new("."), &dir]).unwrap();
        assert_eq!(
            resolve_native_exe("fixture", None, Some(&path)).unwrap(),
            exe.as_os_str()
        );
        assert!(resolve_native_exe("fixture", Some(shim.as_os_str()), Some(&path)).is_err());
        assert!(resolve_native_exe("missing", None, Some(&path)).is_err());
        assert!(resolve_native_exe("fixture", Some(OsStr::new("fixture.exe")), None).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

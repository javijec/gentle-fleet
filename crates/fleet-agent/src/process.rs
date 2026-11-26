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
            Err(unavailable("provider override must be an existing absolute .exe"))
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
    Err(unavailable("native provider .exe not found in absolute PATH entries"))
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
                return Err(unavailable("provider JS must be an existing absolute .js/.mjs/.cjs"));
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
/// On failure terminate/reap only our direct child. A descendant inheriting
/// stdout can delay EOF; the caller still returns at the deadline, but tree
/// containment remains a separate native-process validation requirement.
pub fn version_stdout(command: &Command) -> Result<Vec<u8>, AgentError> {
    // provider_command builds only a program and fixed prefix arguments.
    // Preserve the exact executable and entrypoint used for the later RPC.
    let mut probe = Command::new(command.get_program());
    probe.args(command.get_args()).arg("--version");
    capture_stdout(probe, VERSION_TIMEOUT)
}

fn capture_stdout(mut command: Command, timeout: Duration) -> Result<Vec<u8>, AgentError> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| unavailable("cannot start provider version probe"))?;
    let deadline = Instant::now() + timeout;
    let stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = mpsc::sync_channel(1);
    let reader = std::thread::Builder::new().spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout
            .take((MAX_VERSION_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = tx.send(result);
    });
    if reader.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return Err(unavailable("cannot start provider version reader"));
    }
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
                    exited = true;
                }
            }
            if output.is_none() {
                match rx.try_recv() {
                    Ok(Ok(bytes)) if bytes.len() <= MAX_VERSION_BYTES => output = Some(bytes),
                    Ok(_) => return Err(unavailable("provider version output invalid or too large")),
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
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_fixture() {
        match std::env::var("FLEET_TEST_VERSION_FIXTURE").as_deref() {
            Ok("ok") => println!("fixture 1.2.3"),
            Ok("fail") => std::process::exit(9),
            Ok("large") => print!("{}", "x".repeat(MAX_VERSION_BYTES + 1)),
            Ok("hang") => std::thread::sleep(Duration::from_secs(20)),
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

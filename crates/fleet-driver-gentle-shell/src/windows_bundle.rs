//! Resolve the vendor's generated Windows bundle without executing its CMD.
//! Treat the launcher as data: extract only its fixed runtime directory.
//! Explicit operator EXE/JS overrides still use the shared launcher policy.

use fleet_agent::AgentError;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

const ENTRY: &str = "node_modules/gentle-pi/bin/gentle-shell.mjs";
const PREFIX: &str = r#""%~dp0..\runtime\node-"#;
const SUFFIX: &str = r#"\node.exe" "%~dp0..\versions\%GENTLE_SHELL_CURRENT%\node_modules\gentle-pi\bin\gentle-shell.mjs" %*"#;
const INVALID: &str = "Gentle Shell bundle metadata invalid";
const MISSING: &str = "Gentle Shell bundle entrypoint or runtime missing";
const ESCAPE: &str = "Gentle Shell bundle path escaped root";

fn error(message: &str) -> AgentError {
    AgentError {
        code: "provider_unavailable".into(),
        message: message.into(),
    }
}

fn bounded_text(path: &Path, limit: u64) -> Result<String, AgentError> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(limit + 1).read_to_end(&mut bytes))
        .map_err(|_| error(INVALID))?;
    if bytes.len() as u64 > limit {
        return Err(error(INVALID));
    }
    String::from_utf8(bytes).map_err(|_| error(INVALID))
}

fn active_version(text: &str) -> Result<&str, AgentError> {
    let value = text.trim_end_matches(['\r', '\n']);
    if value.len() <= 128
        && value.as_bytes().first().is_some_and(u8::is_ascii_digit)
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
        && !value.contains("..")
        && !value.ends_with('.')
    {
        Ok(value)
    } else {
        Err(error(INVALID))
    }
}

fn node_version(text: &str) -> Result<&str, AgentError> {
    let mut matches = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix(PREFIX)?.strip_suffix(SUFFIX));
    let version = matches.next().ok_or_else(|| error(INVALID))?;
    if matches.next().is_some()
        || version.len() > 32
        || version.split('.').count() != 3
        || !version
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(error(INVALID));
    }
    Ok(version)
}

fn contained_file(root: &Path, path: &Path) -> Result<PathBuf, AgentError> {
    let resolved = path.canonicalize().map_err(|_| error(MISSING))?;
    if !resolved.starts_with(root) {
        return Err(error(ESCAPE));
    }
    if !resolved.is_file() {
        return Err(error(MISSING));
    }
    Ok(resolved)
}

fn resolve(root: &Path) -> Result<Command, AgentError> {
    if !root.is_absolute() {
        return Err(error(INVALID));
    }
    let root = root.canonicalize().map_err(|_| error(MISSING))?;
    let current = contained_file(&root, &root.join("current"))?;
    let launcher = contained_file(&root, &root.join("bin/gentle-shell.cmd"))?;
    let current_text = bounded_text(&current, 256)?;
    let version = active_version(&current_text)?;
    let launcher_text = bounded_text(&launcher, 16 * 1024)?;
    let node = node_version(&launcher_text)?;
    let entry = contained_file(&root, &root.join("versions").join(version).join(ENTRY))?;
    let executable = contained_file(
        &root,
        &root
            .join("runtime")
            .join(format!("node-{node}"))
            .join("node.exe"),
    )?;
    let mut command = Command::new(executable);
    command.arg(entry);
    Ok(command)
}

pub(super) fn discover() -> Result<Option<Command>, AgentError> {
    if std::env::var_os("FLEET_GENTLE_SHELL_BIN").is_some()
        || std::env::var_os("FLEET_GENTLE_SHELL_JS").is_some()
    {
        return Ok(None);
    }
    let mut roots = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        if local.is_absolute() {
            roots.push(local.join("gentle-shell"));
        }
    }
    if let Some(home) = fleet_agent::process::user_home() {
        roots.push(home.join(".gentle-shell-bundle"));
    }
    // Prefer the known bundle selected by PATH when both layouts exist.
    if let Some(path) = std::env::var_os("PATH") {
        for bin in std::env::split_paths(&path) {
            if !bin.is_absolute() {
                continue;
            }
            if let Some(root) = roots.iter().find(|root| {
                let candidate = root.join("bin");
                candidate == bin
                    || candidate
                        .canonicalize()
                        .ok()
                        .zip(bin.canonicalize().ok())
                        .is_some_and(|(a, b)| a == b)
            }) {
                if root.join("bin/gentle-shell.cmd").is_file() {
                    return resolve(root).map(Some);
                }
            }
        }
    }
    for root in roots {
        if root.join("bin/gentle-shell.cmd").is_file() || root.join("current").exists() {
            return resolve(&root).map(Some);
        }
    }
    // Legacy npm layout. Resolve Node independently of the bundled runtime.
    if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) {
        let entry = appdata.join("npm").join(ENTRY);
        if entry.is_absolute() && entry.is_file() {
            let node = fleet_agent::process::resolve_native_exe(
                "node",
                std::env::var_os("FLEET_NODE_BIN").as_deref(),
                std::env::var_os("PATH").as_deref(),
            )?;
            let mut command = Command::new(node);
            command.arg(entry);
            return Ok(Some(command));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_generated_launcher_without_shell_evaluation() {
        let line = format!("{PREFIX}24.21.0{SUFFIX}");
        let text = format!("@echo off\r\n{line}\r\nexit /b %errorlevel%\r\n");
        assert_eq!(node_version(&text).unwrap(), "24.21.0");
        assert_eq!(active_version("5.0.0-1.6.1\r\n").unwrap(), "5.0.0-1.6.1");
        for value in [
            "../5.0.0",
            "5.0.0/evil",
            "5.0.0:stream",
            "5.0.0%PATH%",
            "5.0.0\r\n6.0.0",
            "5.0.0.",
            "",
            "5.0.0 ",
        ] {
            assert!(active_version(value).is_err());
        }
        assert!(node_version(&format!("{PREFIX}24.21.0 & evil{SUFFIX}")).is_err());
        assert!(node_version(&format!("{line}\n{line}")).is_err());
    }

    #[test]
    fn resolves_active_version_and_matching_runtime_only() {
        let root = std::env::temp_dir().join(
            format!(
                "fleet-gs-bundle-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            )
            .replace(['(', ')'], ""),
        );
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::create_dir_all(root.join("runtime/node-24.21.0")).unwrap();
        std::fs::write(root.join("runtime/node-24.21.0/node.exe"), b"fixture").unwrap();
        std::fs::write(
            root.join("bin/gentle-shell.cmd"),
            format!("{PREFIX}24.21.0{SUFFIX}\r\n"),
        )
        .unwrap();
        for version in ["4.8.0-1.6.0", "5.0.0-1.6.1"] {
            let entry = root.join("versions").join(version).join(ENTRY);
            std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
            std::fs::write(&entry, b"fixture").unwrap();
            std::fs::write(root.join("current"), version).unwrap();
            let command = resolve(&root).unwrap();
            assert_eq!(
                Path::new(command.get_program()),
                root.join("runtime/node-24.21.0/node.exe")
                    .canonicalize()
                    .unwrap()
            );
            assert_eq!(
                Path::new(command.get_args().next().unwrap()),
                entry.canonicalize().unwrap()
            );
        }
        std::fs::write(root.join("current"), "../outside").unwrap();
        assert!(resolve(&root).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}

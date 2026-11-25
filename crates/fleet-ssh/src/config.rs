//! Durable host catalog. The sibling lock is stable across atomic replacement.
//! The local user remains the trust boundary; no SSH authentication is changed.

use super::{HostConfig, SshError};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

fn error(message: &'static str) -> SshError {
    SshError::Protocol(message.into())
}

fn io_error(err: io::Error) -> SshError {
    // Do not echo catalog contents or private paths in diagnostics.
    SshError::Protocol(format!("SSH catalog filesystem error: {:?}", err.kind()))
}

pub(super) fn path() -> Result<PathBuf, SshError> {
    if let Some(value) = std::env::var_os("FLEET_SSH_CONFIG") {
        let path = PathBuf::from(value);
        validate_path(&path)?;
        return Ok(path);
    }
    #[cfg(windows)]
    let root = Some(fleet_protocol::transport::local_app_data().map_err(io_error)?);
    #[cfg(unix)]
    let root = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    let root = root
        .filter(|root| root.is_absolute())
        .ok_or_else(|| error("no absolute durable SSH configuration root"))?;
    Ok(root.join("gentle-fleet").join("hosts.json"))
}

fn validate_path(path: &Path) -> Result<(), SshError> {
    if !path.is_absolute()
        || path.file_name().is_none()
        || path.components().any(|part| part == Component::ParentDir)
    {
        return Err(error(
            "SSH catalog requires an absolute path without parent traversal",
        ));
    }
    Ok(())
}

struct Directory {
    path: PathBuf,
    #[cfg(windows)]
    guard: fleet_protocol::transport::DataDirectory,
}

impl Directory {
    fn open(path: &Path, create: bool) -> Result<Option<Self>, SshError> {
        validate_path(path)?;
        let parent = path
            .parent()
            .ok_or_else(|| error("catalog needs a parent"))?;
        if !create {
            match std::fs::symlink_metadata(parent) {
                Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(err) => return Err(io_error(err)),
                Ok(_) => {}
            }
        }
        #[cfg(windows)]
        let guard = fleet_protocol::transport::DataDirectory::open(parent).map_err(io_error)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
            if create {
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(parent)
                    .map_err(io_error)?;
            }
            let metadata = std::fs::symlink_metadata(parent).map_err(io_error)?;
            if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
                return Err(error("SSH catalog directory is not private"));
            }
        }
        Ok(Some(Self {
            path: parent.to_path_buf(),
            #[cfg(windows)]
            guard,
        }))
    }

    fn validate_file(&self, path: &Path) -> Result<(), SshError> {
        if path.parent() != Some(self.path.as_path()) {
            return Err(error("SSH catalog file outside private directory"));
        }
        #[cfg(windows)]
        self.guard.validate_private_file(path).map_err(io_error)?;
        #[cfg(unix)]
        match std::fs::symlink_metadata(path) {
            Ok(metadata) => validate_metadata(&metadata)?,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(io_error(err)),
        }
        Ok(())
    }

    fn validate_handle(&self, path: &Path, file: &File) -> Result<(), SshError> {
        self.validate_file(path)?;
        #[cfg(unix)]
        validate_metadata(&file.metadata().map_err(io_error)?)?;
        #[cfg(windows)]
        let _ = file;
        Ok(())
    }
}

#[cfg(unix)]
fn validate_metadata(metadata: &std::fs::Metadata) -> Result<(), SshError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.permissions().mode() & 0o077 != 0 {
        return Err(error("SSH catalog file is not private and regular"));
    }
    Ok(())
}

fn options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.read(true);
    options
}

fn validate_hosts(hosts: &[HostConfig]) -> Result<(), SshError> {
    let mut aliases = std::collections::HashSet::new();
    for host in hosts {
        host.validate()?;
        if !aliases.insert(&host.alias) {
            return Err(error("duplicate SSH host alias"));
        }
    }
    Ok(())
}

fn load_in(directory: &Directory, path: &Path) -> Result<Vec<HostConfig>, SshError> {
    directory.validate_file(path)?;
    let mut file = match options().open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(io_error(err)),
    };
    // Readers and writers hold the stable sibling lock while opening and
    // validating the catalog, including during replacement.
    directory.validate_handle(path, &file)?;
    let mut text = String::new();
    file.read_to_string(&mut text).map_err(io_error)?;
    let hosts: Vec<HostConfig> =
        serde_json::from_str(&text).map_err(|_| error("invalid SSH catalog JSON"))?;
    validate_hosts(&hosts)?;
    Ok(hosts)
}

pub(super) fn load(path: &Path) -> Result<Vec<HostConfig>, SshError> {
    match Directory::open(path, false)? {
        Some(directory) => {
            let _lock = lock_catalog(&directory, path, false)?;
            load_in(&directory, path)
        }
        None => Ok(Vec::new()),
    }
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn write_atomic(directory: &Directory, path: &Path, hosts: &[HostConfig]) -> Result<(), SshError> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let text = serde_json::to_vec_pretty(hosts).map_err(|_| error("cannot encode SSH catalog"))?;
    let (temp, mut file) = loop {
        let temp = sibling(
            path,
            &format!(
                ".tmp-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ),
        );
        match options().write(true).create_new(true).open(&temp) {
            Ok(file) => break (temp, file),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(io_error(err)),
        }
    };
    let written = (|| {
        directory.validate_handle(&temp, &file)?;
        file.write_all(&text).map_err(io_error)?;
        file.sync_all().map_err(io_error)?;
        Ok::<(), SshError>(())
    })();
    drop(file);
    let result = written.and_then(|()| {
        directory.validate_file(path)?;
        std::fs::rename(&temp, path).map_err(io_error)?;
        // Flush the directory entry on Unix. Windows replacement is atomic
        // on supported local filesystems, but power-loss durability is not
        // certified by this API.
        #[cfg(unix)]
        File::open(&directory.path)
            .and_then(|dir| dir.sync_all())
            .map_err(io_error)?;
        Ok(())
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

fn lock_catalog(directory: &Directory, path: &Path, exclusive: bool) -> Result<File, SshError> {
    let lock_path = sibling(path, ".lock");
    directory.validate_file(&lock_path)?;
    let lock = options()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(io_error)?;
    directory.validate_handle(&lock_path, &lock)?;
    if exclusive {
        lock.lock().map_err(io_error)?;
    } else {
        lock.lock_shared().map_err(io_error)?;
    }
    Ok(lock)
}

pub(super) fn update(
    path: &Path,
    change: impl FnOnce(&mut Vec<HostConfig>) -> Result<(), SshError>,
) -> Result<(), SshError> {
    let directory =
        Directory::open(path, true)?.ok_or_else(|| error("catalog directory missing"))?;
    let _lock = lock_catalog(&directory, path, true)?;
    let mut hosts = load_in(&directory, path)?;
    change(&mut hosts)?;
    validate_hosts(&hosts)?;
    write_atomic(&directory, path, &hosts)?;
    // Keep the lock file; deleting it would permit two distinct locks.
    Ok(())
}

pub(super) fn add(path: &Path, host: HostConfig) -> Result<(), SshError> {
    update(path, |hosts| {
        if hosts.iter().any(|existing| existing.alias == host.alias) {
            return Err(error("SSH host alias already exists"));
        }
        hosts.push(host);
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "gf-ssh-catalog-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return path,
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(err) => panic!("cannot create test root: {err}"),
            }
        }
    }

    fn host(alias: String) -> HostConfig {
        HostConfig {
            alias,
            destination: "fixture@example.invalid".into(),
            bridge_path: "/opt/fleet-ssh-bridge".into(),
            port: None,
            identity_file: None,
            expected_host_key: None,
            last_status: None,
            protocol_version: None,
        }
    }

    #[test]
    fn concurrent_updates_preserve_every_host() {
        let root = fixture();
        let path = root.join("data").join("hosts.json");
        // Readers must never see a partially written or inconsistent catalog.
        add(&path, host("initial".into())).unwrap();
        let reader = {
            let path = path.clone();
            std::thread::spawn(move || {
                for _ in 0..24 {
                    assert!(load(&path)
                        .unwrap()
                        .iter()
                        .any(|host| host.alias == "initial"));
                }
            })
        };
        let workers: Vec<_> = (0..12)
            .map(|index| {
                let path = path.clone();
                std::thread::spawn(move || add(&path, host(format!("host-{index}"))).unwrap())
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        reader.join().unwrap();
        assert_eq!(load(&path).unwrap().len(), 13);
        assert!(add(&path, host("host-0".into())).is_err());
        assert_eq!(load(&path).unwrap().len(), 13);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_update_and_corrupt_existing_catalog_are_not_overwritten() {
        let root = fixture();
        let path = root.join("data").join("hosts.json");
        add(&path, host("valid".into())).unwrap();
        let before = std::fs::read(&path).unwrap();
        let mut invalid = host("invalid".into());
        invalid.expected_host_key = Some("unverified".into());
        assert!(add(&path, invalid).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::write(&path, b"broken").unwrap();
        assert!(add(&path, host("new".into())).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"broken");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn hardlinked_catalog_is_not_read_or_replaced() {
        let root = fixture();
        let path = root.join("data").join("hosts.json");
        add(&path, host("first".into())).unwrap();
        let alias = path.with_file_name("alias.json");
        std::fs::hard_link(&path, &alias).unwrap();
        let before = std::fs::read(&alias).unwrap();
        assert!(load(&path).is_err());
        assert!(add(&path, host("other".into())).is_err());
        assert_eq!(std::fs::read(&alias).unwrap(), before);
        std::fs::remove_dir_all(root).unwrap();
    }
}
